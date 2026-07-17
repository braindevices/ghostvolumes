//! CLI-side trace logging. `Verbosity`/parsing is shared with the shim
//! via `shim/debug_core.rs` (pulled in verbatim below).
//!
//! Adds the one thing that isn't shared: where a trace line goes. Unlike
//! the shim (which must never touch stdout/stderr), CLI commands write
//! to stderr by default, or to `GHOSTVOLUMES_LOG_FILE` if set.

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

/// Traces `message()` at `level` if at or below the configured
/// verbosity (`GHOSTVOLUMES_DEBUG`, default `Info`). `message` is a
/// closure so it's only evaluated when it will actually be shown.
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
        // Relies on GHOSTVOLUMES_DEBUG being unset (default Info).
        // Trace is above that, so a panic here would mean the closure
        // ran despite being suppressed.
        trace(Verbosity::Trace, || panic!("must not be evaluated"));
    }
}
