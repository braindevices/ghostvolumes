//! CLI-side trace logging (`ai-work/tasks/leveled-verbosity.plan.md`).
//! `Verbosity`/parsing fully implemented in `shim/debug_core.rs` (this
//! file just pulls it in verbatim, same as `src/decision.rs`/
//! `src/cache.rs`) since it's dependency-free and shared with the
//! LD_PRELOAD shim — see that file's doc comment for why.
//!
//! Adds the one thing that *isn't* shared: where a trace line goes.
//! Unlike the shim (which must never touch stdout/stderr under any
//! circumstances, since it runs injected into arbitrary host
//! processes), `convert`/`decide` are deliberate, foreground, human-run
//! commands — writing to stderr by default is fine, and
//! `GHOSTVOLUMES_LOG_FILE` (the same env var the shim already uses for
//! its own, unconditional log file) optionally redirects it to a file
//! instead.

include!("../shim/debug_core.rs");

use std::io::Write;
use std::sync::{Mutex, OnceLock};

enum Sink {
    Stderr,
    File(Mutex<std::fs::File>),
}

struct Context {
    verbosity: Verbosity,
    sink: Sink,
}

fn context() -> &'static Context {
    static CONTEXT: OnceLock<Context> = OnceLock::new();
    CONTEXT.get_or_init(|| {
        let sink = match std::env::var("GHOSTVOLUMES_LOG_FILE") {
            Ok(path) if !path.is_empty() => std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .map(Mutex::new)
                .map(Sink::File)
                .unwrap_or(Sink::Stderr),
            _ => Sink::Stderr,
        };
        Context {
            verbosity: configured_verbosity(),
            sink,
        }
    })
}

/// Traces `message()` at `level`, if `level` is at or below the
/// currently configured verbosity (`GHOSTVOLUMES_DEBUG`, resolved once
/// per process — default `Info`). `message` is a closure so it's only
/// ever evaluated when it will actually be shown — no macro needed,
/// the same lazy-evaluation shape the shim's own `log_debug` already
/// uses.
pub fn trace(level: Verbosity, message: impl FnOnce() -> String) {
    let ctx = context();
    if level > ctx.verbosity {
        return;
    }
    let line = format!("{}\n", format_line(level, &message()));
    match &ctx.sink {
        Sink::Stderr => {
            let _ = std::io::stderr().write_all(line.as_bytes());
        }
        Sink::File(file) => {
            if let Ok(mut file) = file.lock() {
                let _ = file.write_all(line.as_bytes());
            }
        }
    }
}

// Named `trace_tests`, not `tests` - `debug_core.rs`'s own `mod tests`
// (spliced in above via `include!`) already claims that name in this
// same module scope, same reasoning as `cache.rs`'s `compile_tests`.
#[cfg(test)]
mod trace_tests {
    use super::*;

    #[test]
    fn a_suppressed_level_never_evaluates_its_message() {
        // `context()` is a process-wide `OnceLock` (matching the
        // shim's own `LOG_CTX`) - this test relies on the ambient test
        // environment not setting GHOSTVOLUMES_DEBUG at all, same as
        // every other test in this suite implicitly relies on a plain
        // environment. Trace is always above the Info default, so this
        // proves both the gating and the laziness together: a panic
        // here would mean the closure got called despite being
        // suppressed.
        trace(Verbosity::Trace, || panic!("must not be evaluated"));
    }
}
