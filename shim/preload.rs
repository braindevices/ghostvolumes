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
mod git_core;
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

static CACHE_ROWS: OnceLock<Vec<(String, String, bool)>> = OnceLock::new();
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
fn load_cache() -> Vec<(String, String, bool)> {
    let Some(data_dir) = resolved_data_dir() else {
        return Vec::new();
    };
    match std::fs::read_to_string(data_dir.join("compiled.tsv")) {
        Ok(text) => cache_core::parse(&text),
        Err(_) => Vec::new(),
    }
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
    let debug_raw = std::env::var("GHOSTVOLUMES_DEBUG");
    let debug = match &debug_raw {
        Ok(value) => !value.is_empty() && value != "0",
        Err(_) => false,
    };

    let log_path = std::env::var("GHOSTVOLUMES_LOG_FILE")
        .ok()
        .map(PathBuf::from)
        .or_else(|| resolved_data_dir().map(|dir| dir.join("shim.log")));

    let open_result =
        log_path.as_ref().map(|path| std::fs::OpenOptions::new().create(true).append(true).open(path));

    // TEMPORARY diagnostics for the ubuntu-26.04 CI flake investigation
    // (ai-work/tasks/ci-debug-log-test.plan.md): the *intended* log file
    // was observed to receive zero lines at all for one specific
    // invocation, as if this whole function's outcome differed from its
    // neighbors. Recorded unconditionally (not gated on `debug`, since a
    // misread of GHOSTVOLUMES_DEBUG itself is one of the possibilities
    // under test) to a side-channel path, so it survives even when the
    // intended log file couldn't be opened.
    if let Ok(mut diag) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(std::env::temp_dir().join("ghostvolumes-shim-diag.log"))
    {
        let open_summary = match &open_result {
            Some(Ok(_)) => "Some(Ok)".to_string(),
            Some(Err(e)) => format!("Some(Err({e:?}))"),
            None => "None".to_string(),
        };
        let _ = writeln!(
            diag,
            "[pid {}] argv0={:?} GHOSTVOLUMES_DEBUG={debug_raw:?} debug={debug} log_path={log_path:?} open_result={open_summary}",
            std::process::id(),
            std::env::args().next(),
        );
    }

    let file = open_result.and_then(|r| r.ok()).map(Mutex::new);

    LogContext { file, debug }
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
    let _ = writeln!(file, "[{unix_secs}] [pid {}] {msg}", std::process::id());
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

/// The reason behind an interception decision (plan §5 points 4-6,
/// evaluated cheapest-first) — reported verbatim in debug logging.
enum Decision {
    NoCacheMatch,
    AlreadySubvolume,
    GitTracked,
    Accept,
}

/// Does `target` match a watched name under a configured root (one
/// pass over the compiled rows does root-gating and name-matching
/// together, since every row is already root-scoped - see §8.0), is
/// it not already a subvolume (a `stat()`), and is it not git-tracked
/// (the most expensive check - shells out to `git` - so it runs last,
/// only for the rare survivors of the two cheaper checks)?
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
    if git_core::is_git_tracked(target) {
        return Decision::GitTracked;
    }
    Decision::Accept
}

/// Attempts `BTRFS_IOC_SUBVOL_CREATE` for `target`, tolerating
/// `EEXIST` gracefully - real traces show tools retry directory
/// creation bottom-up after an initial `ENOENT` on the leaf, which
/// looks like duplicate `mkdir` calls for the same path (plan §5
/// point 7).
fn try_create_subvolume(target: &Path) -> bool {
    let (Some(parent), Some(name)) = (target.parent(), target.file_name().and_then(|n| n.to_str()))
    else {
        return false;
    };
    match btrfs_core::create_subvolume(parent, name) {
        Ok(()) => true,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => true,
        Err(_) => false,
    }
}

/// Runs the full decide-and-maybe-create pipeline for one intercepted
/// call, logging the outcome (§8.5), and reports whether it was
/// handled (`true`) or the caller should fall through to the real
/// syscall (`false`).
fn handle_intercept(syscall: &str, target: &Path) -> bool {
    match decide(target) {
        Decision::Accept => {
            if try_create_subvolume(target) {
                log_important(format!("{syscall}: created subvolume {}", target.display()));
                log_debug(|| format!("{syscall} {} -> ACCEPT (created subvolume)", target.display()));
                true
            } else {
                log_important(format!(
                    "{syscall}: failed to create subvolume {}, falling back to real {syscall}",
                    target.display()
                ));
                false
            }
        }
        Decision::AlreadySubvolume => {
            log_debug(|| format!("{syscall} {} -> SKIP (already a subvolume)", target.display()));
            false
        }
        Decision::GitTracked => {
            log_debug(|| format!("{syscall} {} -> SKIP (git-tracked)", target.display()));
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
