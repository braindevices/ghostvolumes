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
anymore — see "Decision files" below for why, and what replaced it.

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
`btrfs_core.rs` / `xdg_core.rs` are `include!()`d into the main crate
and `mod`-included into the shim — one literal source, not two
hand-synced copies that only an equivalence test would catch drifting
apart.

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

**Config: only the CLI parses TOML; the shim reads a flat, root-keyed
TSV cache (`compiled.tsv`) instead.**
The shim can't pull in `toml`/`serde` at all (no dependency
resolution), and a hand-rolled TOML parser wasn't worth the risk of
getting subtly wrong. Rows are keyed by each configured root (not a
hardcoded `/`), so root-rejection and name-matching collapse into one
prefix scan over the compiled rows — no separate root-list lookup.

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
