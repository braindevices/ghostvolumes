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

fn write_roots_d(home: &Path, contents: &str) {
    let roots_d = home.join(".config/ghostvolumes").join(ROOTS_D_DIR);
    fs::create_dir_all(&roots_d).unwrap();
    fs::write(roots_d.join("10-local.toml"), contents).unwrap();
}

#[test]
fn roots_list_shows_each_root_with_its_effective_watch_list() {
    let home = tempdir().unwrap();
    write_roots_d(
        home.path(),
        "default-watches = [\"node_modules\", \"target\"]\n\n[\"/home/user\"]\n",
    );

    ghostvolumes_cmd(home.path())
        .args(["roots", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/home/user"))
        .stdout(predicate::str::contains("node_modules"))
        .stdout(predicate::str::contains("target"));
}

#[test]
fn roots_list_shows_a_root_s_own_override_not_the_default() {
    let home = tempdir().unwrap();
    write_roots_d(
        home.path(),
        r#"
default-watches = ["node_modules"]

["/home/user/special-project"]
watches = ["dist"]
"#,
    );

    ghostvolumes_cmd(home.path())
        .args(["roots", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/home/user/special-project"))
        .stdout(predicate::str::contains("dist"))
        .stdout(predicate::str::contains("node_modules").not());
}

#[test]
fn roots_list_omits_a_disabled_root() {
    let home = tempdir().unwrap();
    write_roots_d(
        home.path(),
        r#"
["/home/user/kept"]

["/mnt/noisy"]
enabled = false
"#,
    );

    ghostvolumes_cmd(home.path())
        .args(["roots", "list"])
        .assert()
        .success()
        .stdout(predicate::str::contains("/home/user/kept"))
        .stdout(predicate::str::contains("/mnt/noisy").not());
}

#[test]
fn roots_list_with_no_config_prints_nothing() {
    let home = tempdir().unwrap();

    ghostvolumes_cmd(home.path())
        .args(["roots", "list"])
        .assert()
        .success()
        .stdout("");
}
