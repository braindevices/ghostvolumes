# GhostVolumes

[![CI](https://github.com/braindevices/ghostvolumes/actions/workflows/ci.yml/badge.svg)](https://github.com/braindevices/ghostvolumes/actions/workflows/ci.yml)
![License](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)
![Platform](https://img.shields.io/badge/platform-Linux%20%2F%20BTRFS-informational)

Isolates volatile build artifacts (`node_modules`, `target`, `.venv`, `build`, ...) into unsnapshotted BTRFS subvolumes, so your snapshot tool (Snapper, Timeshift, btrbk...) skips them instead of wasting space and time on things you'll regenerate anyway.

**Requires Linux with BTRFS.** GhostVolumes exits cleanly with a clear message on any other platform.

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

That's the whole setup. **Don't** add `eval "$(ghostvolumes shell-init bash)"` (or `zsh`) to your shell rc file — see the [FAQ](FAQ.md#why-not-just-export-ld_preload-globally) for why. Nothing converts automatically after this step; see the [FAQ](FAQ.md) for the recommended workflow.

## How it works

Two commands, plus an explicit decision record in between:

- **`ghostvolumes intercept -- <cmd>`** runs `<cmd>` with the shim active for that command only. It intercepts `mkdir`/`mkdirat` and converts a directory into a subvolume — but only if a `+` decision is already recorded for it.
- **`ghostvolumes convert <path> [--create <relative-path>]...`** — `<path>` is the project (a decision-file/project-roots boundary); it's never itself converted. If it isn't already covered by a registered project, `convert` asks once upfront before touching anything (see [Projects can't nest](#the-project-roots-list) for the full decision tree) — declining aborts the whole command. Every directory under `<path>` matching a watched name gets resolved automatically, skipping anything matching a configured ignore pattern (see below); `--create <relative-path>` (repeatable) additionally names a specific target directly, bypassing the watched-name check. For each candidate: "yes"/"all" converts (creating fresh or migrating in place) and records a `+` decision; "no" converts nothing and records a `-` instead. Run non-interactively (no TTY), it converts nothing and leaves a pending `?` marker instead, same as `intercept` below. Set `GHOSTVOLUMES_DEBUG=debug` to see why each candidate resolved the way it did. `--dry-run` prints what a real run would do instead — `would register: ...`, `would create/convert: ...`, `undecided: ... (skipped — dry run)`, `would ask to override the '-' decision for ...` — without prompting, or touching the filesystem, the decision file, or the project-roots list at all.
- **Decision files** (`.ghostvolumes-decisions`, one per directory, gitignore-style) record what's approved (`+`), denied (`-`), or still pending (`?`) — `#` is reserved for human comments and never written or touched by the tool. `intercept` never prompts — it runs inside arbitrary subprocess trees with no guaranteed terminal — so an undecided directory is skipped, and a `?`-prefixed marker is appended for later review. `convert` is the one place prompting happens when it can, since it's a deliberate, explicit CLI invocation; when it later resolves a candidate that already has a pending marker, that same line becomes the real decision instead of leaving both around.

See [design.md](design.md) for the full rationale behind this model.

## Commands

| Command | What it does |
|---|---|
| `ghostvolumes roots scan [--save]` | Detect BTRFS snapshot-managed roots |
| `ghostvolumes roots list` | List every configured root and its effective watch list |
| `ghostvolumes reload` | Rebuild the runtime cache after hand-editing `roots.d` |
| `ghostvolumes discover [PATH] [--max-depth N] [--save]` | Find subvolumes that already exist and suggest decision-file lines |
| `ghostvolumes convert <path> [--max-depth N] [--create <relative-path>]... [--dry-run]` | Register `<path>` as a project (asks if not already), then recursively resolve subvolume candidates under it |
| `ghostvolumes projects list` | List registered project roots, flagging any that no longer exist |
| `ghostvolumes projects register <path>` | Register a project root (usually automatic via `convert`) |
| `ghostvolumes projects unregister [path]` | Remove a project root; with no path, scan and interactively prune stale ones |
| `ghostvolumes intercept -- <cmd>` | Run `<cmd>` with the shim active, converting anything with a recorded `+` decision |
| `ghostvolumes init` | Install the shim and default config (idempotent, safe to re-run) |
| `ghostvolumes shell-init <bash\|zsh>` | Print the `LD_PRELOAD` value `intercept` uses (diagnostic only) |

## Configuration

Global config lives under `~/.config/ghostvolumes/roots.d/`:

```
roots.d/00-auto.toml     # written by `roots scan --save` — regenerated, don't hand-edit
roots.d/00-defaults.toml # ships with the package: default-watches = node_modules, target, .venv, .cache, build
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
ignore patterns instead live in their own `.ghostvolumes-ignore` file
(see below), decentralized rather than merged through `roots.d`.

### Ignoring directories entirely

`convert`'s and `discover`'s walks never even check an ignored
directory for a watched-name match, let alone descend into it — same
pattern grammar a decision file uses (bare `name`, anchored `/name`,
`/a/b/**/name`), but no `+`/`-`/`?` prefix, since there's nothing to
decide here, only whether to walk in at all. Three tiers, unioned
(matching *any* skips):

| Tier | Where |
|---|---|
| Global | `default-ignore` in `roots.d` |
| Volume root | `.ghostvolumes-ignore` at a `roots.d`-configured root's own path |
| Project root | `.ghostvolumes-ignore` at a registered project's own path |

Unlike decision files, a `.ghostvolumes-ignore` file exists *only* at
that one boundary location — it's never walked up through every
intermediate directory — though a `**` pattern inside it can still
reach arbitrary depth from there, the same way a single `.gitignore`
at a repo root reaches deep paths. `discover` (which isn't tied to any
one registered project) only honors the global tier.

### Decision files

Per-project, committed to the repo they live in — one `.ghostvolumes-decisions` file per directory:

```gitignore
# .ghostvolumes-decisions at a project root
+ node_modules                 # matches this name at any depth
+ /dist                        # anchored: this exact location only
+ /packages/*/**/node_modules  # anchored prefix, arbitrary depth after it
- vendor                       # never convert, at any depth
? /build/should-review-this    # pending: the shim (or convert, run non-interactively) noted this, not yet a decision
# a real comment, for humans only - never touched by any of the above
```

A later real decision for the *same* pattern replaces a `?` line in
place rather than leaving both around — answering "no"/"yes" for
`/build/should-review-this` above would turn that exact line into `-
/build/should-review-this` or `+ /build/should-review-this`, not add a
second line underneath it. `#` is the one prefix reserved for humans;
nothing in this tool ever writes or rewrites a `#` line.

| Pattern | Meaning |
|---|---|
| `name` | Any depth under this file's directory, by final path component |
| `/name` | Anchored: exact location only |
| `/a/b/**/name` | Anchored prefix, arbitrary depth after it |

Resolution walking up from a candidate: the closest enclosing file with a matching pattern wins; within one file, the last matching line wins.

### The project-roots list

A plain-text file (`project-roots.list` under the XDG data directory, one path per line) telling the shim where to stop walking up when resolving decisions. `convert` registers this automatically; `ghostvolumes projects register <path>` sets it up by hand ahead of time if needed.

**Projects can't nest.** At most one registered project can ever cover a given path — decision (and ignore) files already self-distribute via their own closest-file-wins walk-up, so a hierarchy of registered projects wouldn't buy anything beyond a single, correct stopping boundary. Two projects that are path-ancestor/descendant of each other but sit on *different* BTRFS volumes are treated as unrelated, not nested. Before registering a new project, `convert`/`decide` check:

- Already covered by an existing, same-volume project? No-op — that project's decisions already apply.
- Would registering it *nest over* an already-registered, same-volume descendant project? Warns and asks whether to unregister the descendant(s) and register the new, broader project instead (default: no).
- A decision file exists at some ancestor with nothing registered covering it (a parent registration possibly forgotten)? Warns and asks whether to continue and register the narrower path anyway (default: no).
- Otherwise, the usual "Register `<path>` as a project? [Y/n]" ask.

A missing TTY at any of these aborts rather than guessing.

This is genuine, persistent user data (unlike the disposable, regenerate-anytime `compiled.tsv`), so backing it up or syncing it across machines (a dotfile manager, a disk migration) is fine. Just don't hand-edit it directly — use `ghostvolumes projects register`/`unregister` instead, so a live edit never races the shim's or CLI's own reads and writes of it. Run `ghostvolumes projects unregister` (no path) any time to interactively prune entries that no longer exist, including ones that arrived already-stale via a synced/copied-in list. `ghostvolumes projects list` shows what's currently registered.

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

See [design.md](design.md) for the reasoning behind these tradeoffs, and [FAQ.md](FAQ.md) for common workflow questions.

## License

MIT OR Apache-2.0
