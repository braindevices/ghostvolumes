# GhostVolumes

Automatically isolates volatile build artifacts (`node_modules`, `target`, `.venv`, `build`, etc.) into unsnapshotted BTRFS subvolumes, so your snapshot tool (Snapper, Timeshift, btrbk...) doesn't waste space and time snapshotting things you'll regenerate anyway. Zero sudo at runtime, effectively zero overhead for the common case.

**Requires Linux with BTRFS.** GhostVolumes will refuse to do anything useful on other platforms.

## Install

```bash
cargo install ghostvolumes
ghostvolumes init          # compiles and installs the LD_PRELOAD shim, writes default config
```

Add to your shell rc file:

```bash
# ~/.bashrc
eval "$(ghostvolumes shell-init bash)"
```

```zsh
# ~/.zshrc
eval "$(ghostvolumes shell-init zsh)"
```

Then detect your snapshot-managed BTRFS roots:

```bash
ghostvolumes scan --save
```

Restart your shell (or re-source your rc file) and you're set: `node_modules`, `target`, `.venv`, and `build` directories created anywhere under a detected root become real BTRFS subvolumes automatically, invisible to your snapshot tool.

## How it works

- **Reactive (always on):** an `LD_PRELOAD`ed shim intercepts `mkdir`/`mkdirat` system-wide and redirects matching directory creations to `BTRFS_IOC_SUBVOL_CREATE` instead.
- **Proactive (opt-in per project):** a `cd`-hook pre-creates a project's configured directories as empty subvolumes ahead of any build tool running — this is what covers statically-linked binaries the `LD_PRELOAD` shim can't intercept.
- **Git-tracked content is never touched**, anywhere — reactively, proactively, or via `convert`. GhostVolumes isolates *disposable* artifacts; anything git-tracked is by definition not disposable.

## Commands

| Command | What it does |
|---|---|
| `ghostvolumes scan [--save]` | Detect BTRFS snapshot-managed roots |
| `ghostvolumes reload` | Rebuild the runtime cache after hand-editing config |
| `ghostvolumes discover [PATH] [--save]` | Find subvolumes that already exist and suggest config for them |
| `ghostvolumes convert <path>` | Migrate a pre-existing, populated plain directory into a subvolume |
| `ghostvolumes init` | Install the shim, write default config (idempotent, safe to re-run after upgrading) |
| `ghostvolumes shell-init <bash\|zsh>` | Print the shell integration snippet |

## Configuration

Drop-in TOML files under `~/.config/ghostvolumes/`:

```
roots.d/00-auto.toml       # written by `scan --save` — regenerated, don't hand-edit
roots.d/10-local.toml      # hand-edited additions (e.g. Timeshift/btrbk roots scan couldn't find)
watched.d/00-defaults.toml # ships with the package: node_modules, target, .venv, build
watched.d/10-local.toml    # hand-edited global additions
projects.d/local.toml      # per-project opt-in for proactive (pre-emptive) creation
```

Per-project config can also be checked directly into a repo as `.ghostvolumes.toml`:

```toml
watch     = ["dist", ".next"]  # reactive-only names, on top of the global defaults
proactive = ["node_modules"]   # pre-created by the cd-hook; also covered reactively
```

It's picked up automatically the first time you `cd` into that repo.

## Debugging

The shim always logs critical events (a subvolume actually created, or an unexpected error) to `~/.local/share/ghostvolumes/shim.log`. It never prints to stdout/stderr — it runs injected into arbitrary processes, and writing to their standard streams could corrupt a TUI.

For more detail — every interception decision and why (matched/not, already a subvolume, git-tracked) — turn on debug mode for one command:

```bash
GHOSTVOLUMES_DEBUG=1 npm install
```

Or point the log somewhere else with `GHOSTVOLUMES_LOG_FILE=/path/to/log`. To make either persist across a whole shell session, export it in your rc file alongside the `LD_PRELOAD` line `shell-init` already adds. Debug mode logs *every* intercepted `mkdir`/`mkdirat` call system-wide once `LD_PRELOAD` is set globally — expected, not a bug; turn it back off once you're done.

## Upgrading

```bash
cargo install ghostvolumes --force
```

Or, for a one-command update workflow: `cargo install cargo-update` once, then `cargo install-update ghostvolumes` whenever you want to refresh (`cargo install` itself has no built-in update check).

After upgrading, re-run `ghostvolumes init` to install the new version's shim.

## Known limitations

- Statically-linked binaries (e.g. a musl-static build run in a container) issue raw syscalls that bypass the `LD_PRELOAD` shim entirely. The proactive cd-hook neutralizes most of this in practice by pre-creating subvolumes before any build tool runs.
- Non-interactive shells that don't source rc files (CI runners, `bash -c`, some IDE terminals) don't get proactive pre-creation — set `LD_PRELOAD` globally (e.g. via `/etc/environment`) to get at least reactive coverage there.
- Installing via `cargo binstall` (or any other prebuilt-binary channel) is **not supported** — the shim must be compiled on the machine it runs on to match that machine's libc. Use `cargo install`, which always builds from source locally.
