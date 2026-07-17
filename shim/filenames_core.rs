// Every on-disk filename shared between shim and CLI, as a flat list;
// `SHIM_FILE_NAME` is deliberately excluded since it's only available
// via `env!()` in the Cargo build, not the shim's standalone build.
// Dependency-free, shared via `include!` (CLI) / `mod` (shim).

/// The compiled runtime cache (§8.0) - tab-separated `(prefix, name)`
/// rows. Read by the shim, written by `ghostvolumes reload`.
pub const COMPILED_CACHE_FILE_NAME: &str = "compiled.tsv";

/// Decision file name (§1) - one per directory, gitignore-style. Not
/// user-configurable: the same hardcoded name both the shim and CLI
/// look for.
pub const DECISION_FILE_NAME: &str = ".ghostvolumes-decisions";

/// The project-roots list (§3) - plain-text, one path per line, giving
/// the decision-file walk-up a narrower stopping boundary than the
/// broader `roots.d` entries alone. Mutate it live via `ghostvolumes
/// projects register`/`unregister`, not by hand-editing.
pub const PROJECT_ROOTS_FILE_NAME: &str = "project-roots.list";

/// The shim's own debug log (§8.5) - shim-only (the CLI never reads or
/// writes it), kept here so every on-disk filename is in one place.
#[allow(dead_code)]
pub const SHIM_LOG_FILE_NAME: &str = "shim.log";

/// Per-project-boundary advisory lock files live under this
/// subdirectory of the data dir (§2/§6) - shared here since both the
/// shim and CLI need to compute the same lock path for a boundary.
#[allow(dead_code)]
pub const LOCKS_DIR: &str = "locks";
