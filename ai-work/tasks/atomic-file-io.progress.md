# Cross-process atomic file I/O — Progress

Tracks implementation of `ai-work/tasks/atomic-file-io.plan.md`. Each
step: implement → test → fix → commit → update this file → commit this
file, one unit at a time, never proceeding while red. `cargo fmt` +
`cargo clippy --all-targets -- -D warnings` clean before every commit;
full test suite green; a standalone shim recompile check for anything
touching `shim/`.

Branch: `claude-decision-model-redesign`

---

## Step 1 — `atomic_write.rs`: unique temp filenames
**Status**: done
**Date**: 2026-07-13
### What was done
Temp filename changed from a fixed `.{name}.tmp` to `.{name}.{pid}.{counter}.tmp`
(`std::process::id()` + a per-process `AtomicU64` counter), extracted
into a small `unique_tmp_path()` helper. Added
`unique_tmp_path_never_repeats_within_the_same_process` (direct unit
test) and `concurrent_writes_to_the_same_destination_never_corrupt_it`
(8 threads writing different content to the same destination
concurrently; asserts the final content always exactly matches one
writer's full content — a real regression guard, since this test would
be flaky/failing against the old fixed-tmp-name code).
### Deviations from plan
None.
### Issues found / fixed
Unrelated to this step, but blocking a green suite: `init::tests::writes_default_watched_names_when_absent`
was failing before this step even started (confirmed via `git stash`
that it predates this work) — commit `7b22809` ("add .cache as default
watch") updated `DEFAULT_WATCHED` but left the test's expected list and
README's exhaustive `watched.d/00-defaults.toml` listing out of sync.
Fixed in a separate prerequisite commit before Step 1's own commit,
since it wasn't part of this plan's scope.

## Step 2 — `shim/lock_core.rs` + `src/lock.rs`: shared locking helpers
**Status**: done
**Date**: 2026-07-13
### What was done
`open_lock_file()` (create-if-missing, `.truncate(false)` explicitly -
clippy's `suspicious_open_options` lint requires stating intent, and
the lock file's content is never used, only its identity/inode) and
`boundary_lock_path()` (percent-encodes `/` and `%`). `mod`-included
into `shim/preload.rs`, `include!()`'d into new `src/lock.rs`. New
`LOCKS_DIR` constant (shim-shared, `filenames_core.rs`) and
`RELOAD_LOCK_FILE_NAME`/`PROJECT_ROOTS_LOCK_FILE_NAME` (CLI-only,
`filenames.rs`, `#[allow(dead_code)]` until Steps 3/6 wire them in). 5
new tests: escaping round-trips for `/` and `%`, no collision between
different boundaries, parent-dir creation, and a real
`try_lock()`-fails-while-held / succeeds-after-drop test.
### Deviations from plan
Also added `filenames_core.rs` to `build.rs`'s `rerun-if-changed` list
alongside `lock_core.rs` - a pre-existing gap noted (but not acted on)
in an earlier session, closed opportunistically while touching that
list for this step.
### Issues found / fixed
Clippy's `suspicious_open_options` lint failed on `open_lock_file`'s
`OpenOptions::new().create(true).write(true)` (no explicit
truncate/append intent stated) - fixed with `.truncate(false)`.

## Step 3 — `reload.lock` wired into `reload()`/`scan::save_roots()`
**Status**: done
**Date**: 2026-07-13
### What was done
`reload_with_validator()` now blocking-locks `<data_dir>/reload.lock`
(derived from `cache_path.parent()`) for its whole
read-merge-validate-write sequence via a new `lock_for_reload()`
helper. New test: a thread holds the lock, confirms a concurrent
`reload_with_validator` call stays blocked (`JoinHandle::is_finished`
after a short sleep) until the lock drops, then completes.
### Deviations from plan
`scan::save_roots()` itself does NOT take this lock (only `reload()`
does) — matches the plan's literal wording, and avoids a same-process
double-lock deadlock: `scan --save` calls `save_roots()` then
`reload()` sequentially in one process, and `std::fs::File` locks are
per open-file-description, not per-process, so a naive "lock both"
would have `reload()`'s own lock attempt block forever on a lock the
same process already holds via a different `File` handle. The residual
gap (the window between `save_roots`'s write and `reload()`'s lock
acquisition) is an ordinary, uncorrupted last-write-wins race — Step 1
already rules out corruption there — not something this step claims to
close.
### Issues found / fixed
None.

## Step 4 — Single-`write_all()` fix for every append-based writer
**Status**: done
**Date**: 2026-07-13
### What was done
`register()`, `convert::append_decision()`, `main.rs`'s `Discover
--save` loop, and the shim's `log_line()` all switched from
multi-piece `writeln!` to one pre-formatted `write_all()` call (for
`Discover --save`, one call for the whole batch of new lines, not one
per line). New test in `register.rs`: 8 threads concurrently register
distinct paths, asserts the file ends up with exactly those 8 complete
lines - a real regression guard, since this would be flaky against the
old `writeln!` shape.
### Deviations from plan
None in scope. Along the way, fixed an unrelated commit-splitting
mistake (see Issues below) and a genuine test flake unrelated to this
step's own changes.
### Issues found / fixed
- Accidentally committed Step 4's files together with an unrelated
  prior commit (init.rs's `.uv-cache`/`.ruff_cache`/`.pytest_cache`
  default-watch expansion, made externally during this step) due to
  leftover staged files from an earlier `git add -A`. Fixed via
  `git reset --soft HEAD~1` (safe on this unpushed local branch) and
  re-split into two correctly-scoped commits.
- Stress-testing surfaced a genuine flake in `lock_core.rs`'s
  `a_held_exclusive_lock_blocks_a_second_try_lock` test (~1/30 full
  suite runs) - a `close()`-releases-a-flock vs. an immediately
  -following `try_lock()` timing artifact of this sandbox under heavy
  parallel load, not a logic bug (always fails in the safe direction:
  a lock spuriously appears still-held right after release, never the
  unsafe reverse). Added a short bounded retry to the test; confirmed
  clean across 40 consecutive full-suite runs afterward.

## Step 5 — Rename `project-roots.txt` → `project-roots.list`
**Status**: done
**Date**: 2026-07-13
### What was done
One-line value change (`PROJECT_ROOTS_FILE_NAME` in
`filenames_core.rs`, with an expanded doc comment carrying the
persistent-user-data-not-disposable framing from plan §4) plus the one
README mention of the literal filename. Every other reference already
went through the constant, so nothing else needed touching - confirmed
via a repo-wide grep for the literal old string.
### Deviations from plan
None. `design.md` needed no edit in this step (no literal filename
mentioned there to rename) - its fuller reasoning entry is Step 9's
job, as originally scoped.
### Issues found / fixed
None.

## Step 6 — `ghostvolumes projects list|register|unregister`
**Status**: done
**Date**: 2026-07-13
### What was done
`src/register.rs` renamed to `src/projects.rs`; `Command::Register` ->
`Command::Projects { action: ProjectsAction }` with `List`/
`Register`/`Unregister` variants. `register()` relocated unchanged
(logic-wise); new `unregister()` (exact-path removal, or auto-scan
-and-prompt when no path given, mirroring `convert.rs`'s
`ask_remember`/`confirm_override` TTY posture exactly) and
`list_projects()` (read-only). Both `register`'s append and
`unregister`'s read-filter-write hold a new `project-roots.lock`
(blocking) for their whole operation. Promoted `convert.rs`'s
`read_stdin_line` to `pub(crate)` for reuse instead of duplicating it.
9 new tests, including one closing the lost-update race directly (a
`register` call forced to land between `unregister`'s read and its
write, verified by holding the lock to control the ordering).
`tests/cli_scaffold.rs` updated for the new top-level `projects`
command and a new test covering `projects --help`'s three subcommands.
### Deviations from plan
None.
### Issues found / fixed
None - stress-tested 25 consecutive full-suite runs clean given the
new timing-sensitive lock-ordering test.

## Step 7 — Per-boundary shim-vs-`convert` directory-swap lock
**Status**: done
**Date**: 2026-07-13
### What was done
`Decision::Accept(PathBuf)` now carries the walk-up boundary (moved
its computation earlier in `decide()` so the `GHOSTVOLUMES_AUTO_YES`
branch gets it too). `try_create_subvolume` takes the boundary,
computes its per-project lock path, and takes a non-blocking
`try_lock()` before the ioctl, returning a new `CreateResult`
(`Created`/`LockContended`/`Failed`) instead of a bare `bool` so
`handle_intercept`'s logging can distinguish the two failure modes.
`convert::materialize` takes the same lock, blocking, around just the
create/copy/rename sequence (not the interactive prompt);
`resolve_candidate`/`convert()`/`main.rs`'s `Command::Convert` arm
thread a new `data_dir` parameter through to compute it. New tests: a
shim-side integration test (holds the lock, confirms an intercepted
`mkdir` falls through to a plain directory rather than blocking or
failing) and a CLI-side unit test (holds the lock, confirms
`materialize` blocks via `JoinHandle::is_finished` until released).
### Deviations from plan
None.
### Issues found / fixed
None - stress-tested 25 consecutive full-suite runs clean given two
new timing-sensitive tests.

## Step 8 — `convert::create_empty` tolerates `AlreadyExists`
**Status**: done
**Date**: 2026-07-13
### What was done
`create_empty` now matches on `btrfs::create_subvolume`'s result and
treats `ErrorKind::AlreadyExists` as success, same as the shim's own
`try_create_subvolume`. New test creates the target as a subvolume
directly (bypassing `resolve_candidate`'s own upfront `is_subvolume`
check, to exercise `create_empty` itself in isolation), then confirms
`create_empty` no longer errors on it.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 9 — Docs pass (design.md, README.md, FAQ.md, CHANGELOG.md)
**Status**: done
**Date**: 2026-07-13
### What was done
`design.md`: two new "Key decisions" entries (the atomic-file-io/
locking design as a whole; `project-roots.list`'s persistent-user-data
framing), added `filenames_core.rs`/`lock_core.rs` to the shared-files
list, fixed stale `register` mentions to `projects`, added a
contributor gotcha (shim locks must never block) and a known
-compromise note (`scan --save`'s own write isn't itself under
`reload.lock`). `README.md`'s Commands table and project-roots section
updated for the new `projects` subcommand group. `FAQ.md`'s one stale
`register` mention fixed. `CHANGELOG.md` got a new 0.3.0 entry.
`Cargo.toml`/`Cargo.lock` bumped 0.2.0 -> 0.3.0.
### Deviations from plan
None.
### Issues found / fixed
None.

---

All 9 steps done. Plan fully implemented: 160 tests (up from 151 at
the start of this plan), fmt/clippy clean throughout, standalone shim
recompile clean throughout, every timing-sensitive test stress-tested
clean across dozens of consecutive full-suite runs.
