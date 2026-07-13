// Every on-disk filename/directory name, in one place — named once and
// imported at every call site, rather than repeated as hardcoded
// literals that can silently drift apart (a typo in one spot, the
// correct spelling everywhere else). Shim-shared names (the compiled
// cache, decision files, the project-roots list, the shim's own debug
// log) are pulled in from `shim/filenames_core.rs`, the same file the
// shim itself `mod`-includes — everything below that `include!` is
// CLI-only, since the shim never parses TOML config or references its
// own compiled filename.
//
// No `path_in()`-style helper functions anywhere here — every consumer
// does `dir.join(THE_CONSTANT)` directly, the same way for every name.
//
// Plain `//` comments, not `//!`: `//!` only parses when this file is
// truly the start of a module (`mod filenames;` in `main.rs`), and
// integration tests under `tests/` need to `include!("../src/filenames.rs")`
// this same file mid-file to reuse these constants without their own
// hand-kept copies (they have no `[lib]` target to `use ghostvolumes::...`
// from instead).

include!("../shim/filenames_core.rs");

/// The compiled LD_PRELOAD shim's on-disk filename — deliberately not
/// a generic `preload.so` some other tool could also be using: this
/// exact name is what shows up in `LD_PRELOAD`, `ps`, `/proc/*/maps`,
/// and `preload_guard`'s own refusal message, so it needs to be
/// identifiable at a glance. Defined once in `build.rs` (the actual
/// authority — it's what writes the file) and threaded through here
/// via `cargo:rustc-env`, so this and `init.rs`'s `include_bytes!`
/// path both stay in sync with it automatically rather than by
/// hand-kept comments — a renamed/removed `build.rs` constant fails
/// the *compile*, not silently drifts. Kept CLI-only rather than
/// folded into `filenames_core.rs`: that env var only exists when
/// Cargo compiles the main crate, and would fail to resolve for the
/// shim's own standalone `rustc` build if this constant were
/// `mod`-included there too.
pub const SHIM_FILE_NAME: &str = env!("GHOSTVOLUMES_SHIM_FILE_NAME");

/// `roots.d`/`watched.d` config subdirectories (§2).
pub const ROOTS_D_DIR: &str = "roots.d";
pub const WATCHED_D_DIR: &str = "watched.d";

/// `scan --save`'s auto-generated roots file, within `ROOTS_D_DIR`.
pub const AUTO_ROOTS_FILE_NAME: &str = "00-auto.toml";

/// `init`'s default-watched-names skeleton, within `WATCHED_D_DIR`.
pub const DEFAULT_WATCHED_FILE_NAME: &str = "00-defaults.toml";
