# Snapshot-prune — plan

**Status**: draft for owner review (2026-10-08). No code until approved.
**Branch**: `claude-snapshot-prune` (from `claude-shim-edition-2024`).
**Replaces**: the LD_PRELOAD shim. `convert` stays for now (owner ruling R1).

## 1. Goal

Long-retention Snapper history of project sources, **without volatile noise** (`node_modules`, `target`, `.venv`, `build`, …), on one BTRFS filesystem, with as few moving parts as possible.

Snapper takes each snapshot of the projects subvolume **read-write**. ghostvolumes deletes the explicitly decided volatile directories **inside that snapshot**, proves the VCS metadata was untouched and is healthy, and the snapshot is locked read-only. Snapper's timeline cleanup does retention. No data is copied, and no code runs inside other processes.

## 2. Constraints (from the owner)

| # | Constraint |
|---|---|
| C1 | Artifact dirs stay where each tool puts them, relative to the repo. No relocation (`CARGO_TARGET_DIR`, `build-dir`, `UV_PROJECT_ENVIRONMENT`, …) |
| C2 | No change to project build tools; not every invocation can be wrapped (IDEs, Makefiles, nested tools) |
| C3 | Works with any VCS. Explicit, recorded, committable decisions; an undecided directory is **kept** |
| C4 | Some projects are local-only (no remote, maybe not git); no "push often" |
| C5 | One BTRFS filesystem; backups stay on it. No `btrfs send`; no repository tools (borg/restic/kopia); no `cp`-based sync |
| C6 | Projects may use git, hg, DVC, jj, … — the design must be VCS-agnostic, with per-VCS behaviour only as config |

**Owner rulings (2026-10-08)**
- **R1:** keep `convert`; retire only the shim.
- **R2:** the timer is a transparent shell script calling `snapper` plus small ghostvolumes subcommands.
- **R3:** support only recent Snapper (the generation with stable `snbk`). Snapper can't be exercised in this sandbox; CI's snapper job covers it.
- **R4:** the std-only limit is gone with the shim, so crates are fine where they help.
- **R5:** keep the `.git` check simple. `--shared`/alternates repos are unsupported; a frozen CoW snapshot can't be affected by the live repo.
- **R6:** avoid git-specific machinery.

## 3. Decisions and how they were reached (2026-10-07/08 debates)

Two expert agents (senior software engineer, senior sysops engineer) debated each round; the owner ruled on constraints.

| Question | Outcome | Evidence |
|---|---|---|
| Keep the LD_PRELOAD shim? | **Retire** | Runs inside every host process; misses static binaries, IDEs and anything outside `intercept`; most of the audit and maintenance cost |
| Relocate artifacts per tool | Rejected (C1/C2) | — |
| inotify daemon that recreates directories | Viable, but superseded | It loses same-process recreation (`uv venv --clear` lost 3/3) |
| Sync to a second subvolume with reflink | **No mature tool** | Upstream rsync has no reflink option (checked up to 3.5.1; `--clone-dest` is an unmaintained out-of-tree patch). rclone clones only on macOS APFS. rsync + duperemove/bees: bees warns dedup over snapshots can *increase* usage |
| **Snapshot, then prune** | **Chosen** | Tested on BTRFS: deleting inside a fresh writable snapshot releases the volatile extents (the live `target/big.o` became unshared; source stayed shared); the snapshot then refused writes. Snapper ≥ 0.13 documents `create --read-write`, `--print-number`, `--cleanup-algorithm`, `--userdata`, `modify --read-only` |
| Prune via a Snapper plugin? | **No** | `snapper(8)`: "Using snapper in the plugins is not allowed", plugins must finish "within a few seconds", and whether they run in the client or `snapperd` is undefined. The tool calls Snapper, not the reverse |
| Prune any `CACHEDIR.TAG` dir? | **No, decided names only (option b)** | Tag coverage is partial: Cargo `target` and `uv venv` have one; `python -m venv`, `node_modules` and most `build/` don't. Anyone can commit one into a source directory. Tags are only *suggestions* in `discover` |
| How to protect `.git` (and other VCS metadata)? | **VCS-agnostic core** (final debate round, both agreed). Guards refuse any `+` path that contains a VCS metadata dir. A before/after manifest of the metadata dirs proves prune left them untouched; it catches what fsck misses, such as a deleted `refs/heads`. An optional per-VCS health command from config (git: `fsck --connectivity-only`) runs on the read-only snapshot, only for repos whose metadata changed since the last `ok` snapshot. Dropped: the git deletion-baseline diff (redundant with the tracked guard), content skip keys, the cache file. Alternates and linked worktrees are unsupported | Earlier rounds: `fsck --connectivity-only` exits 0 after tracked files or `.git/refs/heads` are deleted; a `clone --shared` repo's own manifest misses changes in its alternate |

## 4. Design

### 4.1 Layout and Snapper (documented setup, not code)
- Projects live in their own subvolume, e.g. `~/src`. It's nested in `@home`, so `@home` snapshots skip it.
- `snapper -c src create-config ~/src` (root, once) creates `.snapshots` as a nested subvolume.
- `/etc/snapper/configs/src`:
  - `TIMELINE_CREATE=no`: only our script creates snapshots, so no unpruned ones appear.
  - `TIMELINE_CLEANUP=yes` with `TIMELINE_LIMIT_*` (e.g. hourly 24, daily 30, weekly 12, monthly 12, yearly 3) and `TIMELINE_MIN_AGE=3600`.
  - `NUMBER_CLEANUP=no`; `ALLOW_USERS=<user>`, `SYNC_ACL=yes` (the script runs as the user).
  - No `SPACE_LIMIT` (it needs quota).
- The distribution's `snapper-cleanup.timer` must be enabled.
- Permanent milestones: `snapper -c src create -d …` with no cleanup algorithm; they are never cleaned up (and not pruned; take them on a clean tree).
- **`snbk` compatibility** (for a future second location; not used now):
  - snbk transfers only read-only snapshots, so ones still writable mid-prune are skipped.
  - It requires transferred snapshots never to change, so **never `snapper modify --read-write` a `verify=` snapshot**. Documented.

### 4.2 Config: `[vcs.*]` table (new, TOML in the existing config dir)
```toml
[vcs.git]
dir    = ".git"            # a file (submodule) is treated as metadata too
guard  = ["git", "-c", "core.fsmonitor=false", "ls-files", "-z", "--"]   # any output → path refused
health = ["git", "-c", "core.fsmonitor=false", "fsck", "--connectivity-only", "--no-dangling"]

[vcs.hg]
dir = ".hg"
[vcs.svn]
dir = ".svn"
[vcs.jj]
dir = ".jj"
[vcs.dvc]
dir = ".dvc"               # its cache lives inside the metadata dir, so it is never pruned
```
- **Defaults:** `dir` for git, hg, svn, jj and dvc (the VCS-dir guard and the manifest need only that). `guard`/`health` commands only for **git**, the one verified to leave the metadata untouched (`GIT_OPTIONAL_LOCKS=0`) and to work on a read-only snapshot.
- **Other VCSs' commands are user-added**, documented with caveats:
  - `hg files`/`hg verify` may write `.hg/cache` or take the repo lock, which would cause a false `manifest-changed` or fail on a read-only snapshot.
  - Any jj command needs `--ignore-working-copy`, or it snapshots the working copy into `.jj`.
  - A shipped default is added only once a test proves it leaves the manifest unchanged and works read-only.
- Commands run with the repo root as cwd, as the user, with `GIT_OPTIONAL_LOCKS=0` in the environment and a timeout.
- **Unsupported (documented; skipped with a warning):**
  - repos with alternates (`objects/info/alternates`, i.e. `clone --shared`/`--reference`);
  - linked worktrees (their `.git` file points into the live repo);
  - a VCS dir that is a symlink.

### 4.3 The timer script (transparent shell, ~35 lines; shipped as `contrib/src-snapshot` and printed by `ghostvolumes snapshot-script`)
```sh
#!/bin/sh
# hourly, as the owning user (systemd user timer)
set -eu
PATH=$HOME/.cargo/bin:$HOME/.local/bin:/usr/local/bin:/usr/bin:/bin
C=src; S=$HOME/src/.snapshots
L=${XDG_STATE_HOME:-$HOME/.local/state}/ghostvolumes; mkdir -p "$L"
exec 9>"$L/$C.lock"; flock -n 9 || exit 0            # one run at a time (timer vs manual)
M=$(mktemp); trap 'rm -f "$M" "$M.chg"' EXIT
ls_() { snapper --csvout -c $C list --columns number,userdata; }

# Leftovers of a run that died: still writable (snbk never transfers those).
# Prune them now and lock; if that fails, drop them rather than keep noise for months.
for p in $(ls_ | awk -F, '$2=="verify=pending"{print $1}'); do
  r=0; ghostvolumes prune "$S/$p/snapshot" || r=$?
  if [ $r -le 1 ]; then snapper -c $C modify --read-only --userdata verify=recovered "$p"
  else snapper -c $C delete "$p"; echo "dropped unprunable snapshot $p" >&2; exit 1; fi
done

prev=$(ls_ | awk -F, '$2=="verify=ok"{p=$1} END{print p}')
n=$(snapper -c $C create --read-write --cleanup-algorithm timeline \
     --description pruned --userdata verify=pending --print-number)
d=$S/$n/snapshot
ghostvolumes vcs-manifest "$d" >"$M"
rc=0; ghostvolumes prune "$d" || rc=$?            # 1 = some paths refused (logged)
if [ $rc -gt 1 ]; then snapper -c $C delete "$n"; exit 1; fi   # never keep a half-pruned one
snapper -c $C modify --read-only "$n"
ghostvolumes vcs-manifest "$d" | cmp -s - "$M" || rc=9
if [ $rc -le 1 ]; then
  chg=""                                          # snapper's own comparison; on any failure
  if [ -n "$prev" ] && snapper -c $C status -o "$M.chg" "$prev..$n"; then chg="--changes $M.chg"; fi
  ghostvolumes vcs-health "$d" $chg || rc=$((10+$?))   # no --changes → every repo checked
fi                                                # health exit: 1 = failed, 2 = timeout
case $rc in 0) t=ok;; 1) t=refused;; 9) t=manifest-changed;;
  11) t=health-failed;; 12) t=health-timeout;; *) t=failed;; esac
snapper -c $C modify --userdata verify=$t "$n"
case $t in ok|refused|health-timeout) exit 0;; *) exit 1;; esac
```
- Userdata is the single key `verify` (`pending`, `ok`, `refused`, `recovered`, `manifest-changed`, `health-failed`, `health-timeout`, `failed`), so the CSV parsing stays trivial.
- **Crash handling:**
  - A run that dies leaves its snapshot writable and `pending`. The next run prunes it and locks it (`recovered`), or deletes it if that's impossible.
  - A prune that crashes outright (exit > 1) deletes its own snapshot, so no unpruned snapshot is ever locked into long retention.
  - It isn't locked from a `trap`, because a locked unpruned snapshot couldn't be pruned later.
- **Keeping one:** snapshots tagged `failed` age out under timeline cleanup like any other. To keep one for investigation or as a milestone: `snapper -c src modify -c '' N`.

### 4.4 New subcommands (Rust; reuse the decision parser and audited path checks)
- **`ghostvolumes prune <snap>`**
  - Maps registered projects (`project-roots.list`) under the config's subvolume into `<snap>`, then walks each one. It reads **only** decision files, `.ghostvolumes-ignore`, `project-roots.list` and the `[vcs.*]` config, never `roots.d`/`compiled.tsv`.
  - Not descended into: VCS dirs, ignored names, nested subvolumes.
  - A directory whose decision, read **from the snapshot's** decision files, resolves to `+` is deleted, unless a guard refuses it:
    1. containment: strictly inside the project by plain components, no symlinked component, a real directory (`check_contained`);
    2. it is or contains a configured VCS dir (dir or file);
    3. it contains a registered project root or a nested decision file;
    4. the VCS's optional `guard` command prints anything for that path (git: it holds tracked files).
  - **Permission errors** (e.g. root-owned files left by Docker or `sudo` builds, which the user can't delete) count as **refused**: the path is logged, and the run continues with exit 1. They never crash it or page.
  - `remove_dir_all` doesn't follow symlinks; every pruned and refused path is logged, with its size.
  - **Idempotent:** running it again on a partly pruned snapshot finishes the job (used by the recovery loop).
  - Exit codes: 0 = clean, 1 = something refused, > 1 = an internal error.
- **`ghostvolumes vcs-manifest <snap>`**
  - Prints a deterministic, sorted manifest of every configured VCS dir found in `<snap>`: path, type, size, mtime, inode, one line per entry.
  - No VCS knowledge. The script compares the output from before and after the prune with `cmp`.
- **`ghostvolumes vcs-health <snap> [--changes <snapper-status-file>]`**
  - For each repo with a configured `health` command, runs it in the snapshot (it's read-only by now). Per-repo timeout (config, default 5 min).
  - With `--changes`, a repo is checked only if a changed path lies under its VCS dir. Without it, every repo is checked.
  - Exit codes: 0 = ok, 1 = a health command failed (names the repo), 2 = only timeouts.
- **`discover`:**
  - additionally lists `CACHEDIR.TAG` directories as suggestions (never auto-pruned);
  - reports watched-name directories inside **unregistered** projects under a configured snapshot subvolume, because those are never pruned.

**Change detection is Snapper's own comparison** (verified in snapper's source, `snapper/Btrfs.cc`, `Comparison.cc`, `client/snapper/cmd-status.cc`, master, 2026-10):
- On BTRFS, `Btrfs::cmpDirs` compares two snapshots with a **metadata-only send stream** (`BTRFS_SEND_FLAG_NO_FILE_DATA`). The cost depends on what changed, not on tree size.
  - It's enabled by default (`special_cmp = true`) and falls back to a directory walk on error.
  - `snapperd` (root) does it for the user over D-Bus.
- When both snapshots are read-only, Snapper **saves** the result in the newer snapshot's info dir and reuses it. No state of ours.
- **Read from `snapper status -o`, parsed fail-safe** (the D-Bus/`zbus` variant was dropped in the review: an async crate stack, only testable in CI):
  - The format is `"<status> <absolute path>\n"`, with no escaping and no JSON. We only need a yes/no per repo: did anything change under its VCS dir?
  - A line that doesn't match `^[+\-ctpugxa.]+ /` means **check every repo**.
  - A newline inside some filename can only split that one entry into extra lines. It can't remove or alter the real line for a changed path under `.git/`. So the worst case is an extra check, never a missed one.
  - If `snapper status` fails, no `--changes` is passed, and every repo is checked.
- Both snapshots are pruned, so the comparison holds only real source and VCS changes.
- The **prune-integrity** check (before vs after the prune) stays `vcs-manifest` + `cmp`. Snapper compares two snapshots, not one snapshot before and after an edit.

### 4.5 Alerts
| Pages (script exits 1 → `OnFailure=`) | Logged only (exit 0; visible as `verify=` in `snapper list` and in the journal) |
|---|---|
| `manifest-changed`, `health-failed`, `failed`, a dropped unprunable snapshot, any unexpected failure | `refused` (incl. permission errors), `health-timeout`, `recovered` |

- **The `OnFailure` unit** runs `notify-send` **and** `systemd-cat -p crit`. `notify-send` silently fails without a graphical session (e.g. with linger, or a desktop that didn't import its environment); the journal entry always survives.
- **Login check** (documented shell snippet, e.g. for `~/.profile`): warn if the newest snapshot's `verify=` isn't `ok`/`refused`/`health-timeout`/`recovered`, or if it's older than 2 h (the timer stopped).
- **Linger:** without it, the timer doesn't run while logged out, and `Persistent=true` catches up at login. With `loginctl enable-linger` it runs from boot.

### 4.6 Separate integrity jobs (documented, optional)
- **Weekly:** `git fsck` (full) per registered git repo, on the **live** tree, via a user timer with `Nice=19`, `IOSchedulingClass=idle`, `Persistent=true`. It catches content corruption while older snapshots still hold good copies.
- **Monthly:** `btrfs scrub start -B /` (root), which catches bit-rot for every VCS and non-VCS file.

### 4.7 systemd user units (shipped in `contrib/`, documented)
```ini
# src-snapshot.service
[Unit]
OnFailure=notify-failure@%n.service
[Service]
Type=oneshot
ExecStart=%h/bin/src-snapshot
Nice=19
IOSchedulingClass=idle
TimeoutStartSec=30min

# src-snapshot.timer
[Timer]
OnCalendar=hourly
Persistent=true
[Install]
WantedBy=timers.target

# notify-failure@.service
[Service]
Type=oneshot
ExecStart=notify-send -u critical "%i failed" "journalctl --user -u %i"
```

### 4.8 What stays, what goes
| Stays | Goes |
|---|---|
| `convert`, `decide`, `discover`, `projects`, decision files + parser, `roots.d`/`reload`/`compiled.tsv`, path-safety code, `.ghostvolumes-ignore` | The LD_PRELOAD shim (`shim/preload.rs`, the cdylib build in `build.rs`, `init`'s shim install), `intercept`, `shell-init`, `preload_guard`, the shim tests and docs (`intercept.md`; the shim parts of `design.md` and `security.md`) |

`shim/*_core.rs` files that the CLI still `include!`s (decision, cache, project roots, filenames, btrfs, xdg, lock) move to `src/`. They no longer need to compile standalone.

## 5. Steps (each: implement → test → commit `Step N: …` → progress update; fmt + clippy)

| # | Step | Tests (real BTRFS under `/root` via `btrfs_scratch_dir()`) |
|---|---|---|
| 1 | Test helper: BTRFS **snapshot** via ioctl (`BTRFS_IOC_SNAP_CREATE_V2`) and a read-only flag via `BTRFS_IOC_SUBVOL_SETFLAGS` in `test_support`. Prune and the checks are tested on real snapshots; Snapper itself only in CI | The helper creates a writable snapshot sharing data; read-only refuses writes |
| 2 | `[vcs.*]` config: parse and merge, with shipped defaults | Defaults present; a user override and an addition merge; a bad entry gives a clear error |
| 3 | `vcs-manifest` | Deterministic output; changes when a file under `.git` changes, is deleted, or a ref is renamed; ignores everything outside VCS dirs; a `.git` file is included |
| 4 | `prune` walk + decisions read from the snapshot (`--dry-run` first) | `+ node_modules` at depth; anchored `+ /build`; `-` override; undecided kept; VCS and ignored dirs not descended; the live tree's decision files are not used; **`roots.d`/`compiled.tsv` are never read** (works with them absent or garbage) |
| 5 | `prune` guards 1–4 + deletion + logging + idempotence | `..`, symlinked component, file target refused; a path containing `.git`/`.hg` refused; a nested project refused; the git `guard` refuses a path holding tracked files; **an unremovable (permission-denied) subtree → refused, exit 1, no crash**; a deletion stays in the snapshot; the live tree is untouched; **a second run on a partly pruned snapshot completes it**; **each shipped `guard`/`health` command leaves `vcs-manifest` unchanged** |
| 6 | `vcs-health` with `--changes` (fail-safe parser of `snapper status -o`) and timeouts | Parser: `c...... /src/p/.git/refs/heads/main` → p checked; only work-tree changes → skipped; an unparseable line → every repo checked; a filename with an embedded newline (split lines) → extra checks only, never a missed one; no `--changes` → all checked. A healthy repo → 0; a missing object → 1, naming it; timeout → 2. An alternates repo or linked worktree → skipped with a warning. Nothing written into the read-only snapshot. Real `snapper status` output is exercised in CI (Step 8) |
| 7 | `discover` lists `CACHEDIR.TAG` dirs as suggestions | A tagged dir appears as a suggestion; nothing is acted on |
| 8 | `contrib/` script, units, login check, `snapshot-script` printer, and docs: `documents/snapshot-prune.md` (setup, Snapper config, units, linger, restore, the snbk note, keeping a snapshot, weekly fsck, scrub, unsupported cases, hg/jj caveats), README, `security.md` | `shellcheck` on the script. **CI:** Ubuntu ships snapper 0.10.6, which is below R3's snbk generation (snbk arrived in 0.12.0; `GetFilesByPipe` in 0.10.0; `modify` read-only in 0.10.5, per `snapper.changes`). So the job installs snapper 0.13.x from the **openSUSE Build Service repo for Ubuntu** (key-verified; owner suggestion, replacing a first-draft Tumbleweed container) on the runner itself, with its real systemd/D-Bus, a loop BTRFS, and asserts `snapper --version` ≥ 0.12. End to end: a prune run, a no-change run (health skipped), a run with a `.git` change (health run), and a **crash-recovery run** (kill the script mid-prune, run again, assert the leftover snapshot ends up pruned and `recovered`) |
| 9 | Retire the shim (R1: `convert` stays) and move `shim/*_core.rs` into `src/` | Suite and CI green; `cargo install` no longer builds a cdylib |
| 10 | Final QA + security review by the two agents, one debate round, fixes, report | — |
| 11 | **SF-2: warn on `+` rule changes** (§8). `prune --since <prev snapshot>`; script, login check, docs | See §8 |
| 11c | Prune without project roots (§8.2): the whole managed subvolume, decisions resolve up to the subvolume root | See §8.2 |
| 11b | Step 11 review fixes (§8.1): result comparison, log before deleting, never delete snapshots, registration log, prune drops `.ghostvolumes-ignore` | See §8.1 |

Release possible after Step 8 (the new path works; the shim still exists), then Step 9.

## 6. Migration for existing users
- Decision files stay valid. `+` now also means "prune from history". `convert` still makes live subvolumes for anyone who wants both.
- Existing per-directory subvolumes are empty stubs in snapshots, so they need nothing.
- `init` stops installing the shim. A leftover shim file in the data dir is harmless, and the docs say how to remove it.

## 7. Decided: `compiled.tsv` stays unchanged in this plan
Both experts reviewed this, and both said keep it unchanged here (option a):
- Its line format existed only for the dependency-free shim.
- Changing it here would mix migration churn with retiring the shim (Step 9 already touches every module that reads it), for no operational gain.
- `prune` doesn't read it (Step 4 asserts that).

**A later, separate plan (option b):**
- The CLI reads the merged `roots.d` TOML directly (`merge::load_all`; microseconds per run).
- `cache.rs`/`cache_core.rs` and the TSV-specific `representable_path` warnings go; decision files keep the check.
- `reload` becomes a validation command (`roots check`, the BTRFS check), and `roots scan --save` just writes TOML.
- On first run, a leftover `compiled.tsv` (lstat'd, regular, owned by the user) is deleted and logged once.

Option (c), a JSON/TOML cache, was rejected by both: it keeps a cache and changes the format, paying both costs.

## 8. SF-2: warn on decision changes, prune anyway (owner ruling 2026-10-08)

**Ruling:** "Warn about decision change that's it." There's no conflict check, no approval state, no holding back, and no comparison with the watch list (`default-watches` is only a registration convenience). Committed decision files stay authoritative, trusted like `.gitignore`.

**Debate (QA/SWE + Security, two rounds), where both converged:**
- **Trigger:** only the effective `+` set, as normalized `(decision-file dir, pattern)` pairs, gaining an entry. Removed `+` lines, `-`/`?` lines, comments and changes in the matched directories don't warn (matches change with every build).
- **Baseline: the previous snapshot**, frozen on disk. No new state file, because a state file can drift out of step (a run updates it, then its snapshot is deleted, and the warning is gone).
  - It is the newest earlier snapshot this tool tagged (any `verify=` value except `pending`).
  - With no previous snapshot, when cleanup removed it, or for a project that's absent from it (newly registered), the project's **whole `+` set is reported once**.
  - A crash-recovered snapshot works as a baseline.
  - Conditions (Security): `--since` must belong to the same subvolume (`subvolume_of`). Its decision files are read with the same `read_regular_file` / `valid_pattern` rules as the current ones.
- **Output:** for each added line, `new prune rule: p/.ghostvolumes-decisions: + /notes (matches N files, newest YYYY-MM-DD)`.
- **Tag:** a single key, `verify=rules-changed`; a second userdata key would break the CSV/awk parsing.
  - Precedence: `failed`, `manifest-changed` and `health-failed` override `rules-changed`, which overrides `refused`/`health-timeout`/`ok`.
  - It isn't a failure: exit 0, no `OnFailure`.
  - The script's `prev` lookup (for `snapper status`) accepts it like `ok`, so each change warns exactly once.
- **Event log (owner ruling 2026-10-08: "keep some special log file for those potentially critical events for user to read, no complex mechanism"):**
  - A change is seen only once, in the first snapshot that has it. Its tag lives only as long as that snapshot, which timeline cleanup may delete within hours.
  - So `prune --since` also **appends** each new rule to `$XDG_STATE_HOME/ghostvolumes/events.log` (default `~/.local/state/ghostvolumes/`, the dir the script already uses for its lock). Each line is `<RFC 3339 time>\t<snapshot>\t<the warning line>`.
  - It's plain text, append-only, and never read back by the tool. The user reads it and deletes or truncates it when done; that's the whole "acknowledge".
  - The login check prints `ghostvolumes: N event(s) in <path>` while the file is non-empty. That's one `[ -s ]` test plus `wc -l`, with no window and no marker.

**Rules are per snapshot (owner ruling 2026-10-08):**
- Each snapshot is pruned only by the decision files frozen inside it. The live tree's files are never used; this is already the case, see the test `decisions_come_from_the_snapshot_not_the_live_tree`.
- A later rule change never touches an earlier snapshot. Nothing re-prunes, and locked snapshots are refused anyway (`ReadOnly`, exit 3).
- `--since` only **reads** the previous snapshot's decision files to compare them. The previous snapshot is never pruned, modified or retagged.
- The only exception is crash recovery of a still-writable `pending` snapshot. It's the same snapshot's first, unfinished prune, using its own frozen rules.

**Interface:**
- `prune --since <snapshot>`: exit **4** = pruned with new `+` rules, refusals (if any) logged. Exit codes 0–3 are unchanged.
- The script maps 4 to `rules-changed` after health, which can still override it.
- Without `--since`, prune behaves as today (no warnings), so manual use is unaffected.

**security.md residual-risk text** (Security's draft, corrected by QA/SWE):
> Committed decision files are trusted like `.gitignore`. A repo that adds `+ /notes` stops that directory's **whole content** from appearing in snapshots from then on, including untracked files you keep there. Earlier snapshots still hold what was there before, until retention expires. In git repos the `allow` command (`check-ignore`) also requires the repo to ignore the path, and the `guard` refuses tracked content. Other VCSs, and plain directories, have no such checks by default. The live tree is never touched. When the `+` rules gain an entry, or on a project's first prune, the run lists the new rules, tags the snapshot `verify=rules-changed` and appends them to `~/.local/state/ghostvolumes/events.log`, which the login check reports until you clear it, but pruning goes ahead. Keep personal work out of directories a project marks `+`.

**Tests:**
- An added rule warns, and so does an edited one.
- A `-`-only change, a removed `+` line and a comment change are silent.
- A new project, a missing `--since` dir, and a `--since` from another subvolume (→ exit 2) behave as described above.
- The baseline decision file is a symlink → treated like the current reader treats it.
- `--since` leaves the baseline snapshot byte-identical: its manifest and the pruned tree are unchanged, and no tag is changed.
- New rules are appended to `events.log`: the directory is created if missing, and the file is never truncated by the tool.
- End to end (fake snapper): commit `+ /notes` → `verify=rules-changed` and an `events.log` line; the next run → `ok` with no new line; the login check reports the file until it is emptied.

Size estimate: about 70 lines of Rust, about 6 lines of script and login check, about 150 lines of tests.

### 8.1 Step 11b — review fixes and owner rulings (2026-10-08)
Both reviewers found problems, two of them reproduced by QA:
- a crash between snapshot creation and the end of prune lost the warning for good (QS-1);
- a failed write to events.log deleted the pruned snapshot every hour (QS-2);
- pruning can widen without a new `+` line: a removed `-`, reordered lines, a removed nested override, a removed ignore line (S1/S6);
- registering a project that already existed in the previous snapshot reported nothing (S2).

**Owner rulings:**
- "compile the rules like in a real prune dry run, then compare";
- "we should never delete snapshots";
- `.ghostvolumes-ignore`: "Let the convert respect it, not prune". Everything in the managed subvolume is meant to be snapshotted and pruned, and a directory that shouldn't be managed doesn't belong in the subvolume.

**Design:**
1. **Comparison of results.** Walk this snapshot's tree twice:
   - B: resolved with this snapshot's decision files.
   - A: resolved with the previous snapshot's decision files at the same relative paths.
   - Warn for every path in B that is neither in A nor inside a path in A.

   This covers every widening (`+`, `-`, order, nested overrides) and adds no build noise, since both runs see the same tree. Baseline files are read only.
   - A baseline file with a symlinked or non-directory ancestor in the baseline, or that is missing, counts as absent: more warnings, never fewer.
   - No baseline (`--since ""`, or one that has gone): A is empty.
2. **Log before deleting.** Warnings (`will prune (decisions changed): <path> (N files, newest D)`) are appended to events.log before anything is deleted. A failed append is reported on stderr and pruning continues; it is never an error exit.
3. **Crash recovery** passes `--since` with the newest usable snapshot older than the pending one and treats exit 4 as success.
   - Baselines are snapshots tagged ok, refused, rules-changed, recovered, health-* or manifest-changed. `failed` and `pending` are never used.
4. **Never delete snapshots.** A prune error locks the snapshot read-only and tags it `verify=failed`, and the run exits 1 (alert). This covers both the main run and recovery. It may keep unpruned data until timeline cleanup.
5. **Registration log.** When `projects register`, `decide` or `convert` adds a project to the list, the CLI appends `registered <project>: will prune <path> (…)` for each current `+` path in the live tree. If there are none, it appends `registered <project>: no + paths yet`. Library code stays free of XDG side effects.
6. **`prune` no longer reads `.ghostvolumes-ignore`** (`convert`/`decide` still do). To keep a vendored path, use `-` (e.g. `- /third_party/**/node_modules`). This is a breaking change, noted in CHANGELOG/docs. `decision::plus_patterns` and the rule-set code from Step 11 are removed.
7. **Docs:**
   - snapshot-prune.md: the comparison, the never-delete rule, and the managed-subvolume rule;
   - security.md;
   - files.md;
   - decision-files.md (`-` for vendored paths);
   - README;
   - CHANGELOG.

   Also: renaming a project re-reports its paths; when rules changed and something was refused, the tag is `rules-changed` and the refusals are in the journal.

**Tests:**
- A removed `-`, reordered lines and a removed nested override each warn; a new build dir doesn't.
- A symlinked baseline ancestor counts as absent.
- The log is written before the deletions.
- An `events.log` that is a directory → pruning still exits 4, and the snapshot is kept.
- e2e:
  - crash recovery across a rule change warns (QS-1 repro);
  - a prune error keeps the snapshot read-only and `failed`;
  - `.ghostvolumes-ignore` is not honoured by prune.
- CLI: registration logs.

### 8.2 Step 11c — prune without project roots (owner ruling 2026-10-08)
**Owner:**
- "prune doesn't have project root concept, it only has managed subvols in theory";
- chose (a), decisions resolve up to the subvolume root;
- "Prune and convert don't need to agree. In a prune volume convert isn't required at all; if someone does it, it's user error. Outside a prune volume, prune doesn't work at all."

**Debate (QA/SWE + Security, two rounds), where both converged:**
- **Accept (a) with no behavioural adjustments.**
  - A cloned repo still can't affect anything outside its own subtree: patterns are relative to their own file, there's no `..`, symlinks aren't followed, and containment is checked against the snapshot root.
  - Rules in `~/src` or `~/src/group/` are the user's and are trusted.
- **Rejected adjustments:**
  - Stopping an enclosing repo's rules at a nested repo's root (Security): it re-introduces VCS detection, which the owner rejected in (b). Documented instead.
  - Suppressing "moved" projects by matching decision-file content (QA): exploitable, since a copied decision file would make a new tree's `notes/` prune silently, and it isn't simple. Rejected by both; the move noise is accepted.
  - Security's grouping of warnings per decision file: dropped. The dry-run comparison reports paths, not the lines behind them, so per-path lines stay.

**Changes:**
- **`prune.rs`:**
  - remove `projects_in` and the "contains a registered project" guard;
  - the walk starts at the snapshot root, and `decision::resolve` is bounded by the snapshot root;
  - containment is checked against the snapshot;
  - the guards that remain: VCS dir or decision file inside, symlinks, git guard/allow, delete-time canonical check;
  - the snapshot is walked once per run (survey reused by the report and the prune), plus the baseline walk.
- **`main.rs`:**
  - prune no longer reads `project-roots.list`;
  - "nothing registered" (exit 2) is gone, and no decision file anywhere → exit 0 with a note;
  - `--since` must be under the same `.snapshots` dir;
  - `--subvolume`/`subvolume_of` stay (vcs-health maps to live paths);
  - `logging_registrations` (11b) is removed: a newly appearing repo is reported by the comparison, because its decision files aren't in the baseline.
- **`discover`:** its "unregistered → never pruned" note changes, since registration now matters only for convert.
- **Docs:**
  - snapshot-prune.md (no "only registered projects");
  - project-roots.md (convert only);
  - security.md (registered-project rows);
  - files.md, README, CHANGELOG (breaking: prune ignores project-roots.list).
  - One note covering: rules apply to everything below their file, including nested repos (a nested repo's own closer `-` wins); a broad user rule like `+ build` in `~/src` can hit a non-VCS directory's real `build/` (nothing guards that); a moved project re-reports its paths once.
- **Tests:**
  - remove the `projects_in`, unregistered and nested-registered tests;
  - add: a root-level `~/src` decision file applies below; a nested repo with its own `-`; an enclosing rule reaches a nested repo; no decision file → exit 0; a foreign `--since`; a single survey;
  - e2e `Env` and the CI script drop `projects register`.
- **After merge:** time `prune --dry-run` on a real `~/src` (by the owner) before touching `TimeoutStartSec`.
- **No way to skip big directories, by design (owner):** "if not needed take it out from managed subvol". There's no ignore mechanism for prune.
- **Deferred until a performance problem is actually seen (owner: "kind of against it till we really see performance problem"):**
  - compare the compiled rules instead of the materialized pruned paths;
  - a top-of-file marker in a decision file saying "no nested decision files below", so the walk could stop early.

**Pre-implementation review of §8.2 (QA + Security):**
1. **Live-subvolume guard (Security, Medium; already possible today).** `prune` must refuse anything that isn't `<subvolume>/.snapshots/<n>/snapshot`, compared canonically, even with `--subvolume`. It refuses `snapshot == subvolume` and requires the snapshot root to be a subvolume root (inode 256). Tests: prune on the live subvolume → exit 2, nothing deleted.
2. **Nested subvolumes (owner: "if it walks a proper snapshot it cannot see any nested subvolume; if it can, it's probably not a snapshot and should not prune").**
   - BTRFS snapshots aren't recursive. Inside a real snapshot, every nested subvolume (`.snapshots`, convert-made ones) is only an empty placeholder directory (inode 2), never a subvolume root.
   - **A subvolume root (inode 256) found anywhere below the snapshot root:** prune refuses the whole run (exit 2: not a proper snapshot). The script then locks the snapshot and tags it `failed`, as for any prune error.
   - **Inode-2 placeholders:** skipped silently. They're empty, there's nothing to prune, and they're never candidates, so there's no `refused` noise.
   - A unit test with a real nested subvolume verifies both.
3. **Single survey (QA, High):**
   - `prune(snapshot, candidates, vcs, timeout)` takes main's walk result instead of walking again;
   - `check()` checks containment against the snapshot root and loses `project`/`projects` and guard 3;
   - test: exactly the candidates passed in are pruned.
4. **`--since` becomes a time window (owner: "a date time expression, exact date or delta; search for the oldest snapshot match"; filter in Rust if Snapper's listing is stable).** Superseded by §8.3.
5. **No decision files (QA and Security differed):** exit 0, tagged `ok`, with a note on stderr (journal). This follows the owner's "no decision file means no prune". A wrong subvolume is caught by (1); unreadable decision files already warn. Security's `verify=no-rules` tag was not taken, to keep things simple.
6. **Migration (QA, High):**
   - CHANGELOG "Breaking: prune ignores project-roots.list; the first run after upgrading lists newly pruned paths (unregistered dirs with decision files, parent rules reaching child repos) in events.log, so read it";
   - the same note in snapshot-prune.md.
7. **Complete doc and comment list:**
   - `prune.rs` module doc and `main.rs` prune help;
   - README (lines 41, 45, Known limitations);
   - FAQ (11, 22);
   - snapshot-prune.md (setup step, registration-log text);
   - security.md ("only registered projects", the "another registered project" guard, "Silent no-op" → exit 2 only for layout/`--since` errors, the 11b registration-log text);
   - files.md (project-roots.list is for convert/decide only);
   - project-roots.md;
   - the earlier plan lines listing guard 3 (marked superseded);
   - `discover::unregistered_note` and its test: removed (registration doesn't affect prune any more).
8. **Tests:**
   - delete `unregistered_or_missing_projects_are_ignored`, the registration-log CLI test, and the e2e `Env` project-roots.list write;
   - rewrite QR-1 as "a symlinked dir under the snapshot is never entered or deleted through";
   - rewrite `nested_registered_projects…` as "an enclosing rule reaches a nested repo; the nested repo's `-` wins";
   - rewrite the projects half of `dirs_holding_vcs_metadata…`;
   - add: e2e "no decision file → `verify=ok`, exit 0"; the live-subvolume refusal; nested-subvolume skip; `--since` validations.

### 8.3 `--since <time>` (owner ruling 2026-10-08; reviewed, see the end)
**Snapper CLI stability (checked):**
- `--csvout`/`--jsonout` and `--columns` date from 0.8.6 (2019), a documented machine interface. No renames or shape changes since.
- `read-only` column: 0.10.5 (2023); we require 0.12 or later.
- JSON: `{"<config>": [{number: int, date: str, userdata: {k: v}, "read-only": bool, …}]}`.
- Machine-readable dates are always ISO; `--utc` is passed explicitly. Snapper has no date filter, so the filtering is ours.

**Design:**
- `prune --config <c> --since <expr> <snapshot>`, where `<expr>` is either:
  - a delta (`1d`, `6h`, `humantime::parse_duration`);
  - an exact UTC time (`2026-10-07`, `2026-10-07 12:00[:00]`).
- `prune` runs `snapper --jsonout --utc -c <c> list --columns number,date,userdata,read-only` (timeout, both pipes drained, via `vcs::run`-style plumbing) and parses it with `serde_json` (new dependency).
- **Baseline:** the oldest snapshot with
  - date ≥ cutoff,
  - number < this snapshot's,
  - `read-only` true,
  - `userdata.verify` in {ok, refused, rules-changed, recovered, health-failed, health-timeout, manifest-changed};
  - fallback: the newest such snapshot before the cutoff;
  - none at all → no baseline, so everything is reported.
- **A snapper failure or unparsable output** → no baseline, with a stderr note. It warns more, never less.
- **The baseline path** is `<subvolume>/.snapshots/<n>/snapshot` from the same layout; it is still layout-checked before reading.
- **Repeated warnings are accepted (owner):** each run inside the window re-reports a change, about 24 events.log lines per change with `1d`.
- **Script:** passes `--config "$C" --since "${SINCE:-1d}"` in the main run and in recovery; `base()` is removed. `SINCE` can be set per timer through the unit's `Environment=`.
- **Manual review:** `prune --dry-run --config src --since 7d <snapshot>` reports without logging.
- **Tests:**
  - parse the deltas and dates;
  - JSON parsing: real snapper sample, missing keys, wrong types, extra configs, CSV-quoted userdata impossible;
  - baseline selection: in the window, fallback, unusable tags, read-only false, number ≥ self;
  - a snapper failure means no baseline;
  - the fake-snapper e2e gains `--jsonout list` support.

**Review of §8.3 (QA verified against snapper master source; Security):**
1. **The window is measured from the target snapshot's own date (QA, High):** `cutoff = date(self) − SINCE`, with `self` taken from the same listing; if it's unlisted, use now. This keeps late crash recovery and `--dry-run` against an old snapshot correct.
2. **Config ↔ subvolume cross-check (Security, Medium).**
   - `snapper -c C get-config`'s `SUBVOLUME` must equal the pruned snapshot's subvolume (canonical paths); otherwise use no baseline, with a note.
   - Only the JSON key equal to C is read.
   - The config name must match `[A-Za-z0-9_.-]+` and not start with `-`; otherwise exit 2.
3. **The real JSON shape (QA):**
   - `{"<config>": [...]}`; `number` int, `read-only` bool;
   - `userdata` is an object or **null** (null means untagged);
   - snapshot 0 has `date: ""` and is skipped;
   - the date is `"%F %T"` with no zone; `--utc` (a global option, before `list`) makes it UTC, and it's parsed as naive UTC.
   - Test with a real sample.
4. **Retention caps the window (QA):** the effective window is min(`SINCE`, the retained span). snapshot-prune.md recommends `TIMELINE_LIMIT_HOURLY` ≥ 24 for `1d`.
5. **Snapper failure (Security):** with no baseline *because of a failure*, events.log gets one summary line (`baseline unavailable (<why>): N path(s) pruned unchecked`); stdout/journal still lists them all. A genuine first run (no earlier usable snapshot) logs the full list.
6. **Forged baselines (Security):** anyone in `ALLOW_USERS`/`ALLOW_GROUPS` can create a snapshot tagged `verify=ok`. This is documented as trusted, with the recommendation `ALLOW_USERS=<owner>` only. The baseline also requires `description == "pruned"` (the script's own snapshots), so accidental manual snapshots are excluded.
7. **Time parsing:**
   - a delta (`humantime`), or `YYYY-MM-DD[ HH:MM[:SS]]` in UTC (a small parser), documented as UTC;
   - a future cutoff, an overflow (`checked_sub`) or anything else → exit 2.
8. **Running snapper:** argv only, `LC_ALL=C`, a timeout, stdout capped (16 MiB). Timeout or failure → no baseline (item 5).
9. **Inode rule confirmed (Security).** Additionally, any directory whose `st_dev` differs from the snapshot root's (a mount or bind mount inside the snapshot path, or a nested subvolume) refuses the run, so `remove_dir_all` can never cross a mount.
10. **fake-snapper (Python):** stores a UTC `%F %T` date at create, with a `FAKE_SNAPPER_DATE` test override; supports `--utc`, `--jsonout list --columns …` (null userdata, snapshot 0 with an empty date), `get-config` (SUBVOLUME) and `--description`; keeps the CSV.
11. **Dependencies:** `serde_json` is justified (`serde` is already a dependency).

**Size:** about 120 lines of Rust, about 170 lines of tests, about 40 lines of fake-snapper; the script gets smaller (`base()` is removed).

*Superseded by §8.2: guard 3 in §5 Step 5 ("a nested project refused") and every "registered projects" scope for prune. Done in Step 11c.*
