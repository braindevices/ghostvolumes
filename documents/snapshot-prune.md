# Pruned Snapper snapshots of your projects

Long Snapper history of your project sources, without the regenerable noise (`node_modules`, `target`, `.venv`, `build`, …). Each hour Snapper takes a **writable** snapshot of your projects subvolume. `ghostvolumes prune` deletes the directories you've decided are volatile, **inside that snapshot only**, and proves the VCS metadata is untouched and healthy. The snapshot is then locked read-only, and Snapper's timeline cleanup keeps it for as long as you configure.

Nothing runs inside your build tools, and your working tree is never touched.

## How it decides what to prune

- **Only decision files in the subvolume.** A `+` line in a `.ghostvolumes-decisions` file (e.g. `+ node_modules`, `+ /build`) marks matching directories volatile, everywhere below that file, up to the subvolume root. Undecided directories are kept: no decision file, no pruning. Nothing outside the subvolume adds a `+`: there's no project list, and `roots.d` watch lists are only suggestions for `discover`/`decide`/`convert`.
- **Rules reach everything below their file.** A `~/src/.ghostvolumes-decisions` with `+ node_modules` acts as your personal default for every repo, and an enclosing repo's unanchored rule also reaches a repo nested in it. The closer file wins, so a nested repo's own `-` keeps its directory. Mind broad rules: `+ build` in `~/src` also prunes a non-VCS directory's real `build/` (only git repos get the tracked/ignored guards).
- **Everything in the subvolume is managed.** It's all snapshotted and pruned by decisions alone; `prune` doesn't read `.ghostvolumes-ignore` (that's `convert`'s). Keep a directory out of history by keeping it out of the subvolume. To keep a path a broader `+` matches, write a closer `-`, e.g. `- /third_party/**/node_modules`. `ghostvolumes decide` records decisions, and `ghostvolumes discover` suggests candidates, including tool-tagged caches (`CACHEDIR.TAG`).
- **Never versioned or user content.** A `+` path is refused, kept and logged when:
  1. it leaves the snapshot or passes through a symlink;
  2. it contains a VCS directory (`.git`, `.hg`, `.svn`, `.jj`, `.dvc`, …), a decision file, or another subvolume or filesystem;
  3. inside a git repo, it holds tracked files (`git ls-files`), **or isn't ignored** by the repo (`git check-ignore`). A committed `+ /notes` can't drop your untracked notes from history;
  4. it can't be deleted, e.g. root-owned files left by a container build.
- **Only real snapshots.** `prune` refuses (exit 2) anything that isn't `<subvolume>/.snapshots/<n>/snapshot` and a subvolume itself, so never the live tree, and refuses the whole run if a real subvolume or another filesystem appears inside (a snapshot shows nested subvolumes only as empty placeholders, which are skipped).

## Setup

**1. A subvolume for your projects** (once, as root). If your projects already live in a plain directory, create the subvolume next to it and move them in (a reflink copy).
```sh
btrfs subvolume create ~/src
```
It's nested inside your home subvolume, so home snapshots skip it entirely.

**2. Snapper config** (once, as root):
```sh
snapper -c src create-config ~/src
```
Then in `/etc/snapper/configs/src`:
```ini
ALLOW_USERS="<you>"
SYNC_ACL="yes"
TIMELINE_CREATE="no"          # only our script snapshots this config (never unpruned)
TIMELINE_CLEANUP="yes"
TIMELINE_MIN_AGE="3600"
TIMELINE_LIMIT_HOURLY="24"
TIMELINE_LIMIT_DAILY="30"
TIMELINE_LIMIT_WEEKLY="12"
TIMELINE_LIMIT_MONTHLY="12"
TIMELINE_LIMIT_QUARTERLY="0"
TIMELINE_LIMIT_YEARLY="3"
NUMBER_CLEANUP="no"
```
Enable Snapper's cleanup: `systemctl enable --now snapper-cleanup.timer`. Requires a recent Snapper (0.12 or newer, the generation with `snbk`).

**3. The script and timer** (as you):
```sh
ghostvolumes contrib ghostvolumes-snapshot > ~/.local/bin/ghostvolumes-snapshot
chmod +x ~/.local/bin/ghostvolumes-snapshot
mkdir -p ~/.config/systemd/user
for f in ghostvolumes-snapshot@.service ghostvolumes-snapshot@.timer ghostvolumes-notify@.service; do
  ghostvolumes contrib "$f" > ~/.config/systemd/user/"$f"
done
systemctl --user edit ghostvolumes-snapshot@src.service    # add: [Service] Environment=SUBVOLUME=%h/src
systemctl --user enable --now ghostvolumes-snapshot@src.timer
```
- Without `loginctl enable-linger`, the timer only runs while you're logged in, and `Persistent=true` catches up at login. With linger it runs from boot.

**4. Decide** what's volatile (no registration needed for pruning):
```sh
ghostvolumes discover ~/src
ghostvolumes decide ~/src/myproj --add node_modules --add /target
```

**5. Optional login check:** add `. ghostvolumes-login-check src` to `~/.profile` (from `ghostvolumes contrib ghostvolumes-login-check`). It warns when the newest snapshot needs attention, the timer has stopped, or `events.log` has entries (see [Decision changes](#decision-changes)).

## What each run does
1. Snapshots left `verify=pending` by a run that died are pruned (with `--since`, like step 4) and locked (`verify=recovered`). If that's impossible they're locked as they are and tagged `verify=failed`. **No snapshot is ever deleted** by this tool; Snapper's cleanup does that.
2. `snapper create --read-write` → snapshot `n`.
3. `ghostvolumes vcs-manifest` records every entry under the VCS dirs. If that fails, the run still prunes (the guards, not the manifest, keep `.git` safe; the manifest only proves it afterwards), skips step 6 and ends `verify=failed`.
4. `ghostvolumes prune --config src --since "${SINCE:-1d}"` deletes the decided directories in `n`, using only the decision files inside `n`. First it reports and logs what is pruned only because the decisions changed (see [Decision changes](#decision-changes)). If prune fails, `n` is still locked and kept, tagged `verify=failed`.
5. `snapper modify --read-only --userdata verify=pending|failed n`: locked together with the tag a crash at this point should leave, so a failed run can't later pass for `recovered`.
6. The manifest must be byte-identical, which proves `.git` (etc.) wasn't touched.
7. `ghostvolumes vcs-health` runs the health command (git: `fsck --connectivity-only`), only for repos whose metadata changed since the last `ok` snapshot. Snapper's own comparison (`snapper status`, a BTRFS metadata-only send stream) says what changed.
8. `snapper modify --userdata verify=<result> n`.

| `verify=` | Meaning | Alerts? |
|---|---|---|
| `ok` | pruned, metadata unchanged, changed repos healthy | no |
| `refused` | some `+` paths kept; see the journal | no (logged) |
| `health-timeout` | a health check ran out of time | no (logged) |
| `rules-changed` | pruned, and decision changes prune something new (also on the very first run); see `events.log`. Also used when something was refused in the same run (the refusals are in the journal) | no (logged) |
| `recovered` | a run that died was finished by the next one | no |
| `manifest-changed` | VCS metadata changed during the prune: a bug, investigate | **yes** |
| `health-failed` | a repo's health check failed: the repo itself is damaged | **yes** |
| `failed` | prune failed (the snapshot is kept, locked, maybe unpruned), the before-manifest couldn't be taken (pruned, health checked, but no proof), or anything unexpected | **yes** |

Alerts come from the unit failing: `ghostvolumes-notify@` writes a critical journal entry and tries `notify-send`.

## Decision changes
Decision files are trusted like `.gitignore`. A `git pull` that adds `+ /notes` is applied, and that directory leaves your snapshots from then on. Every snapshot is pruned by **its own** decision files: a later change never re-prunes an earlier snapshot.

So that such a change doesn't go unnoticed, `prune --config src --since 1d` compares with an earlier snapshot: of the snapshots this tool pruned (`snapper list`: description `pruned`, a `verify=` result, read-only), the **oldest** one taken within the window before this snapshot's own date, or else the newest one before the window. It walks the new snapshot twice: once with its own decision files, once with the earlier snapshot's (read at the same paths, only read). Whatever only the new decisions prune is reported. That covers every way pruning can widen: a new or edited `+`, a removed `-`, reordered lines, a removed nested override, a new repo arriving with decision files. Both walks see the same tree, so new build output never warns. For each such path it:
- prints `will prune (decisions changed): p/notes (37 file(s), newest 2026-10-08)`;
- appends that line, with time and snapshot, to `~/.local/state/ghostvolumes/events.log`, **before** deleting anything, so a crash can't lose it. If the log can't be written, the journal still has the line and pruning continues;
- makes the script tag the snapshot `verify=rules-changed` (failures and `health-timeout` take precedence over the tag, not over the log).

**The window repeats the warning:** with the default `SINCE=1d`, a change is reported by every hourly run until it's a day old (then the baseline already has it). Snapper's retention caps the window: keep `TIMELINE_LIMIT_HOURLY` ≥ 24 for `1d`. Set another window per timer with `systemctl --user edit ghostvolumes-snapshot@src` → `Environment=SINCE=6h` (a duration, or a UTC time `YYYY-MM-DD[ HH:MM[:SS]]`). The first run reports everything it prunes. A moved or renamed project is reported again (its paths are new). If no baseline can be determined (snapper fails, `--config` belongs to another subvolume, or the chosen snapshot isn't a real read-only snapshot), every pruned path is reported in the journal and the log gets one summary line per run, `baseline unavailable (…): N path(s) pruned unchecked`, until it's fixed.

**Who can suppress a warning:** anyone allowed on the Snapper config (`ALLOW_USERS`/`ALLOW_GROUPS`, and root) can create a snapshot that looks like a baseline. Keep `ALLOW_USERS` to yourself.

To review by hand: `ghostvolumes prune --dry-run --config src --since 7d ~/src/.snapshots/N/snapshot` prints what changed within a week, logging nothing.

The login check reports `events.log` while it's non-empty. Read it, then delete it; nothing else reads it.

## Daily use
- `snapper -c src list`, `snapper -c src status N..M`, `snapper -c src diff N..M FILE`, `snapper -c src undochange N..M FILE` all work as usual.
- **Restore** a file: copy it from `~/src/.snapshots/N/snapshot/...`. To restore a whole project: `cp -a --reflink=always ~/src/.snapshots/N/snapshot/proj ~/src/proj.restored`, then rebuild what was pruned.
- **Keep one forever** (a milestone or an investigation): `snapper -c src modify -c '' N` (no cleanup algorithm).
- **Never** `snapper modify --read-write` a `verify=` snapshot. `snbk` (if you add a backup target later) requires transferred snapshots never to change.

## Integrity, beyond each snapshot
- Weekly, as a user timer: `git fsck` (full) in each live repo. It catches content corruption while older snapshots still hold good copies.
- Monthly, as root: `btrfs scrub start -B /`, which catches bit-rot in every file.

## Per-VCS settings (`~/.config/ghostvolumes/vcs.toml`)
Defaults cover git, hg, svn, jj and dvc. Only git ships commands. An entry replaces the default of the same name:
```toml
[vcs.git]
dir    = ".git"
guard  = ["git", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "safe.bareRepository=explicit",
          "--literal-pathspecs", "ls-files", "-z", "--"]     # any output → refused
allow  = ["git", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "safe.bareRepository=explicit",
          "check-ignore", "-q", "--"]                        # must exit 0
health = ["git", "-c", "core.fsmonitor=false", "-c", "core.hooksPath=/dev/null", "-c", "safe.bareRepository=explicit",
          "fsck", "--connectivity-only", "--no-dangling"]
```
Before adding commands for other VCSs, make sure they **write nothing** into their metadata and work on a read-only snapshot. `hg verify` may take a lock, and any `jj` command needs `--ignore-working-copy`. A command that writes causes a `manifest-changed` alert or fails read-only.

## Unsupported (skipped with a message)
- Repos with git alternates (`clone --shared`/`--reference`).
- Linked worktrees (their `.git` file points into the live repo).
- A VCS dir that is a symlink.
- Expect a refusal on every run for a `.venv` containing editable installs from git (`pip install -e git+…` puts a `.git` inside). That's safe, just noisy; decide `- .venv` or remove the editable install.

## Testing (contributors)
`tests/snapshot_script.rs` runs the real timer script end to end on dummy subvolumes under the BTRFS test scratch dir, against `tests/support/fake-snapper` by default. To run it against a **real** snapper 0.13 in an EL9 container (no snapperd, no `CAP_SYS_ADMIN`, nothing written to `/etc`):
```sh
scripts/dev-snapper.sh                                  # once per container: builds the EPEL 10 SRPM for EL9, cached in target/dev-snapper
GHOSTVOLUMES_DEV_SNAPPER=$PWD/target/dev-snapper cargo test --test snapshot_script
```
The window test needs back-dated snapshots and only runs with the fake. CI (`scripts/ci-snapper-e2e.sh`) additionally runs real snapper with snapperd on Ubuntu.
