use std::fs;

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;

#[test]
fn reload_with_no_config_writes_empty_cache_under_xdg_dirs() {
    let home = tempdir().unwrap();

    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.env("HOME", home.path());
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.arg("reload");
    cmd.assert().success();

    let cache_path = home.path().join(".local/share/ghostvolumes/compiled.tsv");
    assert_eq!(fs::read_to_string(cache_path).unwrap(), "");
}

#[test]
fn reload_with_non_btrfs_root_fails_with_clear_message() {
    let home = tempdir().unwrap();
    let config_dir = home.path().join(".config/ghostvolumes");
    fs::create_dir_all(config_dir.join("roots.d")).unwrap();
    fs::write(
        config_dir.join("roots.d/00-auto.toml"),
        format!(r#"roots = ["{}"]"#, home.path().display()),
    )
    .unwrap();

    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.env("HOME", home.path());
    cmd.env_remove("XDG_CONFIG_HOME");
    cmd.env_remove("XDG_DATA_HOME");
    cmd.arg("reload");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("not BTRFS-backed"))
        .stderr(predicate::str::contains("scan --save"));

    assert!(
        !home
            .path()
            .join(".local/share/ghostvolumes/compiled.tsv")
            .exists()
    );
}
