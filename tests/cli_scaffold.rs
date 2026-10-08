use assert_cmd::Command;
use predicates::prelude::*;

#[path = "support/btrfs.rs"]
mod btrfs;

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
        .stdout(predicate::str::contains("prune"))
        .stdout(predicate::str::contains("vcs-manifest"))
        .stdout(predicate::str::contains("vcs-health"))
        .stdout(predicate::str::contains("intercept").not())
        .stdout(predicate::str::contains("shell-init").not());
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

#[test]
fn contrib_lists_and_prints_the_bundled_files_byte_for_byte() {
    let out = Command::cargo_bin("ghostvolumes")
        .unwrap()
        .arg("contrib")
        .output()
        .unwrap();
    let list = String::from_utf8(out.stdout).unwrap();
    assert!(list.lines().any(|l| l == "ghostvolumes-snapshot"), "{list}");
    let script = Command::cargo_bin("ghostvolumes")
        .unwrap()
        .args(["contrib", "ghostvolumes-snapshot"])
        .output()
        .unwrap()
        .stdout;
    assert_eq!(
        script,
        std::fs::read("contrib/ghostvolumes-snapshot").unwrap()
    );
    Command::cargo_bin("ghostvolumes")
        .unwrap()
        .args(["contrib", "nope"])
        .assert()
        .failure();
}

/// `<home>/src/.snapshots/7/snapshot` as a real subvolume holding
/// `p/target` under `+ target`, and the env every prune run gets.
struct Snap {
    home: tempfile::TempDir,
    snap: std::path::PathBuf,
}

fn snap() -> Snap {
    let home = btrfs::scratch();
    let snaps = home.path().join("src/.snapshots/7");
    std::fs::create_dir_all(&snaps).unwrap();
    btrfs::create_subvolume(&snaps, "snapshot");
    let snap = snaps.join("snapshot");
    std::fs::create_dir_all(snap.join("p/target")).unwrap();
    std::fs::create_dir_all(snap.join("p/src")).unwrap();
    std::fs::write(snap.join("p/.ghostvolumes-decisions"), "+ target\n").unwrap();
    Snap { home, snap }
}

impl Snap {
    fn prune(&self, args: &[&std::ffi::OsStr]) -> assert_cmd::assert::Assert {
        let h = self.home.path();
        let mut cmd = Command::cargo_bin("ghostvolumes").unwrap();
        cmd.arg("prune")
            .args(args)
            .env("HOME", h)
            .env("XDG_CONFIG_HOME", h.join("config"))
            .env("XDG_DATA_HOME", h.join("data"))
            .env("XDG_STATE_HOME", h.join("state"))
            .env("PATH", h.join("no-bin")) // no snapper: --since can't find a baseline
            .env_remove("LD_PRELOAD");
        cmd.assert()
    }
}

#[test]
fn prune_dry_run_uses_only_the_snapshots_own_decisions() {
    // No project list, garbage roots.d / compiled.tsv: none of it matters.
    let s = snap();
    let h = s.home.path();
    std::fs::create_dir_all(h.join("config/ghostvolumes/roots.d")).unwrap();
    std::fs::write(
        h.join("config/ghostvolumes/roots.d/00-auto.toml"),
        "not = [toml",
    )
    .unwrap();
    std::fs::create_dir_all(h.join("data/ghostvolumes")).unwrap();
    std::fs::write(h.join("data/ghostvolumes/compiled.tsv"), "\0garbage").unwrap();
    s.prune(&["--dry-run".as_ref(), s.snap.as_os_str()])
        .success()
        .stdout(format!(
            "would prune: {}\n",
            s.snap.join("p/target").display()
        ));
    assert!(s.snap.join("p/target").exists(), "dry run changes nothing");
}

#[test]
fn prune_refuses_anything_but_a_real_snapshot_with_exit_2() {
    let s = snap();
    let h = s.home.path();
    // The live tree, a plain dir in Snapper's layout, a symlinked path.
    std::fs::create_dir_all(h.join("src/p")).unwrap();
    s.prune(&[h.join("src").as_os_str()]).code(2);
    let plain = h.join("src/.snapshots/8/snapshot");
    std::fs::create_dir_all(&plain).unwrap();
    s.prune(&[plain.as_os_str()]).code(2);
    std::fs::create_dir_all(h.join("src/.snapshots/9")).unwrap();
    // A symlink in the layout pointing at the live tree resolves to it.
    std::os::unix::fs::symlink(h.join("src"), h.join("src/.snapshots/9/snapshot")).unwrap();
    s.prune(&[h.join("src/.snapshots/9/snapshot").as_os_str()])
        .code(2);
    assert!(s.snap.join("p/target").exists());
}

#[test]
fn prune_rejects_bad_since_and_config_with_exit_2() {
    let s = snap();
    let snap = s.snap.as_os_str();
    for args in [
        &["--since", "soon", "--config", "src"][..],
        &["--since", "2999-01-01", "--config", "src"],
        &["--since", "1d", "--config", "-x"],
        &["--since", "1d", "--config", "a/b"],
    ] {
        let mut all: Vec<&std::ffi::OsStr> = vec!["--dry-run".as_ref()];
        all.extend(args.iter().map(std::ffi::OsStr::new));
        all.push(snap);
        s.prune(&all).code(2);
    }
    // --since needs --config (a usage error from clap).
    s.prune(&["--since".as_ref(), "1d".as_ref(), snap])
        .failure();
}

#[test]
fn prune_since_without_a_reachable_snapper_reports_everything_once_in_the_log() {
    // No baseline because snapper can't be run: the journal lists every
    // path, the events log gets one summary line.
    let s = snap();
    std::fs::write(s.snap.join("p/target/big.o"), "o").unwrap();
    let args: [&std::ffi::OsStr; 5] = [
        "--since".as_ref(),
        "1d".as_ref(),
        "--config".as_ref(),
        "src".as_ref(),
        s.snap.as_os_str(),
    ];
    let out = s.prune(&args).code(4).get_output().stdout.clone();
    let out = String::from_utf8(out).unwrap();
    assert!(
        out.contains("will prune (decisions changed): p/target (1 file(s), newest "),
        "{out}"
    );
    assert!(!s.snap.join("p/target").exists());
    let log = std::fs::read_to_string(s.home.path().join("state/ghostvolumes/events.log")).unwrap();
    assert_eq!(log.lines().count(), 1, "{log}");
    assert!(
        log.contains("baseline unavailable (") && log.contains("1 path(s) pruned unchecked"),
        "{log}"
    );
}
