# GhostVolumes

[![CI](https://github.com/braindevices/ghostvolumes/actions/workflows/ci.yml/badge.svg)](https://github.com/braindevices/ghostvolumes/actions/workflows/ci.yml)
![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%2F%20BTRFS-informational)

Long Snapper history of your projects **without the build noise**. Volatile, regenerable directories (`node_modules`, `target`, `.venv`, `build`, …) stay where your tools put them. GhostVolumes removes them from your **snapshots**, not from your working tree, so long retention costs only real source changes.

**Requires Linux with BTRFS.** GhostVolumes exits cleanly with a clear message on any other platform.

<!-- START doctoc generated TOC please keep comment here to allow auto update -->
<!-- DON'T EDIT THIS SECTION, INSTEAD RE-RUN doctoc TO UPDATE -->
**Table of Contents**

- [How it works](#how-it-works)
- [Install](#install)
- [Shell completions](#shell-completions)
- [Commands](#commands)
- [Configuration](#configuration)
- [Debugging](#debugging)
- [Upgrading](#upgrading)
- [Known limitations](#known-limitations)
- [License](#license)

<!-- END doctoc generated TOC please keep comment here to allow auto update -->

## How it works

Every hour Snapper takes a **writable** snapshot of your projects subvolume (`~/src`). `ghostvolumes prune` deletes the directories you've decided are volatile **inside that snapshot**, and the snapshot is locked read-only. Snapper's timeline cleanup then keeps it as long as you like. Your working tree, build tools and IDEs are never touched, and nothing is injected into any process.

- **Explicit, reviewable decisions.** A `+`/`-` line in a project's `.ghostvolumes-decisions` says what's volatile. Undecided means kept. Decisions are committable and work with any VCS (or none).
- **Safe by construction.** It never prunes outside the snapshot, through a symlink, VCS metadata, tracked files, or (in git repos) anything the repo doesn't ignore.
- **Proven per snapshot.** VCS metadata gets a byte-identical manifest before and after pruning. Each changed repo gets a health check (git: `fsck --connectivity-only`), which only runs when Snapper's own comparison says that repo changed.
- **Transparent automation.** A ~65-line shell script plus a systemd user timer. Results are tagged on each snapshot (`verify=ok|refused|rules-changed|…`), alerts go to the journal, and decision changes are logged for you to read. No snapshot is ever deleted by the tool.

Setup and day-to-day use: **[snapshot-prune.md](documents/snapshot-prune.md)**.

```
$ ghostvolumes discover ~/src                                   # what looks volatile? (read-only)
$ ghostvolumes decide ~/src/app --add node_modules --add /target  # record decisions (that's all prune needs)
$ systemctl --user enable --now ghostvolumes-snapshot@src.timer  # hourly pruned snapshots
```

- **[`discover [path]`](documents/discover.md)** — a read-only survey suggesting decisions. It includes tool-tagged caches (`CACHEDIR.TAG`).
- **[`decide <path>`](documents/decide.md)** — walks a project and records `+`/`-` decisions (asks on a TTY), or hand-authors them with `--add`/`--deny`.
- **`prune`, `vcs-manifest`, `vcs-health`** — the building blocks the snapshot script calls; see [snapshot-prune.md](documents/snapshot-prune.md).
- **[`convert <path>`](documents/convert.md)** *(optional)* — also turns decided directories in the **live** tree into nested BTRFS subvolumes, for snapshot setups that should skip them entirely.

Shared reference:
- [decision-files.md](documents/decision-files.md): the `.ghostvolumes-decisions` syntax.
- [project-roots.md](documents/project-roots.md): registered projects (for `convert`/`decide`).
- [files.md](documents/files.md): every file GhostVolumes reads or writes.
- [security.md](documents/security.md): the threat model and its defenses.
- [design.md](documents/design.md): design history, including the retired `LD_PRELOAD` shim.
- [FAQ.md](documents/FAQ.md): common questions.

## Install

```bash
cargo install --locked --git https://github.com/braindevices/ghostvolumes --tag vX.Y.Z
ghostvolumes init                # write default config
```

Then follow [snapshot-prune.md](documents/snapshot-prune.md#setup): the projects subvolume, the Snapper config (Snapper ≥ 0.12), and the timer (`ghostvolumes contrib` prints the bundled script and units).

Pick `vX.Y.Z` from [Releases](https://github.com/braindevices/ghostvolumes/releases), or drop `--tag` to build the tip of `main`. `--locked` builds with the committed `Cargo.lock`. `cargo install --git` clones the whole repository, including this project's `ai-work/` planning notes. To avoid that, install from a release's source archive, which excludes `ai-work/` (see `.gitattributes`):

```bash
curl -L -o ghostvolumes.tar.gz https://github.com/braindevices/ghostvolumes/archive/refs/tags/vX.Y.Z.tar.gz
tar xf ghostvolumes.tar.gz
cargo install --locked --path ghostvolumes-X.Y.Z
```

## Shell completions

Dynamic: subcommands and flags, plus live data (registered projects, pending `?` patterns for `decide --add`/`--deny`):

```bash
echo 'source <(COMPLETE=bash ghostvolumes)' >> ~/.bashrc   # or ~/.zshrc with COMPLETE=zsh
```

## Commands

| Command | What it does |
|---|---|
| `ghostvolumes discover [PATH] [flags]` | Survey for undecided directories, tool-tagged caches and drift; suggests commands — see [discover.md](documents/discover.md) |
| `ghostvolumes decide <path> [--max-depth N] [--add <pattern>]... [--deny <pattern>]...` | Record `+`/`-` decisions (walk and ask, or hand-author) — see [decide.md](documents/decide.md) |
| `ghostvolumes projects list` / `register <path>` / `unregister [path]` | Manage registered projects — see [project-roots.md](documents/project-roots.md) |
| `ghostvolumes prune [--dry-run] [--config <c> --since <1d\|time>] <snapshot>` | Delete the decided directories inside a writable Snapper snapshot (exit 0 clean, 1 something refused, 2 error, 3 already read-only, 4 decision changes within the window prune something new, logged to `events.log` first) |
| `ghostvolumes vcs-manifest <path>` | Print a manifest of every VCS metadata dir (for before/after comparison) |
| `ghostvolumes vcs-health <snapshot> [--changes FILE]` | Run each repo's health command in a snapshot; with `--changes` (from `snapper status`), only changed repos |
| `ghostvolumes contrib [name]` | Print a bundled helper: the snapshot script, systemd units, login check |
| `ghostvolumes convert <path> [--max-depth N] [--create <relative-path>]... [--dry-run] [--delete-backup]` | *Optional:* make decided directories in the live tree nested subvolumes — see [convert.md](documents/convert.md) |
| `ghostvolumes roots scan [--save]` / `list` / `disable <path>` / `enable <path>` | Snapshot-managed roots and their watched names (used by `discover`/`decide`/`convert`) |
| `ghostvolumes reload` | Recompile the roots cache after hand-editing `roots.d` |
| `ghostvolumes init` | Write default config; after an upgrade, recompile caches and remove the retired shim |

## Configuration

- `~/.config/ghostvolumes/vcs.toml`: per-VCS metadata dirs and the guard, allow and health commands. Defaults cover git, hg, svn, jj and dvc; see [snapshot-prune.md](documents/snapshot-prune.md#per-vcs-settings-configghostvolumesvcstoml).
- `~/.config/ghostvolumes/roots.d/*.toml`: snapshot-managed roots and the names `discover`/`decide` suggest. Merged in sorted-filename order, **last file wins per field**:

```toml
default-watches = ["node_modules", "target", ".venv", "build"]
default-ignore = [".git", ".hg", ".svn", ".snapshots"]
delete-convert-backup = false       # convert only

["/home/user/some-project"]
watches = ["node_modules", "dist"]  # this root only watches these two

["/mnt/noisy-backup-drive"]
enabled = false
```

- `.ghostvolumes-ignore`: per-project directories `convert`/`decide` never walk into (`prune` doesn't read it: everything in the managed subvolume is pruned by decisions alone). See [files.md](documents/files.md) for every file.

## Debugging

`GHOSTVOLUMES_DEBUG` sets the verbosity of `convert`/`decide` (`error`, `warn`, `info` (default), `debug`, `trace`). Output goes to stderr, or to `GHOSTVOLUMES_LOG_FILE` if set. The snapshot timer logs to the journal: `journalctl --user -u ghostvolumes-snapshot@src`. Each snapshot's result is in `snapper -c src list` (`verify=…`).

## Upgrading

```bash
cargo install --locked --git https://github.com/braindevices/ghostvolumes --tag vX.Y.Z --force
ghostvolumes init
```

`init` recompiles existing caches for the new version and removes the `LD_PRELOAD` shim older versions installed. Also stop using `ghostvolumes intercept` and any `shell-init` export: both are gone. Then re-print the snapshot script with `ghostvolumes contrib ghostvolumes-snapshot` if it changed.

## Known limitations

- **The whole subvolume is managed:** every decision file in it applies to everything below it; there's no way to exclude a directory from pruning except a `-` decision or moving it out of the subvolume.
- **Unsupported repos** (skipped with a message): git alternates (`clone --shared`/`--reference`), linked worktrees, and VCS dirs that are symlinks.
- **Snapper ≥ 0.12** is required, and Snapper itself needs root (`snapperd`) for snapshots.
- **No prebuilt binaries;** installs build from source.

## License

MIT OR Apache-2.0 — see [LICENSE-MIT](LICENSE-MIT) / [LICENSE-APACHE](LICENSE-APACHE).
