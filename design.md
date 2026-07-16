# GhostVolumes — Design Decisions

Why this project is built the way it is. Full detail lives in
`ai-work/tasks/main-plan.md` (original design) and
`ai-work/tasks/decision-model.plan.md` (supersedes main-plan.md's
git-tracked gate, cd-hook, and per-project `.ghostvolumes.toml`/
`projects.d` — read that doc for the current approve/deny model). This
file is the distilled, kept-current version for anyone who doesn't
want to read either whole doc first. When these disagree,
decision-model.plan.md wins for anything it covers, main-plan.md
otherwise — update this file to match.

## What it does

Auto-converts volatile, high-churn directories (`node_modules`,
`target`, build caches, etc.) into their own BTRFS subvolumes, so
snapshot tools (Snapper, etc.) don't waste snapshots snapshotting
disposable build artifacts. One reactive enforcement path — an
`LD_PRELOAD` hook, loaded into a build via
`ghostvolumes intercept -- <cmd>`, that catches `mkdir` wherever it
happens — plus `ghostvolumes convert <path>` for migrating
already-populated directories (or bootstrapping a brand-new project's
first decisions) explicitly. There is no proactive/pre-creation path
anymore — see "No VCS detection anywhere" under Key decisions below
for why, and what replaced it.

## Non-goals (explicit, not just unfinished)

- **Non-Linux, non-BTRFS platforms.** Gated with a clear "only supports
  Linux with BTRFS" message and clean exit (§8.3) — not a compile
  failure, not a silent no-op, but also never going to be supported.
- **Timeshift/btrbk detection.** `scan`'s privileged pass (§3 point 2)
  was designed but never implemented — only Snapper's unprivileged
  `.snapshots`-is-a-subvolume fingerprint exists today.
- **`cargo binstall` / prebuilt release binaries.** Incompatible by
  design, not an oversight — see below.
- **Statically-linked target binaries** (e.g. a musl-static `uv`).
  LD_PRELOAD can't intercept syscalls that bypass libc entirely. Noted
  as an accepted gap; revisit with seccomp-notify only if it's ever
  observed to matter in practice.
- **seccomp sandboxing of the shim itself.** Deferred (main-plan §9
  build order, last item) — not started.

## Key decisions and why

**The shim is compiled by bare `rustc` at the end user's own `cargo
install` time, never shipped precompiled.**
LD_PRELOAD shares the host process's address space and libc — a
libc-family/version mismatch there isn't a clean failure, it's silent
heap corruption (mismatched allocators). `build.rs` shells out to
`rustc --crate-type cdylib` on `shim/preload.rs` during the user's own
build, guaranteeing the shim is always linked against the exact libc
it'll be injected into. This is *why* `cargo binstall` isn't supported:
it skips `build.rs` and would ship a shim built against the CI
machine's libc instead.

**"Dependency-free" shim means no crates.io crates, not no `std`.**
Bare `rustc` has no Cargo.lock, so it can't resolve a registry
dependency — but `std` ships with the toolchain regardless of Cargo.
Only things with no `std` equivalent (`dlsym`, the raw
`BTRFS_IOC_SUBVOL_CREATE` ioctl, the exported `mkdir`/`mkdirat` symbols)
are hand-declared `extern "C"`.

**Shared logic lives once, under `shim/`, spliced into both sides.**
`cache_core.rs` / `decision_core.rs` / `project_roots_core.rs` /
`btrfs_core.rs` / `xdg_core.rs` / `filenames_core.rs` / `lock_core.rs`
are `include!()`d into the main crate and `mod`-included into the
shim — one literal source, not two hand-synced copies that only an
equivalence test would catch drifting apart.

**No VCS detection anywhere — an explicit, recorded human decision is
the only safety net.** The original design's "git-tracked gate"
(`is_git_tracked`, shelling out to `git ls-files`) only ever protected
git repos, leaned on a fragile external process for a
correctness-relevant check, and made the two failure modes
asymmetric: skipping a conversion costs an optimization, wrongly
converting something loses snapshot coverage for its contents (real
data-loss risk on restore). Replaced entirely
(`ai-work/tasks/decision-model.plan.md`) by gitignore-style decision
files (`.ghostvolumes-decisions`, one per directory, `+`/`-` patterns,
closest-enclosing-file-with-a-match wins) that the shim resolves live
on every intercepted call — no compilation, no caching across calls,
just a handful of `stat()`/`open()`s bounded by the nearest registered
project-root boundary. A candidate with no decision anywhere is
**skipped by default**, not converted — the tool's original
fully-automatic default is now opt-in only, via `GHOSTVOLUMES_AUTO_YES`
(documented as not recommended, since it gives up the whole
transparency guarantee). The shim itself never decides anything and
never prompts (it can be injected into arbitrary subprocess trees with
no TTY guarantee) — it only ever appends an inert `# <pattern>` comment
noting an undecided candidate, which a human turns into a real `+`/`-`
line by hand. `ghostvolumes convert <path>` is the one place actual
prompting happens, since it's an explicit, deliberate, human-run CLI
command with no such constraint.

**`shell-init`'s `LD_PRELOAD` export is a diagnostic tool now, not
something to `eval` into an rc file — `intercept` is the sole intended
activation path.** `ld.so` consults `LD_PRELOAD` at `exec()` time,
before any of this crate's own code runs, and there is no way to
un-preload an already-mapped library from inside a running process
afterward. So exporting it globally in a shell rc file means *every*
process that shell spawns inherits it — including every `ghostvolumes`
subcommand itself (`intercept`, `convert`, `projects`, ...), not just
the build command a user actually meant to wrap. Verified directly:
`LD_PRELOAD=/some/path ./a-binary` produces an `ld.so: ... ignored`
line on that binary's own startup, before its `main()` ever runs, when
the path doesn't resolve — proof `ld.so` acts on the *calling* process
too, not only on whatever it later spawns. Two consequences: `intercept`'s
own documented invariant ("the shim only ever loads into the child,
never the parent") silently breaks, since the parent (`ghostvolumes`
itself) now has it too; and `intercept` becomes redundant for its main
job, since every command already gets shim coverage regardless of
wrapping, leaving only its post-run notice as unique value. (Note:
`Command::env("LD_PRELOAD", ...)` on the *child* side is unaffected by
any of this — it replaces the inherited value outright rather than
appending to it, confirmed empirically, so the child process itself
never ends up with a literal duplicated/colon-joined entry, and even a
hypothetical duplicate wouldn't run the shim's constructor twice —
`ld.so` deduplicates identical library paths by realpath/inode.) The
practical fix for whole-session coverage without this downside:
`ghostvolumes intercept -- bash` (or `zsh`) — a deliberate wrapped
subshell, where everything inside genuinely is the "child," rather
than a permanent export on the outer login shell.

**`ghostvolumes` refuses to run at all if its own shim is already in
`LD_PRELOAD` (`preload_guard.rs`) — enforcing the paragraph above
rather than just documenting it.** Checked once, unconditionally, right
after argument parsing and before dispatching to *any* subcommand —
not a warning, a hard refusal.

*Why every subcommand, with no exception for running from inside an
`intercept -- bash` session:* it's tempting to want `ghostvolumes
convert`/`projects`/etc. to still work there, since that session is
already "wrapped." But tracing through the actual designed workflow
turns up no legitimate case for it: `convert` is only ever meant to run
on a brand-new project *before* anything is wrapped (nothing to convert
yet otherwise), and `intercept`'s own "undecided path found" notice only
prints *after* the wrapped command (or, for `intercept -- bash`, the
whole session) exits — by which point the user is already back outside
any wrapper. Managing decisions and being inside a wrapped build shell
are two different activities that never need to overlap; the point of
`intercept -- bash` is specifically for `ghostvolumes` to be invisible
inside it, not reachable inside it. Carving out an exception (e.g. a
companion env var `intercept` sets on its child, marking that session
as "legitimately wrapped") would only reintroduce the same class of
implicit, hard-to-audit coupling this whole redesign has otherwise
avoided, to support a workflow nobody actually has.

*Why match by the shim's filename alone, not its full resolved path:*
a full-path comparison needs `$HOME`/`$XDG_DATA_HOME` to resolve
identically both when the (not-recommended) rc-file export was written
*and* right now — a symlinked `$HOME`, `sudo -E` with a different
effective home, or a container remounting home would each silently
break that equality check even though the exact same file is loaded,
a false negative in precisely the confusing case this guard exists to
catch. Filename-only matching has no such blind spot and doesn't need
`$HOME` to resolve at all — its own failure mode (some *unrelated* file
elsewhere on disk coincidentally named `libghostvolumes_shim.so`) is
negligible by comparison. The compiled shim's filename was deliberately
renamed away from a generic `preload.so` to this distinctive one
specifically so it could be matched this confidently, both here and by
a human skimming `LD_PRELOAD`/`ps`/`/proc/*/maps` output. A safety check
whose only job is catching one specific misconfiguration should fail
loud/often, not silent/rare.

**Config: only the CLI parses TOML; the shim reads a flat, root-keyed
TSV cache (`compiled.tsv`) instead.**
The shim can't pull in `toml`/`serde` at all (no dependency
resolution), and a hand-rolled TOML parser wasn't worth the risk of
getting subtly wrong. Rows are keyed by each configured root (not a
hardcoded `/`), so root-rejection and name-matching collapse into one
prefix scan over the compiled rows — no separate root-list lookup.

**Every write to a shared, machine-managed file goes through an
already-atomic mechanism** (`ai-work/tasks/atomic-file-io.plan.md`) **—
full-file rewrites via a unique-temp-name-then-`rename()` swap, line
appends via a single `write_all()` call, and multi-process
coordination via `std::fs::File::lock()`.**
`write_atomically()`'s temp filename includes the writing process's
PID and a per-process counter, not just the destination's own name —
two concurrent writers to the same destination used to share one temp
path, and the second's open-and-truncating write could land mid-way
through the first's, corrupting the temp file before either renamed (a
torn write, not just last-write-wins). Every append-based writer
(`register`'s append, `convert::append_decision`, the shim's own
`log_line`) builds the whole line into one `String` before a single
`write_all()` call rather than a multi-piece `writeln!` (each
literal/argument piece of which is its own `write()` syscall) —
`O_APPEND` only guarantees a single `write()` call lands atomically,
not a whole logical line assembled from several. `reload()`/
`scan --save` (a `reload.lock`) and `projects register`/`unregister` (a
`project-roots.lock`) each additionally hold a full-operation lock via
`std::fs::File::lock()` — stable since Rust 1.89, so, unlike any
locking crate, usable directly from the dependency-free shim too (no
`extern "C"` declaration needed, unlike almost everything else this
shim hand-declares). The one place that needed a *cross-component*
lock rather than just a same-file one: the shim's subvolume creation
and `convert`'s directory swap must never run against the same project
boundary at once (a build writing into a directory `convert` is mid
-swapping could have its output silently land in `convert`'s
to-be-deleted backup, unrecoverable data loss) — a per-boundary lock
file (path derived by percent-encoding the boundary, so it stays
human-inspectable rather than an opaque hash) that the shim takes
non-blocking (never risk freezing a host build on a lock) and
`convert` takes blocking (an explicit, occasional, human-run command
can afford to wait).

**`project-roots.list` (renamed from `project-roots.txt`) is
persistent user data, not a disposable compiled artifact like
`compiled.tsv`.**
The `.txt` extension invited hand-editing, which races the shim's and
CLI's own atomic reads/writes of it — but unlike `compiled.tsv`
(purely derived from `roots.d`/`watched.d`, trivially regenerated by
`reload`, safe to delete), this file has no other source of truth:
losing it means re-registering every project by hand. It already lives
in the right place for that (`XDG_DATA_HOME`, for persistent user data
that's neither disposable cache nor TOML-format config). Mutate it
live only via `ghostvolumes projects register`/`unregister` — but
persisting or syncing the file itself as a whole (a backup, a disk
migration, a dotfile manager like chezmoi tracking it across machines)
is fine; that's not a live edit racing anything, it's the same as
replacing any other config file at rest. Entries that arrive
already-stale this way (a path that existed on the old machine but not
this one) are exactly what `projects unregister`'s auto-scan-and-prune
mode is for — the same mechanism that handles locally-deleted projects
handles migrated-in-stale ones for free.

**Debug logging: `GHOSTVOLUMES_DEBUG` / `GHOSTVOLUMES_LOG_FILE` env
vars only — no config file.**
An earlier draft routed this through a `settings.toml` → compiled
`shim.conf` pipeline, mirroring `compiled.tsv`'s pattern. Rejected: a
boolean and a path don't need that machinery, and reusing
`compiled.tsv` itself for settings has a real correctness trap — an
empty-string sentinel prefix for a "not a path" row makes
`Path::starts_with("")` match *every* path, corrupting matching for
every intercepted call. Env vars sidestep this entirely and are read
fresh every process start, so there's no compiled artifact to go
stale.

**The shim never writes to stdout/stderr, ever.**
It's injected into arbitrary host processes, including TUIs — writing
to their standard streams risks corrupting output the host never
expects. All logging goes to a file.

**BTRFS specifics are hard kernel conventions, not heuristics:**
inode 256 (`FIRST_FREE_OBJECTID`) identifies a subvolume root; deletion
uses plain `remove_dir_all`/`rmdir` (sufficient since kernel 4.18, no
`user_subvol_rm_allowed` or special ioctl needed); `EEXIST` from
subvolume creation is tolerated as success, since real tool traces
retry directory creation bottom-up after an initial `ENOENT` on the
leaf.

**Testing: one tier, not mocked+real.**
Nearly the whole suite exercises real BTRFS directly (raw ioctls, no
mocking layer) — a mock can't tell "our code does X" from "our code is
wrong but the mock doesn't know," which matters more here than usual
since the shim hand-declares its own syscalls with no library layer to
lean on. `GHOSTVOLUMES_TEST_BTRFS_DIR` lets any contributor point tests
at a real BTRFS-backed path (see main-plan §8.6); `tests/btrfs_loopback.rs`
is a second, opt-in, self-contained layer that gracefully skips (not
fails) wherever mount privilege isn't available.

## Known compromises / accepted gaps

- No `cargo binstall` support (see above) — `cargo install` (build from
  source) is the only supported install path, deliberately.
- `mkdir` interception on an *already-existing* target is best-effort,
  not guaranteed: some `mkdir` implementations (e.g. Ubuntu's newer
  default `coreutils`, the Rust `uutils` reimplementation) `stat()` the
  target first and skip `mkdir()`/`mkdirat()` entirely when it already
  exists — a call the shim can't observe by definition, since it only
  intercepts those two symbols. Harmless: nothing needs to happen for
  an already-existing subvolume anyway. (Full investigation:
  `ai-work/tasks/ci-debug-log-test.plan.md`.)
- **A totally fresh project with no decision file at all gets zero
  benefit from `intercept` on its first build** — every candidate is
  undecided, so the shim can only skip-and-comment, nothing to
  actually convert yet. Accepted as a direct, correct consequence of
  "nothing gets decided without a prior decision existing somewhere,"
  not a gap: `ghostvolumes convert <project-root>` once (or hand-
  authoring decision-file rules ahead of time) is what populates the
  first decisions; `intercept` earns its keep starting with the next
  build, and any subsequent clone/pull of a repo with a committed
  decision file benefits immediately.
- CI's `ubuntu-26.04` and `snapper-interop` legs run
  `continue-on-error: true` (a preview runner image, and an
  as-yet-long-term-unproven interop job, respectively) — not required
  for merges yet.
- `scan::save_roots()`'s own write to `roots.d/00-auto.toml` is not
  itself under `reload.lock` (only `reload()` is) — `scan --save` calls
  `save_roots()` then `reload()` sequentially in one process, and
  `std::fs::File` locks are per open-file-description, not per-process,
  so a naive "lock both" would have `reload()`'s own lock attempt block
  forever on a lock the same process already holds via a different
  handle. The residual window between `save_roots`'s write and
  `reload()`'s lock acquisition is an ordinary, uncorrupted
  last-write-wins race (unique temp filenames already rule out
  corruption there) — accepted, not closed.

## Gotchas for contributors

- **The shim itself never spawns a subprocess** — it only ever reads
  plain files (`compiled.tsv`, the project-roots list, decision files)
  and writes plain files (the log, pending-comment appends). This was
  a deliberate simplification of the original git-tracked gate design
  (which shelled out to `git ls-files`, needing careful
  `LD_PRELOAD`-stripping to avoid recursively reloading the shim into
  its own subprocess) — if any future change ever needs the shim to
  spawn a subprocess again, that same `.env_remove("LD_PRELOAD")`
  discipline would need to come back with it.
- **Never assume a specific `mkdir`/`mkdirat` call pattern from the
  system's `mkdir` binary in tests** — it varies by what `coreutils`
  package is installed (see the accepted gap above). Prefer asserting
  the actual invariant (subvolume state, no double-creation) over
  "the log contains decision X."
- **Never statically link the shim**, even if the main CLI binary is
  built fully static — static linking and LD_PRELOAD are a
  contradiction; the whole point is loading into another process's
  *existing* libc.
- **The shim must never take a blocking lock.** Every lock it touches
  (the per-boundary lock coordinating with `convert`'s directory swap)
  uses non-blocking `try_lock()` and falls through to the real syscall
  on contention — it runs injected into arbitrary host processes, and
  a hang there would freeze the host build, a far worse failure than
  missing one conversion opportunity. Blocking locks (`reload.lock`,
  `project-roots.lock`, `convert`'s own side of the per-boundary lock)
  are only ever acceptable on the CLI side, where a human explicitly
  ran a command and can afford to wait briefly.
