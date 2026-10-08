# Security model

What GhostVolumes defends against, how, and what it deliberately doesn't. The background is the 2026-10 audit (`ai-work/audit-2026-10-06.md`) and its follow-up debates between QA, security and the owner.

## Threat model

- **The user is trusted.** That covers their shell, environment variables (`GHOSTVOLUMES_*`, `HOME`, `XDG_*`), their config under `~/.config/ghostvolumes`, and paths they type.
- **Repository content is untrusted.** A cloned repo can commit anything: `.ghostvolumes-decisions`, `.ghostvolumes-ignore`, symlinks, `.git` files with a crafted `gitdir:`, and directory names with newlines or `$(…)`. `prune` reads it from every directory of every snapshot.
- **Snapper, `snapperd` and the system are trusted.** Snapshots are static once taken: nothing races `prune` inside them.
- **Another local user is out of scope** whenever they already have write access to the project or one of its ancestors. That access already lets them replace the whole project, so racing our steps gains them nothing.
- **Nothing runs inside other processes.** The retired `LD_PRELOAD` shim did; see "Retired" below.

## Defenses

| Risk | Defense |
|---|---|
| A decision pattern escapes the project (`+ /../../Documents`) | Patterns with `.`/`..` components or control characters are invalid and ignored (`valid_pattern`). `convert` additionally refuses any target that isn't strictly inside the project via plain directory names, with no symlinked component and no non-directory (`check_contained`) |
| A committed `.ghostvolumes-decisions` that is a symlink (to `~/.bashrc`, `/dev/zero`, a FIFO, a secret) | Decision and ignore files are only read or appended as regular files of at most 1 MiB. The file is checked with lstat, then opened, then fstat must report the same dev/ino. A missing file is created with `O_EXCL`, which never follows a symlink (`open_regular_file`) |
| A name breaks a one-entry-per-line file (a newline forges decision lines or `compiled.tsv` rows; a tab splits a row; trimmed whitespace or a literal `**` changes meaning) | Never escaped: such names are rejected or skipped with a warning by one shared check, `representable_path` (no control characters, no leading/trailing whitespace, not `.`/`..`/`**`). It runs at every writer: decision-file markers (`anchored_pattern`), `compiled.tsv` rows (`cache::compile`, watched names also without `/`), `roots scan` (a disk label is chosen by whoever made the disk: `/run/media/<user>/<LABEL>`), `projects register`, and `reload`'s canonicalization of project roots (kept as written if the real path fails). No sensible project names a volatile directory like that, so "not representable" just means "not managed" |
| A malformed decision file crashes a run (BOM, non-ASCII) | The parser can't panic (`parse_lines`) |
| Copy-pasting a suggestion runs a crafted name | `discover` quotes paths and names with `shlex::try_quote` (already in the dependency tree via `clap_complete`; never the deprecated `quote`, RUSTSEC-2024-0006), with a test pinning the output. A path or name with control characters gets no runnable command at all, only an escaped mention, so escape sequences can't rewrite what's displayed before you paste |
| Supply chain | Third-party GitHub Actions pinned by SHA (see below); CI token read-only; `cargo-release` version pinned; installs use `--locked`. The CI's snapper repo key (OBS) is pinned by fingerprint (as first seen). `scripts/dev-snapper.sh` (local dev only, as root) pins the SRPM's SHA-256 and the EPEL 10 key fingerprint (as published on fedoraproject.org/security), and its generated wrapper never splices values into code (`awk` reads them from the environment) |
| `convert` losing writes made while it copies | The old tree is kept (reflinked, git-ignored) unless `--delete-backup` or `delete-convert-backup = true` is given |

## Retired: the `LD_PRELOAD` shim

Until 2026-10, a shim injected into build processes created watched directories as subvolumes at `mkdir` time. It was retired because it ran inside every host process (crash, fd, signal and setuid concerns), missed static binaries and anything started outside `intercept`, and carried most of the audit and maintenance cost. Its defenses (kernel path resolution via `/proc/self/fd`, data-dir ownership checks, atomic install, the `mkdir`-mode window analysis) are in the git history and `ai-work/audit-2026-10-06.md`. `init` removes a leftover shim file.

## Snapshot pruning (`prune`, `vcs-manifest`, `vcs-health`)

`prune` deletes directories, so it is guarded the same way as `convert`, plus more (see [snapshot-prune.md](snapshot-prune.md)):

| Risk | Defense |
|---|---|
| Deleting outside the snapshot, or the live tree | Only a real snapshot is accepted: canonically `<subvolume>/.snapshots/<n>/snapshot` and a subvolume root, else exit 2 (so never the live subvolume). The walk never follows symlinks, skips the empty placeholders a snapshot shows for nested subvolumes, and refuses the whole run on a real subvolume or another filesystem below the root (`st_dev`); a candidate containing one is refused, so `remove_dir_all` never crosses a mount. Every candidate passes `check_contained`. Right before deletion, its canonical path must lie inside the canonical snapshot. `remove_dir_all` never follows symlinks. A read-only snapshot is an error (exit 2), never a silent no-op |
| Deleting VCS metadata or versioned source | A candidate containing any configured VCS dir/file or a decision file is refused. Every VCS present at the repo is consulted (a DVC repo is also a git repo). git: tracked content → refused (`ls-files`, with `--literal-pathspecs` so `:(…)`/`*` names are names) |
| A committed `+` line dropping the user's **untracked** work from history | Committed decision files are trusted like `.gitignore` (owner ruling: warn, don't block). In git repos the `allow` command (`check-ignore`) requires the repo to ignore the path, and the `guard` refuses tracked content; other VCSs and plain directories have no such checks by default. Every snapshot is pruned only by its own frozen decision files, so nothing is ever re-pruned. Any decision change that prunes something new (a new `+`, a removed `-`, reordering, a removed override: `prune --since` compares both decision sets on the same tree), or a new repo arriving with decision files, is printed and appended to `~/.local/state/ghostvolumes/events.log` before anything is deleted; the snapshot is tagged `verify=rules-changed` and the login check reports the log until it's deleted. Only decision files inside the snapshot count (each applies to everything below it, so an enclosing repo's rules reach a nested repo unless its own `-` overrides); `.ghostvolumes-ignore` isn't read by `prune`. The baseline comes from `snapper --jsonout list` (only the requested config's key; the config's `SUBVOLUME` must match; the chosen path must pass the same real-snapshot check as the pruned one and be read-only): anyone allowed on the Snapper config can create a snapshot that looks like a baseline and so suppress a warning, so they're trusted (keep `ALLOW_USERS` to the owner). A baseline that can't be determined means everything is reported, never nothing. **Residual (Low):** the repo controls `.gitignore` too, so `+ /notes` plus `notes/` ignored drops that directory's **whole content** from snapshots from then on, including untracked files you keep there. Earlier snapshots still hold what was there before, until retention expires, and the live tree is never touched. Keep personal work out of directories a project marks `+` |
| Running git against the live repo, or running repo-supplied code | Metadata must resolve inside the snapshot (`gitdir:` files followed and checked; alternates/`commondir` refused). Inherited `GIT_DIR`, `GIT_WORK_TREE`, `GIT_INDEX_FILE`, `GIT_COMMON_DIR`, `GIT_OBJECT_DIRECTORY`, `GIT_ALTERNATE_OBJECT_DIRECTORIES` and `GIT_CEILING_DIRECTORIES` are removed. git runs with `core.fsmonitor=false`, `core.hooksPath=/dev/null`, `safe.bareRepository=explicit`, `GIT_NO_LAZY_FETCH=1`, `GIT_OPTIONAL_LOCKS=0`. Commands are argv from the user's config: no shell, stdin closed, timeouts kill |
| Proving metadata survived | A byte-identical before/after manifest of every VCS dir (path, type, size, mtime, inode). `fsck --connectivity-only` on the read-only snapshot, for repos whose metadata changed |
| Skipping a health check by mistake | Snapper's `status` text is parsed fail-safe: any unexpected line, a path outside our subvolume spelling, or a failed `status` means every repo is checked |
| Silent no-op | A path that isn't a real snapshot, a bad `--since`/`--config`, or an unreadable `vcs.toml` → exit 2 (the script locks the snapshot and tags `failed`, which alerts). No decision file at all is not an error ("no decision file, no prune"): it prints a note to the journal |
| The timer script (`contrib/ghostvolumes-snapshot`) | Trusts the user and anyone in Snapper's `ALLOW_USERS` (they can set userdata). Temp files live in a private `mktemp -d` dir. A crashed run's snapshot is pruned (with `--since`) and locked by the next run, or kept as-is if it was already locked (`prune` exit 3). **No snapshot is ever deleted:** one `prune` can't handle is locked and tagged `verify=failed`, which pages; the lock and that provisional tag are one `snapper modify`, so a crash can't make a failed run look `recovered`. A failed before-manifest still prunes (the guards are what protect `.git`) and pages as `failed` |

## Pinned GitHub Actions

Third-party actions are pinned to full commit SHAs, written `uses: owner/repo@<sha> # <ref>`. The `# <ref>` comment names what a bump follows. For example, `dtolnay/rust-toolchain` is pinned to its `stable` branch, whose only difference from `master` is that the `toolchain` input defaults to `stable`.

- **The SHA is the security boundary, not the ref.** A full SHA is immutable, so CI runs exactly the code that was reviewed, whichever branch or tag it came from. Branches and tags are both mutable, so neither is a boundary by itself.
- **The risk is at bump time.** `scripts/bump-action-pins.sh` pins whatever the ref points to *now*, and branches like `stable` move often. Treat every bump as a code review: read `https://github.com/<repo>/compare/<old>...<new>` before committing, and never auto-merge a bump. Running `--check` on a schedule just to notify is fine.
- **Explicit inputs are kept even when they repeat a default** (`toolchain: stable`). If a bump lands on a commit whose default changed, or the pin moves to another branch, CI still gets the intended toolchain instead of drifting or failing.
- **Resolution order:** tags take precedence over a branch with the same name, so a future upstream tag named `stable` would be pinned instead of the branch. That's harmless, but it's visible in the bump's output.

## Deliberately not defended

| Item | Why |
|---|---|
| Hardlinked decision files | git can't create them, `protected_hardlinks` blocks it across users, and legitimate tools (`cp -l`, nix, ostree) do create them |
| The swap window in `convert` (cp → rename → rename) | Path-based, so a concurrent racer could redirect it. Under the static-repo threat model the data is moved, not destroyed, and the backup is kept |
| Terminal escape sequences in paths printed by `convert`/`decide`/`prune` | Display-only and user-local. `discover`'s copy-paste suggestions are the exception, handled above |
