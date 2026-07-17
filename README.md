# GhostVolumes

[![CI](https://github.com/braindevices/ghostvolumes/actions/workflows/ci.yml/badge.svg)](https://github.com/braindevices/ghostvolumes/actions/workflows/ci.yml)
![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%2F%20BTRFS-informational)

Isolates volatile build artifacts (`node_modules`, `target`, `.venv`, `build`, ...) into unsnapshotted BTRFS subvolumes, so your snapshot tool (Snapper, Timeshift, btrbk...) skips them instead of wasting space and time on things you'll regenerate anyway.

**Requires Linux with BTRFS.** GhostVolumes exits cleanly with a clear message on any other platform.

<!-- START doctoc generated TOC please keep comment here to allow auto update -->
<!-- DON'T EDIT THIS SECTION, INSTEAD RE-RUN doctoc TO UPDATE -->
**Table of Contents**

- [Features](#features)
- [Install](#install)
- [How it works](#how-it-works)
- [Commands](#commands)
- [Configuration](#configuration)
- [Debugging](#debugging)
- [Upgrading](#upgrading)
- [Known limitations](#known-limitations)
- [License](#license)

<!-- END doctoc generated TOC please keep comment here to allow auto update -->

## Features

- **Zero sudo at runtime** — subvolume creation only needs standard filesystem permissions.
- **Near-zero overhead** — an `LD_PRELOAD` hook intercepts `mkdir`/`mkdirat` directly, no polling or file-watching.
- **Explicit, reviewable decisions** — every conversion is backed by a committed `+`/`-` record, never a silent guess.
- **VCS-agnostic** — works the same whether or not a project uses git.
- **Built for your machine** — the shim compiles locally at install time, so it always matches your host's libc.

## Install

```bash
cargo install --git https://github.com/braindevices/ghostvolumes
ghostvolumes init                # compile + install the LD_PRELOAD shim, write default config
ghostvolumes roots scan --save   # detect your snapshot-managed BTRFS roots
```

That's the whole setup. **Don't** add `eval "$(ghostvolumes shell-init bash)"` (or `zsh`) to your shell rc file — see the [FAQ](documents/FAQ.md#why-not-just-export-ld_preload-globally) for why. Nothing converts automatically after this step; see the [FAQ](documents/FAQ.md) for the recommended workflow.

## How it works

A directory gets a `+`/`-` decision recorded once; every future build reuses it automatically, no prompt, no guessing:

```
$ npm install                              # node_modules created as a plain directory
$ ghostvolumes convert .                   # asks once, records a decision
$ ghostvolumes intercept -- npm install    # from now on: automatic, no prompt
```

- **[`intercept -- <cmd>`](documents/intercept.md)** — runs `<cmd>` with the shim active, converting anything already decided `+`. Never prompts.
- **[`convert <path>`](documents/convert.md)** — registers `<path>` as a project, then walks it asking about each undecided candidate.
- **[`decide <path>`](documents/decide.md)** — the same walk as `convert`, but only ever records decisions, never touches the filesystem.
- **[`discover [path]`](documents/discover.md)** — a read-only survey of an arbitrary path, suggesting `decide`/`convert` commands to run rather than acting itself.

Shared reference: [decision-files.md](documents/decision-files.md) (the `.ghostvolumes-decisions` syntax), [project-roots.md](documents/project-roots.md) (why projects can't nest), and [files.md](documents/files.md) (every file GhostVolumes reads or writes, annotated). [design.md](documents/design.md) has the full rationale, and [FAQ.md](documents/FAQ.md) has common workflow questions.

## Commands

| Command | What it does |
|---|---|
| `ghostvolumes roots scan [--save]` | Detect BTRFS snapshot-managed roots |
| `ghostvolumes roots list` | List every configured root and its effective watch list |
| `ghostvolumes reload` | Rebuild the runtime cache after hand-editing `roots.d` |
| `ghostvolumes discover [PATH] [flags]` | Survey for undecided directories and drift, suggesting `decide`/`convert` commands to run — see [discover.md](documents/discover.md) |
| `ghostvolumes convert <path> [--max-depth N] [--create <relative-path>]... [--dry-run]` | Register `<path>` as a project (asks if not already), then recursively resolve subvolume candidates under it — see [convert.md](documents/convert.md) |
| `ghostvolumes decide <path> [--max-depth N] [--add <pattern>]... [--deny <pattern>]...` | Walk and resolve decisions like `convert`, but never convert anything; also hand-authors `+`/`-` decisions directly — see [decide.md](documents/decide.md) |
| `ghostvolumes projects list` | List registered project roots, flagging any that no longer exist |
| `ghostvolumes projects register <path>` | Register a project root (usually automatic via `convert`) — see [project-roots.md](documents/project-roots.md) |
| `ghostvolumes projects unregister [path]` | Remove a project root; with no path, scan and interactively prune stale ones |
| `ghostvolumes intercept -- <cmd>` | Run `<cmd>` with the shim active, converting anything with a recorded `+` decision — see [intercept.md](documents/intercept.md) |
| `ghostvolumes init` | Install the shim and default config (idempotent, safe to re-run) |
| `ghostvolumes shell-init <bash\|zsh>` | Print the `LD_PRELOAD` value `intercept` uses (diagnostic only) |

## Configuration

Global config lives under `~/.config/ghostvolumes/roots.d/` — see [files.md](documents/files.md) for every file GhostVolumes reads or writes, config and data alike:

```
roots.d/00-auto.toml     # written by `roots scan --save` — regenerated, don't hand-edit
roots.d/00-defaults.toml # ships with the package — written once by `init` if missing
roots.d/10-local.toml    # hand-edited: extra roots, per-root overrides, disabling a root
```

Every `*.toml` file in `roots.d/` is merged in sorted-filename order,
**last file wins per field** (no unions) — a root path gets its own
table, with an optional `enabled` (default `true`) and `watches`
(replaces, not adds to, `default-watches` for that root):

```toml
default-watches = ["node_modules", "target", ".venv", "build"]
default-ignore = [".git", ".hg", ".svn", ".snapshots"]

["/home/user/some-project"]
watches = ["node_modules", "dist"]   # this root only watches these two

["/mnt/noisy-backup-drive"]
enabled = false                      # roots scan --save keeps finding this root; suppress it
```

A disabled root doesn't cascade to any other root nested under its
path — each root path is its own independent entry.

`default-ignore` is global-only — unlike `watches`, there's no
per-root `["/path"] ignore = [...]` override. Per-root/per-project
ignore patterns instead live in their own `.ghostvolumes-ignore` file,
decentralized rather than merged through `roots.d` — see
[convert.md](documents/convert.md#ignoring-directories-entirely).

## Debugging

The shim always logs critical events (a subvolume created, an undecided candidate skipped, an unexpected error) to `~/.local/share/ghostvolumes/shim.log`. It never writes to stdout/stderr, since it runs inside arbitrary host processes. `convert`/`decide` share the same verbosity levels and write to stderr by default, or to `GHOSTVOLUMES_LOG_FILE` if set.

`GHOSTVOLUMES_DEBUG` takes one of five levels (case-insensitive; unset, empty, or unrecognized all mean `info`):

| Level | |
|---|---|
| `error` | Quietest |
| `warn` | |
| `info` | Default — critical events only |
| `debug` | Every decision and why |
| `trace` | Most verbose |

Each logged line is prefixed with a timestamp, pid, and level: `[<ISO-8601-UTC>] [pid <pid>] [<LEVEL>] <message>` (e.g. `[2026-07-16T18:50:01.461Z] [pid 369670] [DEBUG] ...`).

```bash
GHOSTVOLUMES_DEBUG=debug ghostvolumes intercept -- npm install   # log every decision and why
GHOSTVOLUMES_LOG_FILE=/path/to/log ghostvolumes intercept -- npm install   # redirect the log
GHOSTVOLUMES_AUTO_YES=1 ghostvolumes intercept -- npm install              # skip the decision lookup (not recommended)
```

## Upgrading

```bash
cargo install --git https://github.com/braindevices/ghostvolumes --force
ghostvolumes init   # re-installs the shim to match the new build
```

## Known limitations

- **Statically-linked binaries** bypass the shim entirely — their syscalls skip libc.
- **A brand-new project with no decisions recorded** gets no benefit from `intercept` on its first build. Run `ghostvolumes convert <project-root>` once to seed decisions.
- **No prebuilt binaries** — the shim must compile against the host's own libc, so installs always build from source.

See [design.md](documents/design.md) for the reasoning behind these tradeoffs, and [FAQ.md](documents/FAQ.md) for common workflow questions.

## License

MIT OR Apache-2.0
