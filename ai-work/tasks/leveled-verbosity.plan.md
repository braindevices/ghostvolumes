# Leveled verbosity, shared between the CLI and the shim

Replaces `convert.rs`'s `is_debug: bool` threaded through every
function signature, and the shim's own `bool debug` field, with a
shared `Verbosity` enum (`Error < Warn < Info < Debug < Trace`,
default `Info`) configured once per process from `GHOSTVOLUMES_DEBUG`.
Closure-argument lazy evaluation (already the shim's own
`log_debug(msg: impl FnOnce() -> String)` idiom) means a trace call's
message is only ever formatted when it will actually be shown — no
macro needed.

**Breaking change, deliberately**: `GHOSTVOLUMES_DEBUG`'s old "any
non-empty, non-`0` value enables debug" convention doesn't extend
sensibly to an ordered scale that goes *below* the default (`error`/
`warn` are non-zero too) — so instead of trying to preserve it, the env
var now takes exactly one of five lowercase strings: `error`, `warn`,
`info`, `debug`, `trace`. Unset, empty, or unrecognized → `Info` (same
never-panic, degrade-to-a-sane-default posture as every other env var
this project reads).

**Sink stays asymmetric, not shared**: the shim must never write to
stdout/stderr under any circumstances (it runs injected into arbitrary
host processes) — that's a hard safety invariant, not a preference, so
its sink logic is untouched (still file-only, `GHOSTVOLUMES_LOG_FILE`
override, defaults to `<data_dir>/shim.log`). The CLI is the only side
that gets a real stderr-vs-file choice, reusing the same
`GHOSTVOLUMES_LOG_FILE` env var (unset → stderr, set → that file).

## Design

- `shim/debug_core.rs` (new, dependency-free, shared): `Verbosity`
  enum (`#[derive(PartialOrd, Ord)]`, declaration order gives the
  severity ordering for free) + `parse_verbosity(&str) -> Option<Verbosity>`
  + `configured_verbosity() -> Verbosity` (reads `GHOSTVOLUMES_DEBUG`).
  Pure parsing/ordering only — no I/O, no sink concept — so it's
  usable as-is by both the CLI and the shim without modification.
- `src/debug.rs` (new, CLI-only): `include!`s `debug_core.rs`, adds its
  own `Sink` (`Stderr | File(Mutex<File>)`) and a `OnceLock`-cached
  `Context { verbosity, sink }`. `pub fn trace(level: Verbosity,
  message: impl FnOnce() -> String)` — the closure only runs if `level
  <= configured verbosity`.
- `src/convert.rs`: drop `debug_enabled`/the old `debug_trace`; call
  sites become `crate::debug::trace(Verbosity::Debug, || format!(...))`.
  `is_debug: bool` disappears from `resolve_candidate`/
  `convert_with_io` entirely.
- `shim/preload.rs`: `LogContext.debug: bool` → `verbosity:
  debug_core::Verbosity`, resolved via `debug_core::configured_verbosity()`
  instead of its own inline bool parsing. `log_debug`/`log_important`
  keep their existing names/call sites (no churn there — nothing in the
  shim needs `Warn`/`Error`/`Trace` distinctions today) but compare
  against the new enum threshold instead of a bool.
- `tests/shim_ld_preload.rs`: update `.env("GHOSTVOLUMES_DEBUG", "1"/"0")`
  to the new string values.
- `README.md`/`design.md`: update the one boolean-convention mention
  each to the new five-value convention.

## Steps

1. `shim/debug_core.rs` + unit tests (parsing every value including
   case-insensitivity and unrecognized/empty → `Info`; ordering).
2. `src/debug.rs` + unit tests (level gating; the closure is never
   invoked when suppressed — a test with a panicking closure at a
   suppressed level proves this; stderr vs. file sink).
3. `src/convert.rs`: swap `debug_trace`/`debug_enabled` for
   `crate::debug::trace`, drop `is_debug` from every signature that
   threads it today.
4. `shim/preload.rs`: swap `LogContext.debug: bool` for `Verbosity`.
5. `tests/shim_ld_preload.rs`: update the four `GHOSTVOLUMES_DEBUG`
   env values to the new convention.
6. `README.md`/`design.md` updates.
7. `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + full
   `cargo test` (including the real-shim integration tests) clean.
8. Commit on `claude-convert-project-model` (already the active
   branch).
