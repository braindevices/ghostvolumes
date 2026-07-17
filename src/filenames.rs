// Every on-disk filename/directory name, in one place. Shim-shared names
// are pulled in from `shim/filenames_core.rs`; everything below that
// `include!` is CLI-only.
//
// Plain `//` comments, not `//!`: integration tests under `tests/` need
// to `include!("../src/filenames.rs")` mid-file, which requires this to
// not be a module-level doc comment.

include!("../shim/filenames_core.rs");

/// The compiled LD_PRELOAD shim's on-disk filename — deliberately not a
/// generic `preload.so`. Defined in `build.rs`, threaded through via
/// `cargo:rustc-env` so it can't drift from `init.rs`'s copy.
pub const SHIM_FILE_NAME: &str = env!("GHOSTVOLUMES_SHIM_FILE_NAME");

/// Config subdirectory (§2). `watched.d` was folded in — a root's watch
/// list now lives alongside the root itself, see
/// `ai-work/tasks/root-watch-config.plan.md`.
pub const ROOTS_D_DIR: &str = "roots.d";

/// `scan --save`'s auto-generated roots file, within `ROOTS_D_DIR`.
pub const AUTO_ROOTS_FILE_NAME: &str = "00-auto.toml";

/// `init`'s default-watches/default-ignore skeleton, within `ROOTS_D_DIR`.
pub const DEFAULT_WATCHES_FILE_NAME: &str = "00-defaults.toml";

/// `roots enable`/`disable`'s own file, within `ROOTS_D_DIR` — only ever
/// lists roots explicitly disabled via the CLI, never touched by
/// `scan --save` or any hand-edited file.
pub const DISABLED_ROOTS_FILE_NAME: &str = "10-disable.toml";

/// Guards `roots enable`/`disable`'s read-modify-write sequence on
/// `DISABLED_ROOTS_FILE_NAME`, within `ROOTS_D_DIR`.
pub const DISABLED_ROOTS_LOCK_FILE_NAME: &str = "roots-disable.lock";

/// The ignore-pattern file name: same gitignore-style grammar as
/// `DECISION_FILE_NAME` but no `+`/`-`/`?` prefix, and exists only at
/// one boundary location, never walked up through. CLI-only.
pub const IGNORE_FILE_NAME: &str = ".ghostvolumes-ignore";

/// Guards `reload()`/`scan --save`'s whole read-merge-validate-write
/// sequence. CLI-only, since the shim never writes `compiled.tsv`/`roots.d`.
#[allow(dead_code)]
pub const RELOAD_LOCK_FILE_NAME: &str = "reload.lock";

/// Guards `projects register`/`unregister`'s read-modify-write sequence
/// on the project-roots list (§5) — CLI-only, since the shim never
/// writes it. Wired in at plan Step 6.
#[allow(dead_code)]
pub const PROJECT_ROOTS_LOCK_FILE_NAME: &str = "project-roots.lock";
