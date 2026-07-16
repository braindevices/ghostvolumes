// Every on-disk filename shared between the shim and the CLI, defined
// once here rather than repeated (or split across whichever format's
// own _core.rs file happens to use it) - a single flat list here is
// easier to audit than checking cache_core.rs, decision_core.rs, and
// project_roots_core.rs separately. No path-building helper functions
// here (or in src/filenames.rs, which adds the CLI-only names below
// this file's content) - every consumer just does
// `dir.join(THE_CONSTANT)` directly, the same way for every name,
// rather than a `path_in()`-style helper for some but not others.
//
// `SHIM_FILE_NAME` (the compiled shim's own on-disk filename) is
// deliberately NOT here, even though it's a filename too: it's defined
// via `env!("GHOSTVOLUMES_SHIM_FILE_NAME")` (see `build.rs`/
// `src/filenames.rs`), and that env var only exists when Cargo
// compiles the main crate - it's never set for the shim's own
// standalone `rustc --crate-type cdylib` build, which would fail to
// resolve it if this file (mod-included into the shim) carried it.
//
// Dependency-free (plain `std` only), shared between the main CLI (via
// `include!`, from `src/filenames.rs`) and the LD_PRELOAD shim (via
// `mod`, from `shim/preload.rs`).
//
// Plain `//` comments, not `//!`/`///`: this file gets spliced
// mid-file into src/filenames.rs via `include!`.

/// The compiled runtime cache (§8.0) - tab-separated `(prefix, name)`
/// rows. Read by the shim, written by `ghostvolumes reload`.
pub const COMPILED_CACHE_FILE_NAME: &str = "compiled.tsv";

/// Decision file name (ai-work/tasks/decision-model.plan.md §1) - one
/// per directory, gitignore-style. Not user-configurable: the same
/// hardcoded name both the shim (`decide()`) and the CLI (`convert`)
/// look for.
pub const DECISION_FILE_NAME: &str = ".ghostvolumes-decisions";

/// The project-roots list (§3) - plain-text, one path per line, giving
/// the decision-file walk-up a narrower stopping boundary than the
/// broader `roots.d` entries alone. `.list`, not `.txt`
/// (ai-work/tasks/atomic-file-io.plan.md §4) - not to say this file is
/// disposable (it's genuine, persistent user data with no other source
/// of truth, unlike `compiled.tsv`), but `.txt` invites hand-editing,
/// which races the shim's/CLI's own atomic reads and writes of it.
/// Mutate it live via `ghostvolumes projects register`/`unregister`;
/// persisting/syncing the file itself as a whole (backup, disk
/// migration, a dotfile manager) is fine.
pub const PROJECT_ROOTS_FILE_NAME: &str = "project-roots.list";

/// The shim's own debug log (§8.5) - shim-only (the CLI never reads or
/// writes it), kept here anyway so every on-disk filename is
/// discoverable in one place rather than most of them plus one
/// exception elsewhere.
#[allow(dead_code)]
pub const SHIM_LOG_FILE_NAME: &str = "shim.log";

/// Per-project-boundary advisory lock files live under this
/// subdirectory of the data dir (ai-work/tasks/atomic-file-io.plan.md
/// §2/§6) - both the shim (creating a subvolume) and the CLI
/// (`convert`'s directory swap) need to compute the same lock path for
/// a given boundary, so this directory name is shared here rather than
/// living only on the CLI side.
#[allow(dead_code)]
pub const LOCKS_DIR: &str = "locks";
