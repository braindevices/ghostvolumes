use std::fs;
use std::path::Path;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

// This integration-test binary has no `[lib]` target to `use
// ghostvolumes::...` from — `include!`s the real `src/filenames.rs`
// instead of hand-keeping local copies of its constants.
include!("../src/filenames.rs");

/// A `ghostvolumes` invocation with `HOME` pointed at `home` and no
/// `XDG_*` overrides - the setup every test here needs.
fn ghostvolumes_cmd(home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.env("HOME", home);
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd
}

fn compiled_cache_path(home: &Path) -> std::path::PathBuf {
    home.join(".local/share/ghostvolumes")
        .join(COMPILED_CACHE_FILE_NAME)
}

#[test]
fn reload_with_no_config_writes_empty_cache_under_xdg_dirs() {
    let home = tempdir().unwrap();

    ghostvolumes_cmd(home.path())
        .arg("reload")
        .assert()
        .success();

    assert_eq!(
        fs::read_to_string(compiled_cache_path(home.path())).unwrap(),
        ""
    );
}

#[test]
fn reload_with_non_btrfs_root_fails_with_clear_message() {
    let home = tempdir().unwrap();
    let config_dir = home.path().join(".config/ghostvolumes");
    fs::create_dir_all(config_dir.join(ROOTS_D_DIR)).unwrap();
    fs::write(
        config_dir.join(ROOTS_D_DIR).join(AUTO_ROOTS_FILE_NAME),
        format!("[\"{}\"]", home.path().display()),
    )
    .unwrap();

    ghostvolumes_cmd(home.path())
        .arg("reload")
        .assert()
        .failure()
        .stderr(predicate::str::contains("not BTRFS-backed"))
        .stderr(predicate::str::contains("scan --save"));

    assert!(!compiled_cache_path(home.path()).exists());
}
