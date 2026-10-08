// Every on-disk filename/directory name, in one place (the shared core
// list lives in `filenames_core.rs`, also `include!`d by integration tests).
//
// Plain `//` comments, not `//!`: integration tests under `tests/` need
// to `include!("../src/filenames.rs")` mid-file, which requires this to
// not be a module-level doc comment.

include!("filenames_core.rs");

/// What versions before the snapshot-prune redesign installed into the
/// data dir as the LD_PRELOAD shim; `init` removes a leftover copy.
pub const LEGACY_SHIM_FILE_NAME: &str = "libghostvolumes_shim.so";

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
/// sequence.
#[allow(dead_code)]
pub const RELOAD_LOCK_FILE_NAME: &str = "reload.lock";

/// Guards `projects register`/`unregister`'s read-modify-write sequence
/// on the project-roots list (§5) against concurrent edits.
#[allow(dead_code)]
pub const PROJECT_ROOTS_LOCK_FILE_NAME: &str = "project-roots.lock";

/// Per-VCS settings for `prune`/`vcs-manifest`/`vcs-health`, directly in
/// the config dir (not a `roots.d` drop-in: those treat unknown tables as
/// roots).
pub const VCS_CONFIG_FILE_NAME: &str = "vcs.toml";

/// New `+` rules found by `prune --since`, appended for the user to read
/// and clear; in the state dir (`~/.local/state/ghostvolumes`), next to
/// the timer script's lock files.
pub const EVENTS_LOG_FILE_NAME: &str = "events.log";
