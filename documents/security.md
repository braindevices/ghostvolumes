# Security model

What GhostVolumes defends against, how, and what it deliberately doesn't. The background is the 2026-10 audit (`ai-work/audit-2026-10-06.md`) and its follow-up debates between QA, security and the owner.

## Threat model

- **The user is trusted.** That covers their shell, environment variables (`GHOSTVOLUMES_*`, `HOME`, `XDG_*`), their config under `~/.config/ghostvolumes`, and paths they type.
- **Repository content is untrusted.** A cloned repo can commit anything: `.ghostvolumes-decisions`, `.ghostvolumes-ignore`, symlinks, and directory names with newlines or `$(…)`. The shim reads decision files on every watched-name `mkdir` under a configured root, even in projects that were never registered. So simply cloning or pulling inside an `intercept` shell is enough to expose the shim to them.
- **The shim runs inside arbitrary host processes** (git, cargo, npm, editors). It must never crash, block, write to their stdout/stderr, or leave the host's file descriptors in a different state. When in doubt it passes the call through to the real `mkdir`.
- **Another local user is out of scope** whenever they already have write access to the project or one of its ancestors: that access is strictly more powerful than anything they could gain by racing us (see the `mkdir` mode section).

## Defenses

| Risk | Defense |
|---|---|
| A decision pattern escapes the project (`+ /../../Documents`) | Patterns with `.`/`..` components or control characters are invalid and ignored (`valid_pattern`). `convert` additionally refuses any target that isn't strictly inside the project via plain directory names, with no symlinked component and no non-directory (`check_contained`) |
| A committed `.ghostvolumes-decisions` that is a symlink (to `~/.bashrc`, `/dev/zero`, a FIFO, a secret) | Decision and ignore files are only read or appended as regular files of at most 1 MiB. The file is checked with lstat, then opened, then fstat must report the same dev/ino. A missing file is created with `O_EXCL`, which never follows a symlink (`open_regular_file`) |
| A name breaks a one-entry-per-line file (a newline forges decision lines or `compiled.tsv` rows; a tab splits a row; trimmed whitespace or a literal `**` changes meaning) | Never escaped: such names are rejected or skipped with a warning by one shared check, `representable_path` (no control characters, no leading/trailing whitespace, not `.`/`..`/`**`). It runs at every writer: decision-file markers (`anchored_pattern`), `compiled.tsv` rows (`cache::compile`, watched names also without `/`), `roots scan` (a disk label is chosen by whoever made the disk: `/run/media/<user>/<LABEL>`), `projects register`, and `reload`'s canonicalization of project roots (kept as written if the real path fails). No sensible project names a volatile directory like that, so "not representable" just means "not managed" |
| A malformed decision file crashes the host (BOM, non-ASCII) | The parser can't panic (`parse_lines`) |
| The shim acts on a path the text resolves differently from the kernel (`a/link/..`) | The target's parent is opened and its real path read back from `/proc/self/fd`. Decisions use that path, and the subvolume is created on the same open directory. A bad dirfd, deleted directory, missing `/proc` or unreadable parent → pass through |
| Markers or subvolumes owned by the wrong user (`sudo -E` with your `HOME`) | The shim passes through unless the data dir is owned by the process's euid. `intercept` refuses in that case. Not a plain `euid == 0` check, because root-only containers own their data dir |
| `init` crashing running shells | The shim is installed by temp file + rename, never rewritten in place. A stale shim makes `intercept` refuse until `init` is run |
| Log lines written into a host's file after it reuses fd numbers | The log is opened per line, never held |
| Copy-pasting a suggestion runs a crafted name | `discover` quotes paths and names with `shlex::try_quote` (already in the dependency tree via `clap_complete`; never the deprecated `quote`, RUSTSEC-2024-0006), with a test pinning the output. A path or name with control characters gets no runnable command at all, only an escaped mention, so escape sequences can't rewrite what's displayed before you paste |
| Supply chain | Third-party GitHub Actions pinned by SHA (see below); CI token read-only; `cargo-release` version pinned; installs use `--locked` |
| `convert` losing writes made while it copies | The old tree is kept (reflinked, git-ignored) unless `--delete-backup` or `delete-convert-backup = true` is given |

## `mkdir` mode: why there's a window, and why it doesn't matter

A real `mkdir(path, 0700)` sets the mode atomically. `BTRFS_IOC_SUBVOL_CREATE` takes no mode and creates `0777 & ~umask`, so the shim creates the subvolume first and then narrows the mode. For a few microseconds the directory exists with the umask default (usually 0755). That window can't be closed without a mode-aware create ioctl.

The fix-up applies `(cur & mode & 0o777) | (mode & 0o1000) | (cur & 0o2000)`: that's `mode & ~umask`, plus the requested sticky bit and any setgid inherited from the parent, as a real `mkdir` would give. It works like this:

1. Open `target.path`, the kernel-resolved real path the shim decided on.
2. On the opened fd, require a directory with inode 256 (a subvolume root).
3. `fchmod` through that same fd.

**chmod follows symlinks** (`chmod(2)` and open + `fchmod` alike). The no-follow variant, `fchmodat(AT_SYMLINK_NOFOLLOW)`, fails with `EOPNOTSUPP` on a symlink. So the guard against a symlink swapped in at the path is step 2, the check on the opened fd, not chmod's own behaviour. That check proves "a subvolume root", not "our subvolume", because every BTRFS subvolume root has inode 256.

**Why that gap doesn't matter.** We considered anchoring the open to the parent directory (`/proc/self/fd/<parent fd>/<name>`, an `openat` stand-in), and decided against it:

- Redirecting the chmod to another of the victim's subvolumes means renaming or replacing an entry in an ancestor of the project during the window.
- Anyone who can do that can replace the whole project before the build starts, so every file the build creates lands in a tree they control. Racing our chmod gains them nothing beyond that.
- A sticky-bit ancestor (like `/tmp`) stops the rename outright, and an attacker-owned ancestor already means an attacker-controlled tree.
- An attacker with write access to the project directory itself can interfere with a plain `mkdir` just as well. The shim is no weaker than POSIX here.

The only case the anchor would have helped is accidental: your own tooling replacing a path component with a symlink mid-build. That's a robustness corner, not a security property, and not worth extra machinery.

## Pinned GitHub Actions

Third-party actions are pinned to full commit SHAs, written `uses: owner/repo@<sha> # <ref>`. The `# <ref>` comment names what a bump follows. For example, `dtolnay/rust-toolchain` is pinned to its `stable` branch, whose only difference from `master` is that the `toolchain` input defaults to `stable`.

- **The SHA is the security boundary, not the ref.** A full SHA is immutable, so CI runs exactly the code that was reviewed, whichever branch or tag it came from. Branches and tags are both mutable, so neither is a boundary by itself.
- **The risk is at bump time.** `scripts/bump-action-pins.sh` pins whatever the ref points to *now*, and branches like `stable` move often. Treat every bump as a code review: read `https://github.com/<repo>/compare/<old>...<new>` before committing, and never auto-merge a bump. Running `--check` on a schedule just to notify is fine.
- **Explicit inputs are kept even when they repeat a default** (`toolchain: stable`). If a bump lands on a commit whose default changed, or the pin moves to another branch, CI still gets the intended toolchain instead of drifting or failing.
- **Resolution order:** tags take precedence over a branch with the same name, so a future upstream tag named `stable` would be pinned instead of the branch. That's harmless, but it's visible in the bump's output.

## Deliberately not defended

| Item | Why |
|---|---|
| A FIFO or other file swapped in between the shim's directory check and its open | Needs an active racer. A fix needs `O_NONBLOCK`/`O_DIRECTORY`, whose values differ by architecture in a shim with no `libc` crate |
| Hardlinked decision files | git can't create them, `protected_hardlinks` blocks it across users, and legitimate tools (`cp -l`, nix, ostree) do create them |
| The swap window in `convert` (cp → rename → rename) | Path-based, so a concurrent racer could redirect it. Under the static-repo threat model the data is moved, not destroyed, and the backup is kept |
| Terminal escape sequences in paths printed by `convert`/`decide`/`intercept`, and forged lines in `shim.log` | Display-only and user-local. `discover`'s copy-paste suggestions are the exception, handled above |
| Privileged and setuid processes | glibc ignores `LD_PRELOAD` entries with a slash for setuid binaries, and sudo strips `LD_*` |
