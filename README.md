# GhostVolumes

Automatically isolates volatile build artifacts (`node_modules`, `target`, `.venv`, `build`, etc.) into unsnapshotted BTRFS subvolumes, so your snapshot tool (Snapper, Timeshift, btrbk...) doesn't waste space and time snapshotting things you'll regenerate anyway. Zero sudo at runtime, effectively zero overhead for the common case.

**Requires Linux with BTRFS.** GhostVolumes will refuse to do anything useful on other platforms.

## Install

```bash
cargo install ghostvolumes
ghostvolumes init          # compiles and installs the LD_PRELOAD shim, writes default config
ghostvolumes scan --save   # detect your snapshot-managed BTRFS roots
```

That's the whole one-time setup. **Don't** add `eval "$(ghostvolumes shell-init bash)"` (or `zsh`) to your rc file — despite following the same pattern as `starship`/`zoxide`/`direnv`, that would export `LD_PRELOAD` globally for your whole shell session, which causes real problems here (see "Why not just export `LD_PRELOAD` globally?" below). Nothing happens automatically after this setup — see "Recommended workflow" below for what actually turns matching directories into subvolumes.

## How it works

Two pieces, and an explicit approve/deny record in between them:

- **`ghostvolumes intercept -- <cmd>`** runs `<cmd>` with the `LD_PRELOAD` shim active for that command (and only that command) — it intercepts `mkdir`/`mkdirat` and converts a matching directory into a subvolume instead of a plain one, but **only for directories with a recorded `+` decision**. Everything else is left alone.
- **`ghostvolumes convert <path>`** recursively finds every matching directory under `<path>`, converts it (creating it fresh if it doesn't exist yet, or migrating it in place if it's already a populated plain directory), and interactively asks whether to remember that decision for next time.
- **Decision files** (`.ghostvolumes-decisions`, one per directory, gitignore-style) are the record of what's been approved or denied. `intercept` never prompts for anything — it can't, since it's injected into arbitrary subprocess trees with no guaranteed terminal — so a directory with no decision anywhere is **skipped, not converted**, and a short note about it gets appended as a `#`-prefixed comment to the project's decision file for a human to review later. `convert` is the one place actual prompting happens, since it's a deliberate, explicit CLI invocation.

There is no proactive/pre-creation path (no `cd`-hook) and no VCS-based gate — the decision file is the only safety net, and it works the same regardless of what VCS (if any) a project uses.

### Why not just export `LD_PRELOAD` globally?

`ghostvolumes shell-init <shell>` still exists and still prints a valid `export LD_PRELOAD=...` line — but it's a diagnostic/reference tool now, showing exactly the value `intercept` sets internally, not something meant to go in your rc file. Sourcing it there means every process your shell spawns inherits `LD_PRELOAD`, including every `ghostvolumes` subcommand itself (`intercept`, `convert`, `register`, ...). Two consequences:

- `ld.so` processes `LD_PRELOAD` at `exec()` time, before any of `ghostvolumes`'s own code runs — there's no way to un-preload an already-mapped library from inside the process afterward. So `intercept`'s documented invariant ("the shim only ever loads into the child, never the parent") silently breaks: `ghostvolumes` itself would have the shim loaded into *itself* too, for every subcommand, not just `intercept`.
- `intercept` becomes redundant for its main job — if `LD_PRELOAD` is already global, every command gets shim coverage whether wrapped or not, and the only thing `intercept` still uniquely adds is its post-run "undecided path found" notice.

If you actually want whole-session coverage (every command you run gets the shim, without wrapping each one individually), run `ghostvolumes intercept -- bash` (or `zsh`) to open a deliberate wrapped subshell instead — everything inside that subshell is the "child," so the invariant above holds, and your outer login shell (and every other `ghostvolumes` invocation) stays unaffected.

## Recommended workflow

**Brand new project, nothing built yet:**

```bash
cd ~/projects/my-app
npm install                    # build normally — node_modules etc. are created as plain directories
ghostvolumes convert .         # recursively finds them, asks "remember this?" for each
```

`convert` walks answer three ways per match: **no** (just this once, nothing recorded), **yes, just this path**, or **yes, every match of this name** (scoped to the directory you converted, so it won't silently apply somewhere else you never looked). Answering yes writes a `+`/`-` line to `.ghostvolumes-decisions` at your project root. Commit that file — it's meant to be shared, the same way `.gitignore` is:

```bash
git add .ghostvolumes-decisions && git commit -m "Record subvolume decisions"
```

From your *next* build onward, wrap it with `intercept` and matching directories convert automatically, no prompting:

```bash
rm -rf node_modules && ghostvolumes intercept -- npm install
```

**Cloning a repo that already has decisions committed:** nothing to do — `ghostvolumes intercept -- <your build command>` works from the very first build, since the decisions already exist.

**Pre-authoring decisions before ever building:** hand-write `.ghostvolumes-decisions` yourself (see "Decision files" below for the pattern syntax) — `intercept` benefits immediately, same as the cloned-repo case.

**If `intercept` finds something undecided,** it prints a notice after your command finishes, naming the one covering command to run:

```
ghostvolumes: new undecided path(s) found under /home/user1/projects/my-app — run `ghostvolumes convert /home/user1/projects/my-app` to review them
```

Running that `convert` resolves everything pending under that root in one pass, including anything nested (e.g. a `packages/foo/node_modules` inside a monorepo).

**A directory you never want converted:** write a `- name` (or `- /exact/path`) line to the decision file yourself, or answer accordingly when `convert` asks. `convert` refuses to silently override an existing `-` decision — pointing it directly at a denied path asks for confirmation first.

## Commands

| Command | What it does |
|---|---|
| `ghostvolumes scan [--save]` | Detect BTRFS snapshot-managed roots |
| `ghostvolumes reload` | Rebuild the runtime cache after hand-editing `roots.d`/`watched.d` |
| `ghostvolumes discover [PATH] [--max-depth N] [--save]` | Find subvolumes that already exist and suggest decision-file lines for them |
| `ghostvolumes convert <path> [--max-depth N]` | Recursively find and resolve subvolume candidates under `<path>`, prompting to remember new decisions |
| `ghostvolumes register <path>` | Register a project root, narrowing the decision-file lookup boundary (usually not needed — `convert` does this for you) |
| `ghostvolumes intercept -- <cmd>` | Run `<cmd>` with the shim active for that command, converting anything with a recorded `+` decision |
| `ghostvolumes init` | Install the shim, write default config (idempotent, safe to re-run after upgrading) |
| `ghostvolumes shell-init <bash\|zsh>` | Print the `LD_PRELOAD` value `intercept` uses (diagnostic — not meant to be `eval`'d into your rc file, see above) |

## Configuration

Drop-in TOML files under `~/.config/ghostvolumes/` — global, machine-wide:

```
roots.d/00-auto.toml       # written by `scan --save` — regenerated, don't hand-edit
roots.d/10-local.toml      # hand-edited additions (e.g. Timeshift/btrbk roots scan couldn't find)
watched.d/00-defaults.toml # ships with the package: node_modules, target, .venv, build
watched.d/10-local.toml    # hand-edited global additions
```

### Decision files

Per-project, gitignore-style, and meant to be committed to the repo they live in. One file, `.ghostvolumes-decisions`, per directory:

```
# .ghostvolumes-decisions at a project root
+ node_modules              # unanchored: matches this name at any depth under this directory
+ /dist                     # anchored: exactly this location only
+ /packages/*/**/node_modules  # anchored prefix, arbitrary depth after it (not a real glob — see below)
- vendor                    # never convert, at any depth
# /build/should-review-this # a pending note the shim appended - not yet a real decision
```

Three pattern forms only (not a full `.gitignore` clone — no negation, no character classes):

| Pattern | Meaning |
|---|---|
| `name` | Matches at any depth under this file's directory, by final path component |
| `/name` | Anchored: that exact single location only |
| `/a/b/**/name` | Anchored prefix, arbitrary depth after it |

Resolution, walking up from a candidate toward the project root: the **closest enclosing file with any matching pattern wins**; within one file, the **last matching line wins** (add a narrow override after a broad rule). Nesting a decision file in a subdirectory is a manual, advanced feature — everything `convert`/`intercept` write automatically always goes to the top-level (project-root) file.

A `#`-prefixed line is always a comment, including the pending-notice lines the shim appends for undecided candidates — turn one into a real decision by hand-editing `#` into `+` or `-`.

### The project-roots list

A separate, plain-text file (`project-roots.txt` under the XDG data directory, one path per line) telling the shim where to stop walking up when resolving decisions — this is what keeps the walk-up cheap and precise instead of climbing all the way to a broad, shared `roots.d` entry. You almost never need to touch this yourself: `convert` registers its own resolved project root automatically the first time it records a decision there. `ghostvolumes register <path>` exists for registering one proactively, e.g. before ever running `convert` in a project.

## Debugging

The shim always logs critical events (a subvolume actually created, an undecided candidate skipped, or an unexpected error) to `~/.local/share/ghostvolumes/shim.log`. It never prints to stdout/stderr — it runs injected into arbitrary processes, and writing to their standard streams could corrupt a TUI.

For more detail — every interception decision and why (matched/not, already a subvolume, accepted, denied, undecided) — turn on debug mode for one command:

```bash
GHOSTVOLUMES_DEBUG=1 ghostvolumes intercept -- npm install
```

Or point the log somewhere else with `GHOSTVOLUMES_LOG_FILE=/path/to/log`.

`GHOSTVOLUMES_AUTO_YES=1` bypasses the decision lookup entirely and always converts a matching directory, like an older fully-automatic version of this tool — **not recommended**, since it gives up the whole point of an explicit, reviewable decision trail, but available if you want it.

## Upgrading

```bash
cargo install ghostvolumes --force
```

Or, for a one-command update workflow: `cargo install cargo-update` once, then `cargo install-update ghostvolumes` whenever you want to refresh (`cargo install` itself has no built-in update check).

After upgrading, re-run `ghostvolumes init` to install the new version's shim.

## Known limitations

- **Statically-linked binaries** (e.g. a musl-static build run in a container) issue raw syscalls that bypass the `LD_PRELOAD` shim entirely — an accepted gap, not something `intercept` can work around.
- **A brand-new project with no decisions recorded anywhere gets zero benefit from `intercept` on its first build** — every candidate is undecided, so it can only skip and leave a note. Run `ghostvolumes convert <project-root>` once (or hand-author decisions ahead of time) to populate the first decisions; `intercept` earns its keep from the next build onward.
- Installing via `cargo binstall` (or any other prebuilt-binary channel) is **not supported** — the shim must be compiled on the machine it runs on to match that machine's libc. Use `cargo install`, which always builds from source locally.
