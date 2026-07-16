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

// This integration-test binary has no `[lib]` target to `use
// ghostvolumes::...` from — `include!`s the real `src/filenames.rs`
// instead of hand-keeping local copies of its constants.
include!("../src/filenames.rs");
// Same reasoning - the real boundary_lock_path()/open_lock_file(),
// used to simulate a concurrent convert holding the per-project lock.
include!("../src/lock.rs");

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
    let dir = data_home.join("ghostvolumes");
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = String::new();
    for (prefix, name) in rows {
        text.push_str(&format!("{}\t{name}\n", prefix.display()));
    }
    std::fs::write(dir.join(COMPILED_CACHE_FILE_NAME), text).unwrap();
}

/// The decision file's path at `project_root` (ai-work/tasks/decision-model.plan.md
/// §1).
fn decision_file_path(project_root: &Path) -> PathBuf {
    project_root.join(DECISION_FILE_NAME)
}

/// Writes a decision file at `project_root` - the shim's replacement
/// for the old git-tracked gate. Most of these tests use
/// `project_root == scratch.path()`, matching `write_cache`'s row
/// prefix, since that's also the walk-up boundary the shim resolves to
/// when nothing's registered in the (absent, in these tests)
/// project-roots list.
fn write_decision(project_root: &Path, text: &str) {
    std::fs::write(decision_file_path(project_root), text).unwrap();
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

#[test]
fn matching_name_with_an_accept_decision_becomes_a_subvolume() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap(); // plain text file, no BTRFS needed
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);
    write_decision(scratch.path(), "+ node_modules\n");

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
fn lock_contention_falls_through_to_a_plain_mkdir_rather_than_blocking() {
    // The shim side of the shim-vs-convert directory-swap lock
    // (ai-work/tasks/atomic-file-io.plan.md §6): simulates a `convert`
    // already holding this project's boundary lock (as it would mid
    // create/copy/rename) while the shim tries to create the same
    // subvolume - the shim's non-blocking try_lock must lose gracefully,
    // falling through to a real (plain) mkdir rather than blocking the
    // build or erroring.
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);
    write_decision(scratch.path(), "+ node_modules\n");

    let data_dir = data_home.path().join("ghostvolumes");
    let lock_path = boundary_lock_path(&data_dir.join(LOCKS_DIR), scratch.path());
    let lock_file = open_lock_file(&lock_path).unwrap();
    lock_file.lock().unwrap();

    let target = scratch.path().join("node_modules");
    assert!(run_mkdir_with_shim(data_home.path(), &target).success());
    assert!(
        !is_subvolume(&target),
        "lock contention must fall through to a plain mkdir, not block or fail"
    );

    drop(lock_file);
}

#[test]
fn relative_path_resolves_via_cwd() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "target")]);
    write_decision(scratch.path(), "+ target\n");

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
    write_decision(scratch.path(), "+ .venv\n");

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
fn denied_decision_is_never_converted() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "vendor")]);
    write_decision(scratch.path(), "- vendor\n");

    let target = scratch.path().join("vendor");
    assert!(run_mkdir_with_shim(data_home.path(), &target).success());
    assert!(
        !is_subvolume(&target),
        "a `-` decision must never become a subvolume"
    );
}

#[test]
fn undecided_candidate_stays_plain_and_logs_an_always_on_notice() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);
    // No decision file at all - nothing to resolve against.
    let log_file = tempfile::NamedTempFile::new().unwrap();

    let target = scratch.path().join("node_modules");
    let status = Command::new("mkdir")
        .arg(&target)
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home.path())
        .env("GHOSTVOLUMES_LOG_FILE", log_file.path())
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!is_subvolume(&target), "undecided must never convert");

    // Undecided is logged even without GHOSTVOLUMES_DEBUG (plan §4):
    // it's the one signal a human has that a decision is waiting.
    let log_text = std::fs::read_to_string(log_file.path()).unwrap();
    assert!(log_text.contains("undecided"), "log:\n{log_text}");
}

#[test]
fn undecided_candidate_appends_a_pending_comment_to_the_project_decision_file() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);

    let target = scratch.path().join("node_modules");
    assert!(run_mkdir_with_shim(data_home.path(), &target).success());
    assert!(!is_subvolume(&target));

    let decision_text = std::fs::read_to_string(decision_file_path(scratch.path())).unwrap();
    assert_eq!(decision_text, "? /node_modules\n");
}

#[test]
fn undecided_candidate_does_not_duplicate_the_pending_comment_on_repeat_runs() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);

    let target = scratch.path().join("node_modules");
    run_mkdir_with_shim(data_home.path(), &target); // creates the plain dir
    run_mkdir_with_shim(data_home.path(), &target); // EEXIST at the OS level, but decide() still runs

    let decision_text = std::fs::read_to_string(decision_file_path(scratch.path())).unwrap();
    assert_eq!(
        decision_text.lines().count(),
        1,
        "decision file:\n{decision_text}"
    );
}

#[test]
fn ghostvolumes_auto_yes_bypasses_the_decision_lookup_entirely() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);
    // No decision file at all - would be undecided (skip) without
    // GHOSTVOLUMES_AUTO_YES.

    let target = scratch.path().join("node_modules");
    let status = Command::new("mkdir")
        .arg(&target)
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home.path())
        .env("GHOSTVOLUMES_AUTO_YES", "1")
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(
        is_subvolume(&target),
        "GHOSTVOLUMES_AUTO_YES must bypass undecided-skip"
    );

    // Nothing gets recorded - the env var itself is the standing
    // approval, not a decision file entry.
    assert!(!decision_file_path(scratch.path()).exists());
}

#[test]
fn ghostvolumes_auto_yes_zero_does_not_bypass_the_lookup() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "node_modules")]);

    let target = scratch.path().join("node_modules");
    let status = Command::new("mkdir")
        .arg(&target)
        .env("HOME", fake_home())
        .env("XDG_DATA_HOME", data_home.path())
        .env("GHOSTVOLUMES_AUTO_YES", "0")
        .env("LD_PRELOAD", compiled_shim())
        .status()
        .unwrap();
    assert!(status.success());
    assert!(!is_subvolume(&target));
}

#[test]
fn mkdir_on_an_already_existing_subvolume_passes_through_and_reports_eexist_normally() {
    let scratch = btrfs_scratch_dir();
    let data_home = tempfile::tempdir().unwrap();
    write_cache(data_home.path(), &[(scratch.path(), "build")]);
    write_decision(scratch.path(), "+ build\n");

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
    write_decision(scratch.path(), "+ node_modules\n");
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
    write_decision(scratch.path(), "+ node_modules\n");
    let log_file = tempfile::NamedTempFile::new().unwrap();

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

    // ACCEPT: matches, gets created.
    let accepted = scratch.path().join("node_modules");
    assert!(run(&accepted).success());
    // SKIP (no cache match): unrelated name.
    let no_match = scratch.path().join("unrelated");
    assert!(run(&no_match).success());
    // Re-run against the now-existing subvolume. Exit status isn't
    // asserted: some `mkdir` implementations (e.g. uutils' Rust
    // coreutils, now the default `mkdir` on newer Ubuntu releases)
    // `stat()` an already-existing target and report their own "File
    // exists" error without ever calling `mkdir()`/`mkdirat()` at all -
    // so the shim may not even be *entered* for this call. That's a
    // real, legitimate difference between `mkdir` implementations, not
    // a bug: nothing the shim is responsible for (creating or
    // converting a directory) happens either way. The assertions below
    // check that invariant directly instead of assuming any particular
    // libc call pattern from whichever `mkdir` binary is installed.
    run(&accepted);

    let log_text = std::fs::read_to_string(log_file.path()).unwrap();
    assert!(log_text.contains("-> ACCEPT (created subvolume)"));
    assert!(log_text.contains("-> SKIP (no cache match)"));

    // `-> ENTER` is logged before `decide()` even runs (see
    // handle_intercept), so it tells apart "the shim was entered but
    // decided X" from "the shim was never entered for this call at
    // all". Exactly 1 occurrence means only run 1's create reached it
    // (run 3's `mkdir` resolved existence on its own); exactly 2 means
    // run 3 reached it too, in which case it must have logged the
    // correct reason.
    let enter_marker = format!("{} -> ENTER", accepted.display());
    match log_text.matches(&enter_marker).count() {
        1 => {}
        2 => assert!(
            log_text.contains("-> SKIP (already a subvolume)"),
            "shim was entered for the re-run but didn't log the expected decision:\n{log_text}"
        ),
        n => panic!("expected 1 or 2 '{enter_marker}' occurrences, got {n}:\n{log_text}"),
    }

    // Whichever path the host's `mkdir` took, the shim must never have
    // created a second subvolume for an already-existing path.
    assert_eq!(
        log_text.matches("-> ACCEPT (created subvolume)").count(),
        1,
        "must never create a second subvolume for an already-existing path:\n{log_text}"
    );
    assert!(is_subvolume(&accepted));
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
    write_decision(scratch.path(), "+ node_modules\n");

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
