use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

// This integration-test binary has no `[lib]` target to `use
// ghostvolumes::...` from — `include!`s the real `src/filenames.rs`
// instead of hand-keeping a local copy of `SHIM_FILE_NAME`.
include!("../src/filenames.rs");

/// A `ghostvolumes scan` invocation with `HOME` set to `home` (or
/// removed entirely if `None`, to test the guard's `$HOME`-independence)
/// and no `XDG_*` overrides - the setup every test here needs, before
/// each one adds its own `LD_PRELOAD` value.
fn ghostvolumes_scan_cmd(home: Option<&Path>) -> Command {
    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    match home {
        Some(home) => cmd.env("HOME", home),
        None => cmd.env_remove("HOME"),
    };
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.arg("scan");
    cmd
}

#[test]
fn refuses_to_run_when_ld_preload_already_contains_its_own_shim() {
    let home = tempdir().unwrap();

    ghostvolumes_scan_cmd(Some(home.path()))
        .env("LD_PRELOAD", format!("/some/other/path/{SHIM_FILE_NAME}"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("refusing to run"))
        .stderr(predicate::str::contains("shell-init"));
}

#[test]
fn refuses_even_when_home_is_entirely_unset() {
    // Matching is basename-only and doesn't need $HOME/XDG dirs to
    // resolve at all - the guard must still fire even when nothing else
    // about the environment could be resolved.
    ghostvolumes_scan_cmd(None)
        .env("LD_PRELOAD", format!("/anywhere/{SHIM_FILE_NAME}"))
        .assert()
        .failure()
        .stderr(predicate::str::contains("refusing to run"));
}

#[test]
fn runs_normally_when_ld_preload_is_unset() {
    let home = tempdir().unwrap();

    ghostvolumes_scan_cmd(Some(home.path()))
        .env_remove("LD_PRELOAD")
        .assert()
        .success();
}

#[test]
fn runs_normally_when_ld_preload_points_at_something_unrelated() {
    let home = tempdir().unwrap();

    ghostvolumes_scan_cmd(Some(home.path()))
        .env("LD_PRELOAD", "/usr/lib/libsomething-else.so")
        .assert()
        .success();
}
