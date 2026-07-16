use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn help_lists_all_subcommands() {
    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.arg("--help");
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("roots"))
        .stdout(predicate::str::contains("reload"))
        .stdout(predicate::str::contains("init"))
        .stdout(predicate::str::contains("discover"))
        .stdout(predicate::str::contains("convert"))
        .stdout(predicate::str::contains("projects"))
        .stdout(predicate::str::contains("intercept"))
        .stdout(predicate::str::contains("shell-init"));
}

#[test]
fn roots_help_lists_scan_and_list() {
    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.args(["roots", "--help"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("scan"))
        .stdout(predicate::str::contains("list"));
}

#[test]
fn projects_help_lists_list_register_and_unregister() {
    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.args(["projects", "--help"]);
    cmd.assert()
        .success()
        .stdout(predicate::str::contains("list"))
        .stdout(predicate::str::contains("register"))
        .stdout(predicate::str::contains("unregister"));
}

#[test]
fn no_args_fails_with_usage() {
    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.assert().failure();
}

#[test]
fn unrecognized_subcommand_fails_with_usage() {
    // Every subcommand is fully implemented - this checks clap's own
    // handling of an invalid subcommand name rather than "still a
    // stub", which no longer applies to anything.
    let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
    cmd.arg("frobnicate");
    cmd.assert()
        .failure()
        .stderr(predicate::str::contains("unrecognized subcommand"));
}
