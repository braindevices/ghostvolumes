//! End-to-end validation of the LD_PRELOAD shim (§5): compiles
//! `shim/preload.rs` via bare `rustc` (exactly how `build.rs` will,
//! Step 12c) and actually `LD_PRELOAD`s it into real `mkdir`/`mkdirat`
//! calls against real BTRFS subvolumes under `btrfs_scratch_dir()`.
//! This is what caught (and confirmed the fix for) several behaviors
//! during manual exploration before this test existed:
//! EEXIST→AlreadyExists mapping, relative-path/dirfd resolution, and
//! the git-tracked gate.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;
use tempfile::TempDir;

fn compiled_shim() -> &'static Path {
    static SHIM: OnceLock<PathBuf> = OnceLock::new();
    SHIM.get_or_init(|| {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let out =
            std::env::temp_dir().join(format!("ghostvolumes-test-shim-{}.so", std::process::id()));
        let status = Command::new("rustc")
            .args([
                "--edition",
                "2021",
                "--crate-type",
                "cdylib",
                "-O",
                "-C",
                "panic=abort",
            ])
            .arg("-o")
            .arg(&out)
            .arg(format!("{manifest_dir}/shim/preload.rs"))
            .status()
            .expect("rustc must be available to compile the shim");
        assert!(status.success(), "shim failed to compile");
        out
    })
}

fn compiled_mkdirat_probe() -> &'static Path {
    static PROBE: OnceLock<PathBuf> = OnceLock::new();
    PROBE.get_or_init(|| {
        let src = std::env::temp_dir().join(format!("mkdirat-probe-{}.rs", std::process::id()));
        std::fs::write(
            &src,
            r#"
            unsafe extern "C" {
                fn open(path: *const i8, flags: i32) -> i32;
                fn mkdirat(dirfd: i32, path: *const i8, mode: u32) -> i32;
            }
            fn main() {
                let dir = std::ffi::CString::new(std::env::args().nth(1).unwrap()).unwrap();
                let name = std::ffi::CString::new(std::env::args().nth(2).unwrap()).unwrap();
                let fd = unsafe { open(dir.as_ptr(), 0o200000) };
                assert!(fd >= 0, "open failed");
                let rc = unsafe { mkdirat(fd, name.as_ptr(), 0o755) };
                std::process::exit(if rc == 0 { 0 } else { 1 });
            }
            "#,
        )
        .unwrap();
        let out = std::env::temp_dir().join(format!("mkdirat-probe-{}", std::process::id()));
        let status = Command::new("rustc")
            .args(["--edition", "2021", "-O"])
            .arg("-o")
            .arg(&out)
            .arg(&src)
            .status()
            .unwrap();
        assert!(status.success());
        out
    })
}

/// Where BTRFS-dependent tests create their scratch subvolumes.
/// Override with `GHOSTVOLUMES_TEST_BTRFS_DIR` if your checkout isn't
/// on a BTRFS filesystem but some other mounted location is. Defaults
/// to `<CARGO_MANIFEST_DIR>/target/ghostvolumes-test-scratch` — the
/// project's own build directory, BTRFS-backed whenever the checkout
/// itself is, rather than hardcoding any particular machine's layout.
/// Kept in sync with the identical helper in `src/test_support.rs`
/// (this file is a separate integration-test binary and can't import
/// that one — the crate has no lib target to link against).
fn btrfs_test_root() -> PathBuf {
    if let Ok(dir) = std::env::var("GHOSTVOLUMES_TEST_BTRFS_DIR") {
        return PathBuf::from(dir);
    }
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ghostvolumes-test-scratch")
}

/// A tempdir under `btrfs_test_root()` instead of the default `/tmp`
/// (`tempfile::tempdir()`), which is very often a non-BTRFS overlay or
/// tmpfs and would silently make BTRFS-dependent tests exercise the
/// wrong filesystem.
fn btrfs_scratch_dir() -> TempDir {
    let parent = btrfs_test_root();
    std::fs::create_dir_all(&parent)
        .unwrap_or_else(|e| panic!("create BTRFS test scratch dir {}: {e}", parent.display()));
    tempfile::tempdir_in(&parent)
        .unwrap_or_else(|e| panic!("create scratch tempdir under {}: {e}", parent.display()))
}

/// A placeholder `$HOME` for tests that need *some* valid `HOME` set
/// (so the shim's `$HOME`-gated code paths don't bail out) but always
/// override `XDG_DATA_HOME` explicitly, so `HOME`'s actual value never
/// affects the test logic itself. Deliberately fake and nonexistent —
/// not the invoking developer's real `$HOME` — so that if some future
/// change ever *did* cause a path to fall back to `$HOME`-derived
/// resolution, the test would fail loudly (open()/write() against a
/// nonexistent directory) instead of silently reading or writing
/// somewhere in the real developer's home directory.
fn fake_home() -> &'static str {
    "/nonexistent-fake-home-for-ghostvolumes-tests"
}

fn write_cache(data_home: &Path, rows: &[(&Path, &str)]) {
    std::fs::create_dir_all(data_home.join("ghostvolumes")).unwrap();
    let mut text = String::new();
    for (prefix, name) in rows {
        text.push_str(&format!("{}\t{name}\n", prefix.display()));
    }
    std::fs::write(data_home.join("ghostvolumes/compiled.tsv"), text).unwrap();
}

fn run_mkdir_with_shim(data_home: &Path, target: &Path) -> std::process::ExitStatus {
    Command::new("mkdir")
        .arg(target)
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home)
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap()
}

fn is_subvolume(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| m.is_dir() && m.ino() == 256)
        .unwrap_or(false)
}

/// Diagnostic-only: dumps `tree`'s view of `root` (with inode numbers, so
/// subvolume roots at inode 256 are visible directly) and `stat`'s view of
/// `target`, both via real external processes independent of anything the
/// shim itself observed. Printed with `eprintln!` so libtest's output
/// capture surfaces it automatically under the failing test's output
/// section, no `--nocapture` required.
fn debug_dump(label: &str, root: &Path, target: &Path) {
    eprintln!("\n=== DEBUG DUMP [{label}] ===");
    eprintln!("--- tree -a -p --inodes {} ---", root.display());
    match Command::new("tree")
        .args(["-a", "-p", "--inodes"])
        .arg(root)
        .output()
    {
        Ok(o) => {
            eprint!("{}", String::from_utf8_lossy(&o.stdout));
            if !o.stderr.is_empty() {
                eprintln!("[tree stderr] {}", String::from_utf8_lossy(&o.stderr));
            }
        }
        Err(e) => eprintln!("[tree unavailable: {e}]"),
    }
    eprintln!("--- stat {} ---", target.display());
    match Command::new("stat").arg(target).output() {
        Ok(o) => {
            eprint!("{}", String::from_utf8_lossy(&o.stdout));
            if !o.stderr.is_empty() {
                eprint!("[stat stderr] {}", String::from_utf8_lossy(&o.stderr));
            }
        }
        Err(e) => eprintln!("[stat unavailable: {e}]"),
    }
    eprintln!("=== END DEBUG DUMP [{label}] ===\n");
}

#[test]
fn matching_name_under_configured_root_becomes_a_subvolume() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap(); // plain text file, no BTRFS needed
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);

    let target = scratch.path().join("node_modules");
    assert!(run_mkdir_with_shim(data_home.path(), &target).success());
    assert!(
        is_subvolume(&target),
        "expected a real subvolume (inode 256)"
    );
}

#[test]
fn non_matching_name_stays_a_plain_directory() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);

    let target = scratch.path().join("some_other_dir");
    assert!(run_mkdir_with_shim(data_home.path(), &target).success());
    assert!(!is_subvolume(&target));
}

#[test]
fn path_outside_every_configured_root_stays_plain() {
    let scratch = btrfs_scratch_dir();
    let unrelated = btrfs_scratch_dir();
    // compiled.tsv only knows about `scratch`, not `unrelated`.
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);

    let target = unrelated.path().join("node_modules");
    assert!(run_mkdir_with_shim(data_home.path(), &target).success());
    assert!(!is_subvolume(&target));
}

#[test]
fn relative_path_resolves_via_cwd() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "target")]);

    let status = Command::new("mkdir")
        .arg("target")
        .current_dir(scratch.path())
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home.path())
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(is_subvolume(&scratch.path().join("target")));
}

#[test]
fn mkdirat_with_a_real_dirfd_resolves_correctly() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), ".venv")]);

    let status = Command::new(compiled_mkdirat_probe())
        .arg(scratch.path())
        .arg(".venv")
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home.path())
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(is_subvolume(&scratch.path().join(".venv")));
}

#[test]
fn git_tracked_path_is_never_converted() {
    let scratch = btrfs_scratch_dir();
    let repo = scratch.path().join("repo");
    std::fs::create_dir_all(repo.join("vendor")).unwrap();
    std::fs::write(repo.join("vendor/keep.txt"), b"keep").unwrap();
    for args in [
        vec!["init", "-q"],
        vec!["add", "vendor/keep.txt"],
        vec![
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "init",
        ],
    ] {
        assert!(
            Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .status()
                .unwrap()
                .success()
        );
    }
    std::fs::remove_dir_all(repo.join("vendor")).unwrap(); // gone from disk, still tracked

    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(repo.as_path(), "vendor")]);

    let target = repo.join("vendor");
    assert!(run_mkdir_with_shim(data_home.path(), &target).success());
    assert!(
        !is_subvolume(&target),
        "git-tracked path must never become a subvolume"
    );
}

#[test]
fn mkdir_on_an_already_existing_subvolume_passes_through_and_reports_eexist_normally() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "build")]);

    let target = scratch.path().join("build");
    assert!(run_mkdir_with_shim(data_home.path(), &target).success());
    assert!(is_subvolume(&target));

    // Calling mkdir again on the now-existing subvolume must behave
    // exactly like calling mkdir on any pre-existing directory: EEXIST,
    // not silently "succeed" or panic.
    let status = run_mkdir_with_shim(data_home.path(), &target);
    assert!(!status.success());
}

#[test]
fn missing_home_env_var_degrades_to_passthrough_not_a_crash() {
    let scratch = btrfs_scratch_dir();
    let target = scratch.path().join("node_modules");
    let status = Command::new("mkdir")
        .arg(&target)
        .env_remove("HOME")
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!is_subvolume(&target)); // no cache loadable => never intercepts
}

// --- §8.5 debug logging ---

#[test]
fn normal_mode_logs_only_the_creation_not_every_call() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(
        data_home.path(),
        &[(scratch.path(), "node_modules"), (scratch.path(), "other")],
    );
    let log_file = tempfile::NamedTempFile::new().unwrap();

    // A matching name (logged: created) and a non-matching name
    // (never logged in normal mode - would be pure noise).
    let matching = scratch.path().join("node_modules");
    let non_matching = scratch.path().join("unrelated");
    for target in [&matching, &non_matching] {
        let status = Command::new("mkdir")
            .arg(target)
            .env("HOME", fake_home())
            .env("XDG_DATA_HOME", data_home.path())
            .env("GHOSTVOLUMES_LOG_FILE", log_file.path())
            .env("LD_PRELOAD", compiled_shim())
            .status()
            .unwrap();
        assert!(status.success());
    }

    let log_text = std::fs::read_to_string(log_file.path()).unwrap();
    assert_eq!(log_text.lines().count(), 1, "log:\n{log_text}");
    assert!(log_text.contains("created subvolume"));
    assert!(log_text.contains("node_modules"));
    assert!(!log_text.contains("unrelated"));
}

#[test]
fn debug_mode_logs_every_decision_with_its_reason() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);
    let log_file = tempfile::NamedTempFile::new().unwrap();
    // Every run() call below passes this exact path via GHOSTVOLUMES_LOG_FILE
    // explicitly - printed here so a real CI run's captured output settles,
    // in one place, whether every call really shared one log file (as
    // intended) or something caused a different path to be used/read.
    eprintln!("log_file.path() = {}", log_file.path().display());

    let run = |target: &Path| {
        Command::new("mkdir")
            .arg(target)
            .env("HOME", fake_home())
            .env("XDG_DATA_HOME", data_home.path())
            .env("GHOSTVOLUMES_LOG_FILE", log_file.path())
            .env("GHOSTVOLUMES_DEBUG", "1")
            .env("LD_PRELOAD", compiled_shim())
            .status()
            .unwrap()
    };

    let accepted = scratch.path().join("node_modules");
    let no_match = scratch.path().join("unrelated");

    // Reset the shim's side-channel diagnostic file so only this test's
    // own 3 invocations show up below, not leftovers from earlier tests
    // in this same binary (every test in this file loads the shim, which
    // writes to this same fixed path on every invocation).
    let diag_path = std::env::temp_dir().join("ghostvolumes-shim-diag.log");
    let _ = std::fs::remove_file(&diag_path);

    debug_dump("before any mkdir", scratch.path(), &accepted);

    // ACCEPT: matches, gets created.
    assert!(run(&accepted).success());
    debug_dump("after run 1 (expected ACCEPT)", scratch.path(), &accepted);

    // SKIP (no cache match): unrelated name.
    assert!(run(&no_match).success());
    debug_dump(
        "after run 2 (expected SKIP no-match)",
        scratch.path(),
        &accepted,
    );

    // SKIP (already a subvolume): re-run against the now-existing one.
    debug_dump(
        "immediately before run 3 (expected SKIP already-subvolume)",
        scratch.path(),
        &accepted,
    );
    run(&accepted); // expected to fail (EEXIST) - not asserted, only the log matters here
    debug_dump("immediately after run 3", scratch.path(), &accepted);

    let log_text = std::fs::read_to_string(log_file.path()).unwrap();
    eprintln!("\n=== full log_file content ({} bytes) ===", log_text.len());
    eprintln!("{log_text}");
    eprintln!("=== end log_file content ===\n");

    match std::fs::read_to_string(&diag_path) {
        Ok(diag_text) => {
            eprintln!(
                "\n=== shim side-channel diagnostics ({} bytes, {}) ===",
                diag_text.len(),
                diag_path.display()
            );
            eprintln!("{diag_text}");
            eprintln!("=== end shim side-channel diagnostics ===\n");
        }
        Err(e) => eprintln!(
            "\n[no shim side-channel diagnostics at {}: {e}]\n",
            diag_path.display()
        ),
    }

    assert!(log_text.contains("-> ACCEPT (created subvolume)"));
    assert!(log_text.contains("-> SKIP (no cache match)"));
    assert!(log_text.contains("-> SKIP (already a subvolume)"));
}

#[test]
fn ghostvolumes_debug_zero_explicitly_disables_debug_logging() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);
    let log_file = tempfile::NamedTempFile::new().unwrap();

    let target = scratch.path().join("unrelated"); // no cache match
    let status = Command::new("mkdir")
        .arg(&target)
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home.path())
        .env("GHOSTVOLUMES_LOG_FILE", log_file.path())
        .env("GHOSTVOLUMES_DEBUG", "0")
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap();
    assert!(status.success());

    let log_text = std::fs::read_to_string(log_file.path()).unwrap();
    assert!(log_text.is_empty(), "log:\n{log_text}");
}

#[test]
fn shim_never_writes_to_stdout_or_stderr_even_in_debug_mode() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);
    let log_file = tempfile::NamedTempFile::new().unwrap();

    let target = scratch.path().join("node_modules");
    let output = Command::new("mkdir")
        .arg(&target)
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home.path())
        .env("GHOSTVOLUMES_LOG_FILE", log_file.path())
        .env("GHOSTVOLUMES_DEBUG", "1")
        .env("LD_PRELOAD", compiled_shim())
        .output()
        .unwrap();

    assert!(output.status.success());
    assert!(
        output.stdout.is_empty(),
        "stdout: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert!(
        output.stderr.is_empty(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    // Confirms logging actually happened (in the file, not on screen).
    assert!(!std::fs::read_to_string(log_file.path()).unwrap().is_empty());
}

#[test]
fn no_log_file_configured_or_writable_is_not_a_crash() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);

    let target = scratch.path().join("node_modules");
    // Points at a path whose parent doesn't exist - the open() will
    // fail, logging must silently no-op rather than error/panic.
    let status = Command::new("mkdir")
        .arg(&target)
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home.path())
        .env("GHOSTVOLUMES_LOG_FILE", "/no/such/directory/shim.log")
        .env("GHOSTVOLUMES_DEBUG", "1")
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(is_subvolume(&target)); // interception itself still works
}
