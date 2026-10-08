// Core on-disk filenames, as a flat list (included by `src/filenames.rs`
// and by integration tests, which have no lib target to import from).

/// The compiled runtime cache (§8.0) - tab-separated `(prefix, name)`
/// rows, written by `ghostvolumes reload`.
pub const COMPILED_CACHE_FILE_NAME: &str = "compiled.tsv";

/// Decision file name (§1) - one per directory, gitignore-style. Not
/// user-configurable.
pub const DECISION_FILE_NAME: &str = ".ghostvolumes-decisions";

/// The project-roots list (§3) - plain-text, one path per line, giving
/// the decision-file walk-up a narrower stopping boundary than the
/// broader `roots.d` entries alone. Mutate it live via `ghostvolumes
/// projects register`/`unregister`, not by hand-editing.
pub const PROJECT_ROOTS_FILE_NAME: &str = "project-roots.list";

/// Per-project-boundary advisory lock files live under this
/// subdirectory of the data dir (§2/§6).
#[allow(dead_code)]
pub const LOCKS_DIR: &str = "locks";
