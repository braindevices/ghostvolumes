## configurable ignored pattern
some pattern should never be walked
.git .hg .svn, etc, but we can never exhaust the the list
thus we need a ignore pattern configured: globally, per root and per project
we do not centralize the per root, per project, just put .ghostvolumes-ignore under volume root or under project root

## dry-run mode
only log what is planned

## `convert --decide-only` or just a `decide` subcommand same level of convert
only modify decision file based on sub path and watch list, do not actual convert
use --add <pattern> --deny <pattern> to explicitly add decisions


## if possible avoid calling cp
if possible use builtin relink copy method

## debug_trace
now the interface is annoying we pass the flag all the way through each function.
I prefer to use a global flag to decide
we can use macro, to wrap around the call, or other more rust friendly way.
The idea is per process, we config the verbosity once.
And a more preferable behavior is like:
debug_trace(
    convert_to_string(some_heavy_function()),
    message_level
)
the full convert_to_string(some_heavy_function()), will never evaluation, if message_level < verbosity_level.
the verbosity_level is globally configured.
I used macro do to this trick in c++11
I did something like following:

```rust
// debug.rs

use std::sync::atomic::{AtomicU8, Ordering};

pub static VERBOSITY: AtomicU8 = AtomicU8::new(0);

pub fn enabled(level: u8) -> bool {
    VERBOSITY.load(Ordering::Relaxed) >= level
}

#[macro_export]
macro_rules! debug_trace {
    ($level:expr, $($arg:tt)*) => {
        if $crate::debug::enabled($level) {
            eprintln!($($arg)*);
        }
    };
}
```


```rust
mod debug;

fn main() {
    debug::VERBOSITY.store(2, std::sync::atomic::Ordering::Relaxed);

    debug_trace!(
        2,
        "value = {}",
        convert_to_string(some_heavy_function())
    );
}
```

some_heavy_function() is not executed unless verbosity is at least 2.

Or Option 2: Use the log crate, if it is much nicer.

my idea is, maybe we can use this with preload.rs too.
