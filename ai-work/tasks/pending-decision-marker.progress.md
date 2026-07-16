# Progress: Replace `#`-comment pending markers with a toggleable `?` marker

## Step 1 — shim/decision_core.rs: rename + toggle/remove functions
**Status**: done
**Date**: 2026-07-16
### What was done
`pending_comment_line`/`needs_pending_comment` renamed to
`pending_marker_line`/`needs_pending_marker` (now `?`-prefixed, not
`#`). Added `toggle_or_replace_pending` (in-place line swap, falls back
to append if the marker isn't found) and `remove_pending` (drops an
exact-match marker line, no-op otherwise). 8 unit tests covering both
new functions plus the renamed pair.
### Deviations from plan
None.
### Issues found / fixed
None yet — `build.rs`'s standalone shim compile immediately caught the
now-stale `shim/preload.rs` call sites (expected, fixed in step 2).

## Step 2 — shim/preload.rs: use new marker + decisions lock
**Status**: done
**Date**: 2026-07-16
### What was done
`append_pending_comment` renamed to `append_pending_marker`; now takes
a non-blocking `locks/decisions/<boundary>.lock` (via the existing
`lock_core::boundary_lock_path`/`open_lock_file`, same non-blocking
posture as `try_create_subvolume`'s own lock) before its read-check-
append, and uses the renamed `pending_marker_line`/`needs_pending_marker`.
`tests/shim_ld_preload.rs`'s pending-comment test updated to expect
`"? /node_modules\n"`.
### Deviations from plan
None.
### Issues found / fixed
None — `build.rs`'s standalone shim compile passed cleanly this time;
remaining errors are in `src/convert.rs` (step 3).

## Step 3 — src/convert.rs: toggle/remove logic + decisions lock
**Status**: done
**Date**: 2026-07-16
### What was done
Added `lock_decisions` (blocking, `locks/decisions/<boundary>.lock`,
mirrors `materialize`'s subvolume-creation lock but a separate
namespace). `append_pending_comment` renamed to `append_pending_marker`,
now fallible (`anyhow::Result`) and lock-protected. New
`record_decision` handles both cases: same anchored pattern (y/deny) →
`toggle_or_replace_pending`; different broader pattern (a) →
`remove_pending` the anchored marker then append the broader line
separately — one atomic `write_atomically` rewrite either way, under
the lock. Removed the now-fully-superseded plain `append_decision`
(dead code, no remaining call sites). Added 3 new tests exercising the
exact reported scenario (pending marker present, then y/n/a) plus
fixed the two existing non-interactive tests' `#` → `?` expectations.
### Deviations from plan
None.
### Issues found / fixed
None - all 34 convert:: tests green on first full run after the edit.

## Step 4 — README.md update
**Status**: done
**Date**: 2026-07-16
### What was done
Decision-file example updated to show `? /build/should-review-this`
(pending) alongside a genuine `#` human comment, with a short
paragraph on the in-place-toggle behavior and `#` being reserved for
humans. "How it works" section's two bullet points updated from `#` to
`?` throughout.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 5 — Tests + live cross-check
**Status**: done
**Date**: 2026-07-16
### What was done
3 new convert:: tests reproducing the exact reported scenario (pending
marker present, then y/n/a) - see step 3. Two live cross-checks against
the real compiled shim `.so`: (1) shim writes `? /node_modules`, then
non-interactive `convert` against the identical candidate doesn't
duplicate it - confirms dedup holds across the two independently-
compiled subsystems sharing the same lock/file. (2) (from the prior
plain-pending-marker work) byte-for-byte identical output already
established the shared mechanism; this step re-confirms it still holds
after the toggle/lock changes.
### Deviations from plan
Did not attempt a scripted real-concurrency test (two processes racing
on the same decision file) - reliably forcing that race from a shell
script is impractical; the lock reuses `lock_core.rs`'s
already-tested `open_lock_file`/`boundary_lock_path` machinery
(`a_held_exclusive_lock_blocks_a_second_try_lock` etc. already cover
the primitive itself), so this is judged adequate rather than a gap.
### Issues found / fixed
None.

## Step 6 — fmt + clippy + full test pass
**Status**: done
**Date**: 2026-07-16
### What was done
`rustfmt` + `cargo clippy --all-targets` clean, full `cargo test` green
(201 lib tests + all integration suites, 241 total).
### Deviations from plan
No `CHANGELOG.md` entry or `Cargo.toml` bump here, same reasoning as
`root-watch-config.progress.md`'s step 9 - both happen at actual
release/tag time on `main`, not on a feature branch, per the
auto-version-derivation work earlier this session.
### Issues found / fixed
None.
