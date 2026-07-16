# Progress: leveled verbosity, shared between the CLI and the shim

## Step 1-2 — debug_core.rs + src/debug.rs
**Status**: done
**Date**: 2026-07-16
### What was done
`shim/debug_core.rs` (new, dependency-free, shared): `Verbosity`
(`Error < Warn < Info < Debug < Trace` via derived `Ord`),
`parse_verbosity`/`configured_verbosity` (`GHOSTVOLUMES_DEBUG`, unset/
empty/unrecognized → `Info`), `Verbosity::as_str`, and
`format_line(level, message)` (shared timestamp+pid+level head,
`[<unix-seconds>] [pid <pid>] [<LEVEL>] <message>`, no trailing
newline). `src/debug.rs` (new, CLI-only): `include!`s `debug_core.rs`,
adds `Sink` (`Stderr | File`), a `OnceLock`-cached `Context`, and `pub
fn trace(level, message: impl FnOnce() -> String)` — reuses
`GHOSTVOLUMES_LOG_FILE` for the CLI's own optional file redirect.
### Deviations from plan
Added `format_line`/`Verbosity::as_str` (timestamp/pid/level line
head) beyond the original plan scope, per a follow-up request in the
same conversation to give every logged line a consistent, greppable
head — shared between the CLI and the shim rather than duplicated,
since it's pure formatting with no sink-specific concerns. Timestamp
went through a second iteration: unix-seconds first, then replaced
with hand-rolled ISO 8601 UTC + millisecond precision
(`iso8601_utc_millis`/`civil_from_days`, Howard Hinnant's public-domain
days-since-epoch algorithm) per a follow-up asking for human-readable
output — no `chrono`/`time` crate, since the shim can't link any
crates.io dependency at all.
### Issues found / fixed
`src/debug.rs`'s own test module had to be named `trace_tests`, not
`tests` — `debug_core.rs`'s own `mod tests` (spliced in via `include!`)
already claims that name in the same module scope, same reasoning as
`cache.rs`'s `compile_tests`.

## Step 3 — src/convert.rs
**Status**: done
**Date**: 2026-07-16
### What was done
Removed `debug_enabled`/the old `debug_trace` wrapper entirely (per a
follow-up request in the same conversation: the wrapper was a single
pass-through line, not worth keeping) — call sites now call
`trace(Verbosity::Debug, || ...)` directly via a `use
crate::debug::{Verbosity, trace};` import. `is_debug: bool` dropped
from `resolve_candidate`/`convert_with_io` entirely.
### Deviations from plan
See above (wrapper removed rather than kept, per direct user
instruction after the initial implementation).
### Issues found / fixed
None.

## Step 4 — shim/preload.rs
**Status**: done
**Date**: 2026-07-16
### What was done
`LogContext.debug: bool` → `verbosity: debug_core::Verbosity`,
resolved via `debug_core::configured_verbosity()`. `log_line` now takes
a `Verbosity` and renders via `debug_core::format_line` (the old inline
timestamp+pid formatting removed, now shared). `log_important` (`Info`
threshold) / `log_debug` (`Debug` threshold) keep their existing names
and call sites — no churn there, nothing in the shim needs `Warn`/
`Error`/`Trace` distinctions today.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 5-6 — tests/shim_ld_preload.rs, README/design.md
**Status**: done
**Date**: 2026-07-16
### What was done
Updated all `.env("GHOSTVOLUMES_DEBUG", "1"/"0")` occurrences to the
new string convention (`"debug"`/`"info"`); renamed
`ghostvolumes_debug_zero_explicitly_disables_debug_logging` to
`ghostvolumes_debug_info_explicitly_disables_debug_logging` (the
"zero" framing no longer means anything); added
`an_unrecognized_ghostvolumes_debug_value_degrades_to_the_info_default`
covering the explicit design decision to abandon the old on/off
convention rather than try to preserve it. `README.md`: documented the
five levels and the shared line-head format under "Debugging", updated
the `convert` bullet's example. `design.md`: one-line accuracy update
noting the boolean was later extended to the five-level enum without
changing the original no-config-file rationale.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 7 — verification
**Status**: done
**Date**: 2026-07-16
### What was done
`cargo fmt`, `cargo clippy --all-targets --all-features -- -D
warnings` clean. Full `cargo test` green: 248 lib tests (up from 237 —
9 for `format_line`/`iso8601_utc_millis`, including two known-reference
dates and a leap-day/end-of-day boundary), 25 shim integration tests
(up from 24, the new unrecognized-value test). Live smoke test of
`convert --create` with `GHOSTVOLUMES_DEBUG=debug` confirmed the final
line format visually: `[2026-07-16T18:50:01.461Z] [pid 369670]
[DEBUG] <message>`.
### Deviations from plan
None.
### Issues found / fixed
None.
