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
ghostvolumes init          # compile + install the LD_PRELOAD shim, write default config
ghostvolumes scan --save   # detect your snapshot-managed BTRFS roots
```

That's the whole setup. **Don't** add `eval "$(ghostvolumes shell-init bash)"` (or `zsh`) to your shell rc file — see the [FAQ](FAQ.md#why-not-just-export-ld_preload-globally) for why. Nothing converts automatically after this step; see the [FAQ](FAQ.md) for the recommended workflow.

## How it works

Two commands, plus an explicit decision record in between:

- **`ghostvolumes intercept -- <cmd>`** runs `<cmd>` with the shim active for that command only. It intercepts `mkdir`/`mkdirat` and converts a directory into a subvolume — but only if a `+` decision is already recorded for it.
- **`ghostvolumes convert <path>`** recursively converts matching directories under `<path>` (creating them fresh or migrating them in place) and asks whether to remember each decision.
- **Decision files** (`.ghostvolumes-decisions`, one per directory, gitignore-style) record what's approved or denied. `intercept` never prompts — it runs inside arbitrary subprocess trees with no guaranteed terminal — so an undecided directory is skipped, and a `#`-prefixed note is appended for later review. `convert` is the one place prompting happens, since it's a deliberate, explicit CLI invocation.

See [design.md](design.md) for the full rationale behind this model.

## Commands

| Command | What it does |
|---|---|
| `ghostvolumes scan [--save]` | Detect BTRFS snapshot-managed roots |
| `ghostvolumes reload` | Rebuild the runtime cache after hand-editing `roots.d`/`watched.d` |
| `ghostvolumes discover [PATH] [--max-depth N] [--save]` | Find subvolumes that already exist and suggest decision-file lines |
| `ghostvolumes convert <path> [--max-depth N]` | Recursively convert subvolume candidates, prompting to remember decisions |
| `ghostvolumes register <path>` | Register a project root (usually automatic via `convert`) |
| `ghostvolumes intercept -- <cmd>` | Run `<cmd>` with the shim active, converting anything with a recorded `+` decision |
| `ghostvolumes init` | Install the shim and default config (idempotent, safe to re-run) |
| `ghostvolumes shell-init <bash\|zsh>` | Print the `LD_PRELOAD` value `intercept` uses (diagnostic only) |

## Configuration

Global config lives under `~/.config/ghostvolumes/`:

```
roots.d/00-auto.toml       # written by `scan --save` — regenerated, don't hand-edit
roots.d/10-local.toml      # hand-edited additions (e.g. roots scan couldn't find)
watched.d/00-defaults.toml # ships with the package: node_modules, target, .venv, build
watched.d/10-local.toml    # hand-edited global additions
```

### Decision files

Per-project, committed to the repo they live in — one `.ghostvolumes-decisions` file per directory:

```gitignore
# .ghostvolumes-decisions at a project root
+ node_modules                 # matches this name at any depth
+ /dist                        # anchored: this exact location only
+ /packages/*/**/node_modules  # anchored prefix, arbitrary depth after it
- vendor                       # never convert, at any depth
# /build/should-review-this    # pending note the shim appended, not yet a decision
```

| Pattern | Meaning |
|---|---|
| `name` | Any depth under this file's directory, by final path component |
| `/name` | Anchored: exact location only |
| `/a/b/**/name` | Anchored prefix, arbitrary depth after it |

Resolution walking up from a candidate: the closest enclosing file with a matching pattern wins; within one file, the last matching line wins.

### The project-roots list

A plain-text file (`project-roots.txt` under the XDG data directory, one path per line) telling the shim where to stop walking up when resolving decisions. `convert` registers this automatically; `ghostvolumes register <path>` sets it up by hand ahead of time if needed.

## Debugging

The shim always logs critical events (a subvolume created, an undecided candidate skipped, an unexpected error) to `~/.local/share/ghostvolumes/shim.log`. It never writes to stdout/stderr, since it runs inside arbitrary host processes.

```bash
GHOSTVOLUMES_DEBUG=1 ghostvolumes intercept -- npm install   # log every decision and why
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
