// GhostVolumes LD_PRELOAD shim (§5). Compiled standalone by bare `rustc`
// (no cargo, no crates.io crates). Hand-declares `extern "C"` only for
// `dlsym` and the exported `mkdir`/`mkdirat` symbols; everything else uses `std`.

mod btrfs_core;
mod cache_core;
mod debug_core;
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
/// equivalent) - where `compiled.tsv` and the debug log live. `None`
/// if `$HOME` isn't set (must degrade gracefully, not panic).
fn resolved_data_dir() -> Option<PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let xdg_data_home = std::env::var("XDG_DATA_HOME").ok();
    Some(xdg_core::data_dir_from(&home, xdg_data_home.as_deref()))
}

/// Loads `compiled.tsv` (§8.0). Any failure (no `$HOME`, missing or
/// unreadable file) degrades to an empty cache ("match nothing, pass
/// every call through") rather than a panic.
fn load_cache() -> Vec<(String, String)> {
    let Some(data_dir) = resolved_data_dir() else {
        return Vec::new();
    };
    match std::fs::read_to_string(data_dir.join(filenames_core::COMPILED_CACHE_FILE_NAME)) {
        Ok(text) => cache_core::parse(&text),
        Err(_) => Vec::new(),
    }
}

/// Loads the registered project-roots list (plan §3), same
/// degrade-to-empty posture as `load_cache`. Missing file means no
/// project registered yet; falls back to `compiled.tsv` alone.
fn load_project_roots() -> Vec<String> {
    let Some(data_dir) = resolved_data_dir() else {
        return Vec::new();
    };
    match std::fs::read_to_string(data_dir.join(filenames_core::PROJECT_ROOTS_FILE_NAME)) {
        Ok(text) => project_roots_core::parse(&text),
        Err(_) => Vec::new(),
    }
}

/// The decision-file walk-up's stopping boundary for `target` (plan §3):
/// the longest ancestor-or-self prefix among `compiled.tsv`'s rows and
/// the registered project-roots list. Parent-of-target fallback is
/// unreachable in practice; kept only so this never panics.
fn walkup_boundary(rows: &[(String, String)], target: &Path) -> PathBuf {
    let project_roots = PROJECT_ROOTS.get_or_init(load_project_roots);
    let combined = rows.iter().cloned().chain(
        project_roots
            .iter()
            .map(|root| (root.clone(), String::new())),
    );
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
    verbosity: debug_core::Verbosity,
}

/// Resolves verbosity and the log file (§8.5) purely from env vars —
/// `GHOSTVOLUMES_DEBUG` (`error`/`warn`/`info`/`debug`/`trace`, default
/// `info`) and `GHOSTVOLUMES_LOG_FILE` (defaults to `<data_dir>/shim.log`).
/// A missing/unopenable log path degrades to "no logging."
fn load_log_context() -> LogContext {
    let verbosity = debug_core::configured_verbosity();

    let log_path = std::env::var("GHOSTVOLUMES_LOG_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| resolved_data_dir().map(|dir| dir.join(filenames_core::SHIM_LOG_FILE_NAME)));

    let file = log_path
        .and_then(|path| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .ok()
        })
        .map(Mutex::new);

    LogContext { file, verbosity }
}

/// `GHOSTVOLUMES_AUTO_YES` (§4): any value other than empty/`0` bypasses
/// the decision-file lookup entirely and always accepts. Nothing gets
/// recorded when this is set — gives up the transparency guarantee, but
/// available for anyone who wants the old fully-automatic behavior back.
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
/// prints to stdout/stderr — the shim runs injected into arbitrary host
/// processes, and writing to their standard streams risks corrupting a
/// TUI or polluting output the host process doesn't expect (§8.5).
fn log_line(level: debug_core::Verbosity, msg: &str) {
    let Some(file) = &log_ctx().file else {
        return;
    };
    let Ok(mut file) = file.lock() else {
        return;
    };
    // Single write_all call (not writeln!'s multi-piece writes) so a line
    // stays atomic across concurrent shim processes on an O_APPEND file;
    // the Mutex above only serializes threads within this process.
    let line = format!("{}\n", debug_core::format_line(level, msg));
    let _ = file.write_all(line.as_bytes());
}

/// Logged at `Info` verbosity or more — reserved for critical events
/// (a subvolume created, or an unexpected error). On by default.
fn log_important(msg: String) {
    if log_ctx().verbosity >= debug_core::Verbosity::Info {
        log_line(debug_core::Verbosity::Info, &msg);
    }
}

/// Only logged at `Debug` verbosity or more. Takes a closure so the
/// `format!` work only happens when it'll actually be shown.
fn log_debug(msg: impl FnOnce() -> String) {
    if log_ctx().verbosity >= debug_core::Verbosity::Debug {
        log_line(debug_core::Verbosity::Debug, &msg());
    }
}

extern "C" fn init_shim() {
    let _ = CACHE_ROWS.set(load_cache());
    let _ = LOG_CTX.set(load_log_context());
}

// One-time config load at process start, before any intercepted mkdir
// call - same mechanism as C's __attribute__((constructor)), hand-written
// since crate-based helpers like `ctor` aren't available (plan §8.1).
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
/// `base` is a closure so the cwd/dirfd syscall only happens when the
/// path actually turns out to be relative.
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
    /// Resolved walk-up boundary, so `handle_intercept` can append a
    /// pending-comment line there (§4) without recomputing it.
    Undecided(PathBuf),
    /// Resolved walk-up boundary, needed to pick the per-boundary lock
    /// file that coordinates with `convert`'s directory swap (§6).
    Accept(PathBuf),
}

/// Does `target` match a watched name under a configured root, is it
/// not already a subvolume, and what does the nearest decision file
/// along the walk-up (§3) say about it — `+`, `-`, or nothing?
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
    log_debug(|| {
        format!(
            "{} is_subvolume() raw result -> {subvol_check:?}",
            target.display()
        )
    });
    if subvol_check.unwrap_or(false) {
        return Decision::AlreadySubvolume;
    }
    let boundary = walkup_boundary(rows, target);
    if auto_yes_enabled() {
        log_debug(|| {
            format!(
                "{} GHOSTVOLUMES_AUTO_YES set -> bypassing decision lookup",
                target.display()
            )
        });
        return Decision::Accept(boundary);
    }
    match decision_core::resolve(
        target,
        &boundary,
        filenames_core::DECISION_FILE_NAME,
        read_decision_file,
    ) {
        Some(true) => Decision::Accept(boundary),
        Some(false) => Decision::Denied,
        None => Decision::Undecided(boundary),
    }
}

/// Appends a `? <pattern>` pending-marker line (§4), deduplicated
/// against existing content; no-ops silently on any failure. Uses its
/// own non-blocking lock, since `convert` may concurrently rewrite the
/// same file and this must not block inside an intercepted call.
fn append_pending_marker(boundary: &Path, target: &Path) {
    let Some(pattern) = decision_core::anchored_pattern(boundary, target) else {
        return;
    };
    let Some(data_dir) = resolved_data_dir() else {
        return;
    };
    let lock_path = lock_core::boundary_lock_path(
        &data_dir.join(filenames_core::LOCKS_DIR).join("decisions"),
        boundary,
    );
    let Ok(lock_file) = lock_core::open_lock_file(&lock_path) else {
        return;
    };
    if lock_file.try_lock().is_err() {
        return;
    }
    let file_path = boundary.join(filenames_core::DECISION_FILE_NAME);
    let existing = std::fs::read_to_string(&file_path).unwrap_or_default();
    if !decision_core::needs_pending_marker(&existing, &pattern) {
        return;
    }
    let line = format!("{}\n", decision_core::pending_marker_line(&pattern));
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)
    {
        let _ = file.write_all(line.as_bytes());
    }
}

/// Outcome of `try_create_subvolume` - distinguishes an ioctl failure
/// from being skipped because another process (e.g. `convert`) holds
/// this project's lock, for `handle_intercept`'s logging.
enum CreateResult {
    Created,
    LockContended,
    Failed,
}

/// Attempts `BTRFS_IOC_SUBVOL_CREATE` for `target`, tolerating `EEXIST`.
/// Guarded by a non-blocking `try_lock()` on `boundary`'s per-project
/// lock file, coordinating with `convert`'s own lock; must not block
/// (falls through to the real syscall on contention).
fn try_create_subvolume(target: &Path, boundary: &Path) -> CreateResult {
    let (Some(parent), Some(name)) = (target.parent(), target.file_name().and_then(|n| n.to_str()))
    else {
        return CreateResult::Failed;
    };
    let Some(data_dir) = resolved_data_dir() else {
        return CreateResult::Failed;
    };
    let lock_path =
        lock_core::boundary_lock_path(&data_dir.join(filenames_core::LOCKS_DIR), boundary);
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
    // Logged before `decide()` runs, so debug output can tell "entered
    // but decided X" apart from "never entered" (some `mkdir`
    // implementations skip the syscall entirely via a pre-check `stat`).
    log_debug(|| format!("{syscall} {} -> ENTER", target.display()));
    match decide(target) {
        Decision::Accept(boundary) => match try_create_subvolume(target, &boundary) {
            CreateResult::Created => {
                log_important(format!("{syscall}: created subvolume {}", target.display()));
                log_debug(|| {
                    format!(
                        "{syscall} {} -> ACCEPT (created subvolume)",
                        target.display()
                    )
                });
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
            log_debug(|| {
                format!(
                    "{syscall} {} -> SKIP (already a subvolume)",
                    target.display()
                )
            });
            false
        }
        Decision::Denied => {
            log_debug(|| format!("{syscall} {} -> SKIP (denied)", target.display()));
            false
        }
        Decision::Undecided(boundary) => {
            // Always logged, not debug-gated (§4): the one signal a human
            // has that a decision is waiting to be made.
            log_important(format!(
                "{syscall}: undecided, skipping {}",
                target.display()
            ));
            log_debug(|| format!("{syscall} {} -> SKIP (undecided)", target.display()));
            append_pending_marker(&boundary, target);
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
