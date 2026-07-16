// GhostVolumes LD_PRELOAD shim (§5). Compiled standalone by bare
// `rustc --edition 2021 --crate-type cdylib` from `build.rs` (Step
// 12c) - never through `cargo build`, so no crates.io crate can be
// linked. `mod`-includes the dependency-free logic shared with the
// main CLI; hand-declares `extern "C"` only for the handful of things
// with no `std` equivalent: `dlsym` (RTLD_NEXT symbol resolution) and
// the exported `mkdir`/`mkdirat` replacement symbols themselves.
// Everything else (reading compiled.tsv, running `git`, `stat`-ing
// paths, getting the cwd) uses plain `std` - see plan §8.1 for why
// that's fine even though external crates aren't ("dependency-free"
// means no crates.io crates, not no std).

mod btrfs_core;
mod cache_core;
mod decision_core;
mod filenames_core;
mod lock_core;
mod project_roots_core;
mod xdg_core;

use std::ffi::CStr;
use std::io::Write;
use std::os::raw::{c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

unsafe extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

const RTLD_NEXT: *mut c_void = -1isize as *mut c_void;
const AT_FDCWD: c_int = -100;

type MkdirFn = unsafe extern "C" fn(*const c_char, u32) -> c_int;
type MkdiratFn = unsafe extern "C" fn(c_int, *const c_char, u32) -> c_int;

static CACHE_ROWS: OnceLock<Vec<(String, String)>> = OnceLock::new();
static PROJECT_ROOTS: OnceLock<Vec<String>> = OnceLock::new();
static REAL_MKDIR: OnceLock<usize> = OnceLock::new();
static REAL_MKDIRAT: OnceLock<usize> = OnceLock::new();
static LOG_CTX: OnceLock<LogContext> = OnceLock::new();

fn real_mkdir() -> MkdirFn {
    let ptr = *REAL_MKDIR.get_or_init(|| unsafe { dlsym(RTLD_NEXT, c"mkdir".as_ptr()) as usize });
    unsafe { std::mem::transmute::<usize, MkdirFn>(ptr) }
}

fn real_mkdirat() -> MkdiratFn {
    let ptr =
        *REAL_MKDIRAT.get_or_init(|| unsafe { dlsym(RTLD_NEXT, c"mkdirat".as_ptr()) as usize });
    unsafe { std::mem::transmute::<usize, MkdiratFn>(ptr) }
}

/// `~/.local/share/ghostvolumes` (or `$XDG_DATA_HOME`-relative
/// equivalent) - where `compiled.tsv` lives, and the default location
/// for the debug log if `GHOSTVOLUMES_LOG_FILE` isn't set. `None` if
/// `$HOME` isn't set at all (rare, but must degrade gracefully rather
/// than panic - see `load_cache`'s doc comment).
fn resolved_data_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
    Some(xdg_core::data_dir_from(&home, xdg_data_home.as_deref()))
}

/// Loads `compiled.tsv` (§8.0) from the same path `reload`/`init`
/// write it to - resolved via the shared `xdg_core` logic, since a
/// custom `XDG_DATA_HOME` must be honored identically on both sides.
/// Any failure (no `$HOME`, file missing, unreadable) degrades to an
/// empty cache - "match nothing, pass every call through" - never a
/// panic. A broken LD_PRELOAD shim must never be able to break every
/// command on the system.
fn load_cache() -> Vec<(String, String)> {
    let Some(data_dir) = resolved_data_dir() else {
        return Vec::new();
    };
    match std::fs::read_to_string(data_dir.join(filenames_core::COMPILED_CACHE_FILE_NAME)) {
        Ok(text) => cache_core::parse(&text),
        Err(_) => Vec::new(),
    }
}

/// Loads the registered project-roots list (plan §3) - same
/// never-panic, degrade-to-empty posture as `load_cache`. A missing
/// file just means no project has been explicitly registered yet; the
/// walk-up boundary then falls back to the broader `compiled.tsv` row
/// alone (see `walkup_boundary`).
fn load_project_roots() -> Vec<String> {
    let Some(data_dir) = resolved_data_dir() else {
        return Vec::new();
    };
    match std::fs::read_to_string(data_dir.join(filenames_core::PROJECT_ROOTS_FILE_NAME)) {
        Ok(text) => project_roots_core::parse(&text),
        Err(_) => Vec::new(),
    }
}

/// The decision-file walk-up's stopping boundary for `target` (plan
/// §3): the longest ancestor-or-self prefix among `compiled.tsv`'s own
/// rows *and* the registered project-roots list, whichever is more
/// specific. Reuses `cache_core::longest_matching_prefix` over a
/// combined row set (synthesizing a `(root, "", false)` row per
/// registered path) rather than duplicating its max-by-length logic.
/// `target` already matched a `compiled.tsv` row's prefix by the time
/// this runs (the existing name/root filter in `decide()`), so the
/// `compiled.tsv` half alone always yields at least one candidate;
/// falling back to `target`'s parent below is unreachable in practice,
/// kept only so this never panics if that invariant ever changes.
fn walkup_boundary(rows: &[(String, String)], target: &Path) -> PathBuf {
    let project_roots = PROJECT_ROOTS.get_or_init(load_project_roots);
    let combined = rows
        .iter()
        .cloned()
        .chain(project_roots.iter().map(|root| (root.clone(), String::new())));
    let combined: Vec<(String, String)> = combined.collect();
    match cache_core::longest_matching_prefix(&combined, target) {
        Some(prefix) => PathBuf::from(prefix),
        None => target.parent().unwrap_or(target).to_path_buf(),
    }
}

fn read_decision_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

struct LogContext {
    file: Option<Mutex<std::fs::File>>,
    debug: bool,
}

/// Resolves debug mode and the log file (§8.5) purely from environment
/// variables — `GHOSTVOLUMES_DEBUG` (any value other than empty/`0`
/// enables it) and `GHOSTVOLUMES_LOG_FILE` (defaults to
/// `<data_dir>/shim.log` if unset). No TOML/config file involved: env
/// vars are read live on every process start, so there's no compiled
/// artifact that can go stale relative to a config file, and no
/// hand-rolled file-format parsing to get subtly wrong for a setting
/// this simple. A missing/unopenable log path degrades to "no
/// logging," same never-panic-never-break-the-host-process posture as
/// `load_cache`.
fn load_log_context() -> LogContext {
    let debug = match std::env::var("GHOSTVOLUMES_DEBUG") {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    };

    let log_path = std::env::var("GHOSTVOLUMES_LOG_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| resolved_data_dir().map(|dir| dir.join(filenames_core::SHIM_LOG_FILE_NAME)));

    let file = log_path
        .and_then(|path| std::fs::OpenOptions::new().create(true).append(true).open(path).ok())
        .map(Mutex::new);

    LogContext { file, debug }
}

/// `GHOSTVOLUMES_AUTO_YES` (ai-work/tasks/decision-model.plan.md §4):
/// any value other than empty/`0` bypasses the decision-file lookup
/// entirely and always accepts, restoring the tool's original
/// fully-automatic behavior. Nothing gets recorded when this is set —
/// the env var itself is the standing approval. Read live, same as
/// `GHOSTVOLUMES_DEBUG` - not recommended, since it gives up this
/// design's whole "every subvolume traces back to an explicit
/// decision" transparency guarantee, but available for anyone who
/// wants the old behavior back.
fn auto_yes_enabled() -> bool {
    match std::env::var("GHOSTVOLUMES_AUTO_YES") {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    }
}

fn log_ctx() -> &'static LogContext {
    LOG_CTX.get_or_init(load_log_context)
}

/// Writes one line to the log file, if configured and openable. Never
/// prints to stdout/stderr under any circumstances — the shim runs
/// injected into arbitrary host processes, and writing to their
/// standard streams risks corrupting a TUI or polluting output the
/// host process doesn't expect (§8.5).
fn log_line(msg: &str) {
    let Some(file) = &log_ctx().file else {
        return;
    };
    let Ok(mut file) = file.lock() else {
        return;
    };
    let unix_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    // One write_all call for the whole formatted line, not writeln!'s
    // multi-piece format string (each literal/argument is its own
    // write() syscall) - concurrent shim instances (e.g. many processes
    // under one `make -j`) could otherwise interleave/garble each
    // other's log lines (ai-work/tasks/atomic-file-io.plan.md §3). The
    // `Mutex` above only serializes threads within *this* process; it's
    // this single-write_all that makes a line atomic across processes
    // too, on an O_APPEND-opened file.
    let line = format!("[{unix_secs}] [pid {}] {msg}\n", std::process::id());
    let _ = file.write_all(line.as_bytes());
}

/// Always logged, in both normal and debug mode — reserved for
/// critical/important events (a subvolume actually created, or an
/// unexpected error), per §8.5's "normal mode logs only critical info."
fn log_important(msg: String) {
    log_line(&msg);
}

/// Only logged when debug mode is on — every interception decision and
/// why, for troubleshooting "why did/didn't this become a subvolume."
/// Takes a closure so the (never free) `format!` work only happens
/// when debug mode is actually enabled.
fn log_debug(msg: impl FnOnce() -> String) {
    if log_ctx().debug {
        log_line(&msg());
    }
}

extern "C" fn init_shim() {
    let _ = CACHE_ROWS.set(load_cache());
    let _ = LOG_CTX.set(load_log_context());
}

// One-time config load at process start, before any intercepted mkdir
// call can happen - the same mechanism __attribute__((constructor))
// uses in C. Crates like `ctor` aren't available without dependency
// resolution, so this is hand-written (plan §8.1).
#[used]
#[link_section = ".init_array"]
static INIT_ARRAY: extern "C" fn() = init_shim;

fn cwd() -> PathBuf {
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
}

/// Resolves `mkdirat`'s dirfd to a path via `/proc/self/fd/<fd>` - the
/// only portable way to recover a path from a bare fd without extra
/// crates (plan §5 point 3).
fn dirfd_path(dirfd: c_int) -> PathBuf {
    if dirfd == AT_FDCWD {
        return cwd();
    }
    std::fs::read_link(format!("/proc/self/fd/{dirfd}")).unwrap_or_else(|_| cwd())
}

/// Resolves a raw C string path argument to an absolute `PathBuf`.
/// `base` is a closure (not a value) so the cwd/dirfd lookup - a
/// syscall - only happens when the path actually turns out to be
/// relative, not on every call regardless.
fn resolve_path(raw: *const c_char, base: impl FnOnce() -> PathBuf) -> Option<PathBuf> {
    if raw.is_null() {
        return None;
    }
    let s = unsafe { CStr::from_ptr(raw) }.to_str().ok()?;
    let p = Path::new(s);
    if p.is_absolute() {
        Some(p.to_path_buf())
    } else {
        Some(base().join(p))
    }
}

/// The reason behind an interception decision (ai-work/tasks/decision-model.plan.md
/// §4, evaluated cheapest-first) — reported verbatim in debug logging.
enum Decision {
    NoCacheMatch,
    AlreadySubvolume,
    Denied,
    /// Carries the resolved walk-up boundary (the project-root decision
    /// file's own directory), so `handle_intercept` can append a
    /// pending-comment line there (§4) without recomputing it.
    Undecided(PathBuf),
    /// Carries the same walk-up boundary too - `try_create_subvolume`
    /// needs it to compute which per-boundary lock file coordinates
    /// with `convert`'s directory swap (ai-work/tasks/atomic-file-io.plan.md
    /// §6).
    Accept(PathBuf),
}

/// Does `target` match a watched name under a configured root (one
/// pass over the compiled rows does root-gating and name-matching
/// together, since every row is already root-scoped - see §8.0), is
/// it not already a subvolume (a `stat()`), and what does the nearest
/// decision file along the walk-up (§3, replacing the old git-tracked
/// gate) say about it — a recorded `+`, a recorded `-`, or nothing at
/// all?
fn decide(target: &Path) -> Decision {
    let (Some(parent), Some(name)) = (target.parent(), target.file_name().and_then(|n| n.to_str()))
    else {
        return Decision::NoCacheMatch;
    };
    let rows = CACHE_ROWS.get_or_init(load_cache);
    if !cache_core::names_for(rows, parent).contains(name) {
        return Decision::NoCacheMatch;
    }
    let subvol_check = btrfs_core::is_subvolume(target);
    log_debug(|| format!("{} is_subvolume() raw result -> {subvol_check:?}", target.display()));
    if subvol_check.unwrap_or(false) {
        return Decision::AlreadySubvolume;
    }
    let boundary = walkup_boundary(rows, target);
    if auto_yes_enabled() {
        log_debug(|| format!("{} GHOSTVOLUMES_AUTO_YES set -> bypassing decision lookup", target.display()));
        return Decision::Accept(boundary);
    }
    match decision_core::resolve(target, &boundary, filenames_core::DECISION_FILE_NAME, read_decision_file) {
        Some(true) => Decision::Accept(boundary),
        Some(false) => Decision::Denied,
        None => Decision::Undecided(boundary),
    }
}

/// Appends a `# <pattern>` pending-comment line (§4) to the project's
/// top-level decision file at `boundary`, noting `target` as an
/// undecided candidate — best-effort deduplicated against the file's
/// current content, so repeated builds hitting the same undecided
/// candidate don't pile up duplicate lines. Silently does nothing if
/// `target` somehow isn't under `boundary`, or if the file can't be
/// read/opened — never a hard failure path, same posture as logging.
fn append_pending_comment(boundary: &Path, target: &Path) {
    let Some(pattern) = decision_core::anchored_pattern(boundary, target) else {
        return;
    };
    let file_path = boundary.join(filenames_core::DECISION_FILE_NAME);
    let existing = std::fs::read_to_string(&file_path).unwrap_or_default();
    if !decision_core::needs_pending_comment(&existing, &pattern) {
        return;
    }
    let line = format!("{}\n", decision_core::pending_comment_line(&pattern));
    if let Ok(mut file) = std::fs::OpenOptions::new().create(true).append(true).open(&file_path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Outcome of `try_create_subvolume` - a plain `bool` can't distinguish
/// "the ioctl itself failed" from "skipped because another process
/// (almost certainly `convert`, mid directory-swap) holds this
/// project's lock right now" (ai-work/tasks/atomic-file-io.plan.md
/// §6), which `handle_intercept`'s logging needs to tell apart.
enum CreateResult {
    Created,
    LockContended,
    Failed,
}

/// Attempts `BTRFS_IOC_SUBVOL_CREATE` for `target`, tolerating
/// `EEXIST` gracefully - real traces show tools retry directory
/// creation bottom-up after an initial `ENOENT` on the leaf, which
/// looks like duplicate `mkdir` calls for the same path (plan §5
/// point 7).
///
/// Guarded by a non-blocking `try_lock()` on `boundary`'s per-project
/// lock file (ai-work/tasks/atomic-file-io.plan.md §6) - coordinates
/// with `convert`'s directory-swap, which takes the same lock
/// (blocking) around its own create/copy/rename sequence for a
/// candidate under this same boundary. Never blocks: this runs inside
/// an intercepted `mkdir`/`mkdirat` call, and a hang here would freeze
/// the host build - on contention (or if the lock can't be
/// established at all, e.g. `$HOME`/`$XDG_DATA_HOME` don't resolve),
/// this skips creating a subvolume for *this* call and falls through
/// to the real syscall, same as any other decline; a later build or an
/// explicit `convert` picks it up.
fn try_create_subvolume(target: &Path, boundary: &Path) -> CreateResult {
    let (Some(parent), Some(name)) = (target.parent(), target.file_name().and_then(|n| n.to_str()))
    else {
        return CreateResult::Failed;
    };
    let Some(data_dir) = resolved_data_dir() else {
        return CreateResult::Failed;
    };
    let lock_path = lock_core::boundary_lock_path(&data_dir.join(filenames_core::LOCKS_DIR), boundary);
    let Ok(lock_file) = lock_core::open_lock_file(&lock_path) else {
        return CreateResult::Failed;
    };
    if lock_file.try_lock().is_err() {
        return CreateResult::LockContended;
    }
    match btrfs_core::create_subvolume(parent, name) {
        Ok(()) => CreateResult::Created,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => CreateResult::Created,
        Err(_) => CreateResult::Failed,
    }
}

/// Runs the full decide-and-maybe-create pipeline for one intercepted
/// call, logging the outcome (§8.5), and reports whether it was
/// handled (`true`) or the caller should fall through to the real
/// syscall (`false`).
fn handle_intercept(syscall: &str, target: &Path) -> bool {
    // Logged before `decide()` even runs, so debug-mode troubleshooting
    // (and tests) can tell "the shim was entered but decided X" apart
    // from "the shim was never entered for this call at all" - the
    // latter happens legitimately whenever the calling program itself
    // resolves the outcome without ever reaching mkdir()/mkdirat() (e.g.
    // some `mkdir` implementations `stat()` an already-existing target
    // and skip the syscall entirely - see plan §8.5 and
    // ai-work/tasks/ci-debug-log-test.plan.md).
    log_debug(|| format!("{syscall} {} -> ENTER", target.display()));
    match decide(target) {
        Decision::Accept(boundary) => match try_create_subvolume(target, &boundary) {
            CreateResult::Created => {
                log_important(format!("{syscall}: created subvolume {}", target.display()));
                log_debug(|| format!("{syscall} {} -> ACCEPT (created subvolume)", target.display()));
                true
            }
            CreateResult::LockContended => {
                log_debug(|| {
                    format!(
                        "{syscall} {} -> SKIP (another process is converting this project right now)",
                        target.display()
                    )
                });
                false
            }
            CreateResult::Failed => {
                log_important(format!(
                    "{syscall}: failed to create subvolume {}, falling back to real {syscall}",
                    target.display()
                ));
                false
            }
        },
        Decision::AlreadySubvolume => {
            log_debug(|| format!("{syscall} {} -> SKIP (already a subvolume)", target.display()));
            false
        }
        Decision::Denied => {
            log_debug(|| format!("{syscall} {} -> SKIP (denied)", target.display()));
            false
        }
        Decision::Undecided(boundary) => {
            // Always logged, not debug-gated (plan §4) - this is the
            // one signal a human has that a decision is waiting to be
            // made, so it can't be silent-by-default the way most
            // decide() outcomes are.
            log_important(format!("{syscall}: undecided, skipping {}", target.display()));
            log_debug(|| format!("{syscall} {} -> SKIP (undecided)", target.display()));
            append_pending_comment(&boundary, target);
            false
        }
        Decision::NoCacheMatch => {
            log_debug(|| format!("{syscall} {} -> SKIP (no cache match)", target.display()));
            false
        }
    }
}

#[no_mangle]
pub extern "C" fn mkdir(path: *const c_char, mode: u32) -> c_int {
    if let Some(target) = resolve_path(path, cwd) {
        if handle_intercept("mkdir", &target) {
            return 0;
        }
    }
    unsafe { real_mkdir()(path, mode) }
}

#[no_mangle]
pub extern "C" fn mkdirat(dirfd: c_int, path: *const c_char, mode: u32) -> c_int {
    if let Some(target) = resolve_path(path, || dirfd_path(dirfd)) {
        if handle_intercept("mkdirat", &target) {
            return 0;
        }
    }
    unsafe { real_mkdirat()(dirfd, path, mode) }
}
