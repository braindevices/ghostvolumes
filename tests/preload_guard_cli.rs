use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

/// The shim's filename (matches `init::SHIM_FILE_NAME`) - the guard
/// matches by this basename only, regardless of directory, so these
/// tests deliberately use paths that aren't under any resolved XDG data
/// dir to prove that.
const SHIM_FILE_NAME: &str = "libghostvolumes_shim.so";

#[test]
fn refuses_to_run_when_ld_preload_already_contains_its_own_shim() {
    let home = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.env("HOME", home.path());
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.env("LD_PRELOAD", format!("/some/other/path/{SHIM_FILE_NAME}"));
    cmd.arg("scan");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("refusing to run"))
        .stderr(predicate::str::contains("shell-init"));
}

#[test]
fn refuses_even_when_home_is_entirely_unset() {
    // Matching is basename-only and doesn't need $HOME/XDG dirs to
    // resolve at all - the guard must still fire even when nothing else
    // about the environment could be resolved.
    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.env_remove("HOME");
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.env("LD_PRELOAD", format!("/anywhere/{SHIM_FILE_NAME}"));
    cmd.arg("scan");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("refusing to run"));
}

#[test]
fn runs_normally_when_ld_preload_is_unset() {
    let home = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.env("HOME", home.path());
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.env_remove("LD_PRELOAD");
    cmd.arg("scan");
    cmd.assert().success();
}

#[test]
fn runs_normally_when_ld_preload_points_at_something_unrelated() {
    let home = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.env("HOME", home.path());
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.env("LD_PRELOAD", "/usr/lib/libsomething-else.so");
    cmd.arg("scan");
    cmd.assert().success();
}
