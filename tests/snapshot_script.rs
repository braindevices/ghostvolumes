//! End-to-end runs of `contrib/ghostvolumes-snapshot` against a fake
//! `snapper` (`tests/support/fake-snapper`, real BTRFS snapshots via the
//! same ioctls) — or, with `GHOSTVOLUMES_DEV_SNAPPER` set to the dir
//! `scripts/dev-snapper.sh` prints, against a real snapper 0.13 (no
//! daemon, its config under the scratch dir, never /etc). Everything
//! lives in a dummy subvolume under the BTRFS test scratch dir; HOME and
//! XDG_* point there too. CI also runs the real thing with snapperd.

use std::path::{Path, PathBuf};
use std::process::Command;

#[path = "support/btrfs.rs"]
mod btrfs;
use btrfs::{create_subvolume, scratch};

struct Env {
    _scratch: tempfile::TempDir,
    home: PathBuf,
    src: PathBuf,
    /// The `snapper` the script and the tests run.
    snapper: PathBuf,
    /// Real snapper (`scripts/dev-snapper.sh`): no back-dating.
    real: bool,
}

/// `target/dev-snapper` from `scripts/dev-snapper.sh`, if requested.
fn dev_snapper() -> Option<PathBuf> {
    std::env::var_os("GHOSTVOLUMES_DEV_SNAPPER").map(PathBuf::from)
}

impl Env {
    fn new() -> Self {
        let scratch = scratch();
        let home = scratch.path().join("home");
        let bin = home.join(".cargo/bin");
        std::fs::create_dir_all(&bin).unwrap();
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        let snapper = match dev_snapper() {
            Some(dir) => {
                // What `snapper create-config` would write, under the root
                // the wrapper passes as `--root` (SUBVOLUME is root-relative).
                let etc = scratch.path().join("etc");
                std::fs::create_dir_all(etc.join("sysconfig")).unwrap();
                std::fs::create_dir_all(etc.join("snapper/configs")).unwrap();
                std::fs::write(etc.join("sysconfig/snapper"), "SNAPPER_CONFIGS=\"src\"\n").unwrap();
                let template = std::fs::read_to_string(
                    dir.join("root/usr/share/snapper/config-templates/default"),
                )
                .unwrap();
                let config: String = template
                    .lines()
                    .map(|l| match l.split('=').next() {
                        Some("SUBVOLUME") => "SUBVOLUME=\"/src\"".to_string(),
                        Some("TIMELINE_CREATE") => "TIMELINE_CREATE=\"no\"".to_string(),
                        _ => l.to_string(),
                    } + "\n")
                    .collect();
                std::fs::write(etc.join("snapper/configs/src"), config).unwrap();
                dir.join("bin/snapper")
            }
            None => manifest.join("tests/support/fake-snapper"),
        };
        std::os::unix::fs::symlink(&snapper, bin.join("snapper")).unwrap();
        std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_ghostvolumes"), bin.join("ghostvolumes"))
            .unwrap();
        create_subvolume(scratch.path(), "src");
        let src = scratch.path().join("src");
        create_subvolume(&src, ".snapshots"); // like `snapper create-config`
        Env {
            real: dev_snapper().is_some(),
            _scratch: scratch,
            home,
            src,
            snapper,
        }
    }

    fn command(&self, program: &Path) -> Command {
        let mut cmd = Command::new(program);
        cmd.env_clear()
            .env("HOME", &self.home)
            .env("PATH", "/usr/bin:/bin")
            .env("XDG_CONFIG_HOME", self.home.join(".config"))
            .env("XDG_DATA_HOME", self.home.join(".local/share"))
            .env("XDG_STATE_HOME", self.home.join(".local/state"))
            .env("FAKE_SNAPPER_SUBVOL", &self.src)
            .env("DEV_SNAPPER_ROOT", self.src.parent().unwrap());
        cmd
    }

    /// Runs the real contrib script; returns (exit code, stdout+stderr).
    fn run_script(&self) -> (i32, String) {
        self.run_script_at(None)
    }

    /// Same, with the snapshot it creates dated `date` (UTC "%F %T").
    fn run_script_at(&self, date: Option<&str>) -> (i32, String) {
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("contrib/ghostvolumes-snapshot");
        let mut cmd = self.command(Path::new("/bin/sh"));
        if let Some(date) = date {
            cmd.env("FAKE_SNAPPER_DATE", date);
        }
        let out = cmd.arg(script).arg("src").arg(&self.src).output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned()
            + &String::from_utf8_lossy(&out.stderr);
        (out.status.code().unwrap_or(-1), text)
    }

    fn snapper(&self, args: &[&str]) -> String {
        let out = self
            .command(&self.snapper)
            .args(["-c", "src"])
            .args(args)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap()
    }

    fn verify_tag(&self, n: u32) -> String {
        let out = self
            .command(&self.snapper)
            .args([
                "--csvout",
                "-c",
                "src",
                "list",
                "--columns",
                "number,userdata",
            ])
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .find_map(|l| l.strip_prefix(&format!("{n},")).map(|t| t.replace('"', "")))
            .unwrap_or_default()
    }

    /// Replaces the `ghostvolumes` the script runs with `sh` code run before
    /// the real binary (`$@` are its arguments; `$G` is the real binary;
    /// `$STATE` a scratch dir for the hook's own state).
    fn hook_ghostvolumes(&self, code: &str) {
        let bin = self.home.join(".cargo/bin/ghostvolumes");
        std::fs::remove_file(&bin).unwrap();
        let state = self.home.join("hook");
        std::fs::create_dir_all(&state).unwrap();
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\nG='{}'\nSTATE='{}'\n{code}\nexec \"$G\" \"$@\"\n",
                env!("CARGO_BIN_EXE_ghostvolumes"),
                state.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    fn snap(&self, n: u32) -> PathBuf {
        self.src.join(format!(".snapshots/{n}/snapshot"))
    }

    fn git(&self, args: &[&str]) {
        let ok = Command::new("git")
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "-c",
                "init.defaultBranch=main",
            ])
            .args(args)
            .current_dir(self.src.join("p"))
            .status()
            .unwrap()
            .success();
        assert!(ok, "git {args:?}");
    }

    fn write(&self, rel: &str, text: &str) {
        let path = self.src.join("p").join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        // Read-only snapshots can't be removed by the scratch dir's cleanup.
        // Best effort, never a panic (that would abort the test run).
        let snapshots = std::fs::read_dir(self.src.join(".snapshots"));
        for entry in snapshots.into_iter().flatten().flatten() {
            if let Some(n) = entry
                .file_name()
                .to_str()
                .filter(|n| n.parse::<u32>().is_ok())
            {
                let _ = self
                    .command(&self.snapper)
                    .args(["-c", "src", "delete", n])
                    .output();
            }
        }
        // Real snapper can't delete here (no CAP_SYS_ADMIN for the destroy
        // ioctl): unlock what's left, so the scratch dir's rmtree (plain
        // rmdir of an emptied subvolume works) can remove it.
        let left = std::fs::read_dir(self.src.join(".snapshots"));
        for entry in left.into_iter().flatten().flatten() {
            for inner in std::fs::read_dir(entry.path())
                .into_iter()
                .flatten()
                .flatten()
            {
                btrfs::make_writable(&inner.path());
            }
        }
    }
}

fn project(env: &Env, decisions: &str) {
    env.write(".ghostvolumes-decisions", decisions);
    env.write(".gitignore", "target/\n");
    env.write("src/main.rs", "fn main() {}");
    env.write("target/big.o", "build output");
    env.git(&["init", "-q"]);
    env.git(&["add", "."]);
    env.git(&["commit", "-qm", "init"]);
}

#[test]
fn hourly_runs_prune_verify_skip_unchanged_repos_and_catch_corruption() {
    let env = Env::new();
    project(&env, "+ target\n");

    // Run 1: pruned, locked, every repo health-checked; with no earlier
    // snapshot, every `+` rule is reported once as new.
    let (code, out) = env.run_script();
    assert_eq!(code, 0, "{out}");
    assert!(out.contains("pruned: ") && out.contains("ok: "), "{out}");
    assert_eq!(env.verify_tag(1), "verify=rules-changed");
    assert!(!env.snap(1).join("p/target").exists());
    assert!(env.snap(1).join("p/src/main.rs").exists() && env.snap(1).join("p/.git/HEAD").exists());
    assert!(
        std::fs::write(env.snap(1).join("p/x"), "").is_err(),
        "locked read-only"
    );
    assert!(
        env.src.join("p/target/big.o").exists(),
        "live tree untouched"
    );

    // With real snapper, its own metadata is there (the fake writes none).
    assert_eq!(env.src.join(".snapshots/1/info.xml").exists(), env.real);

    // Run 2: nothing changed under .git → health skipped.
    let (code, out) = env.run_script();
    assert_eq!((code, env.verify_tag(2)), (0, "verify=ok".into()), "{out}");
    assert!(out.contains("unchanged: "), "{out}");

    // Run 3: a new commit → checked again.
    env.write("src/lib.rs", "");
    env.git(&["add", "src/lib.rs"]);
    env.git(&["commit", "-qm", "two"]);
    let (code, out) = env.run_script();
    assert_eq!((code, env.verify_tag(3)), (0, "verify=ok".into()), "{out}");
    assert!(out.contains("ok: "), "{out}");

    // Run 4: a missing object in the repo → health-failed, exit 1 (pages).
    let head = std::fs::read_to_string(env.src.join("p/.git/refs/heads/main")).unwrap();
    let (dir, file) = head.trim().split_at(2);
    std::fs::remove_file(env.src.join("p/.git/objects").join(dir).join(file)).unwrap();
    let (code, out) = env.run_script();
    assert_eq!(
        (code, env.verify_tag(4)),
        (1, "verify=health-failed".into()),
        "{out}"
    );
}

#[test]
fn a_run_that_died_mid_way_is_pruned_and_locked_by_the_next() {
    let env = Env::new();
    project(&env, "+ target\n");
    // As if the script died right after `snapper create`.
    let n = env.snapper(&[
        "create",
        "--read-write",
        "--description",
        "pruned",
        "--userdata",
        "verify=pending",
        "--print-number",
    ]);
    assert_eq!(n.trim(), "1");
    assert!(env.snap(1).join("p/target/big.o").exists());

    let (code, out) = env.run_script();
    assert_eq!(code, 0, "{out}");
    assert_eq!(env.verify_tag(1), "verify=recovered");
    assert!(!env.snap(1).join("p/target").exists(), "leftover pruned");
    assert!(
        std::fs::write(env.snap(1).join("p/x"), "").is_err(),
        "and locked"
    );
    assert_eq!(env.verify_tag(2), "verify=ok");
}

#[test]
fn a_refused_path_is_logged_not_paged() {
    let env = Env::new();
    project(&env, "+ target\n+ src\n"); // `src` is tracked
    // Run 1 reports its rules as new (that tag wins over `refused`) ...
    let (code, out) = env.run_script();
    assert_eq!(
        (code, env.verify_tag(1)),
        (0, "verify=rules-changed".into()),
        "{out}"
    );
    assert!(
        out.contains("refused: ") && env.snap(1).join("p/src/main.rs").exists(),
        "{out}"
    );
    assert!(!env.snap(1).join("p/target").exists());
    // ... run 2 has nothing new: just `refused`.
    let (code, out) = env.run_script();
    assert_eq!(
        (code, env.verify_tag(2)),
        (0, "verify=refused".into()),
        "{out}"
    );
}

#[test]
fn a_run_that_died_after_locking_keeps_its_snapshot() {
    // QF-1/SF-1: read-only + still `pending` must be kept, never deleted.
    let env = Env::new();
    project(&env, "+ target\n");
    env.snapper(&[
        "create",
        "--read-write",
        "--description",
        "pruned",
        "--userdata",
        "verify=pending",
        "--print-number",
    ]);
    env.snapper(&["modify", "--read-only", "1"]);

    let (code, out) = env.run_script();
    assert_eq!(code, 0, "{out}");
    assert!(env.snap(1).exists(), "kept: {out}");
    assert_eq!(env.verify_tag(1), "verify=recovered");
    assert_eq!(env.verify_tag(2), "verify=ok");
}

#[test]
fn a_prune_error_keeps_its_snapshot_locked_and_pages() {
    let env = Env::new();
    project(&env, "+ target\n");
    // A broken vcs.toml: prune can't run (exit 2).
    let config = env.home.join(".config/ghostvolumes");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("vcs.toml"), "not = [toml").unwrap();
    let (code, out) = env.run_script();
    assert_eq!(code, 1, "{out}");
    // Never deleted: kept (unpruned), locked and tagged.
    assert_eq!(env.verify_tag(1), "verify=failed", "{out}");
    assert!(env.snap(1).join("p/target/big.o").exists());
    assert!(
        std::fs::write(env.snap(1).join("p/x"), "").is_err(),
        "locked"
    );
}

#[test]
fn a_held_lock_or_a_wrong_subvolume_creates_nothing() {
    use std::os::fd::AsRawFd;
    let env = Env::new();
    project(&env, "+ target\n");
    let state = env.home.join(".local/state/ghostvolumes");
    std::fs::create_dir_all(&state).unwrap();
    let lock = std::fs::File::create(state.join("src.lock")).unwrap();
    assert_eq!(unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX) }, 0);
    let (code, out) = env.run_script();
    assert_eq!(code, 0, "another run holds the lock: quietly skip ({out})");
    assert!(!env.snap(1).exists());
    drop(lock);

    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("contrib/ghostvolumes-snapshot");
    let out = env
        .command(Path::new("/bin/sh"))
        .arg(script)
        .arg("src")
        .arg("/nonexistent")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!env.snap(1).exists());
}

/// UTC "%F %T", `hours` from now (negative: ago).
fn utc_in(hours: i64) -> String {
    let t = std::time::SystemTime::now()
        .checked_add(std::time::Duration::from_secs(hours.max(0) as u64 * 3600))
        .and_then(|t| {
            t.checked_sub(std::time::Duration::from_secs(
                (-hours).max(0) as u64 * 3600,
            ))
        })
        .unwrap();
    humantime::format_rfc3339_seconds(t).to_string()[..19].replace('T', " ")
}

#[test]
fn a_decision_change_is_reported_through_the_window_then_stops() {
    let env = Env::new();
    if env.real {
        eprintln!("skipped with real snapper: needs back-dated snapshots");
        return;
    }
    project(&env, "+ target\n");
    env.write(".gitignore", "target/\nnotes/\n");
    env.write("notes/todo.md", "my untracked work");
    env.git(&["commit", "-qam", "ignore notes"]);
    let log = env.home.join(".local/state/ghostvolumes/events.log");
    // Run 1, two days ago: no baseline yet, everything reported once.
    let (_, out) = env.run_script_at(Some(&utc_in(-48)));
    assert_eq!(env.verify_tag(1), "verify=rules-changed", "{out}");
    std::fs::remove_file(&log).unwrap(); // read and cleared by the user

    // Upstream adds `+ /notes`: pruned anyway, but reported and logged.
    env.write(".ghostvolumes-decisions", "+ target\n+ /notes\n");
    env.git(&["commit", "-qam", "prune notes"]);
    let (code, out) = env.run_script_at(Some(&utc_in(-47)));
    assert_eq!(
        (code, env.verify_tag(2)),
        (0, "verify=rules-changed".into()),
        "{out}"
    );
    assert!(
        out.contains("will prune (decisions changed): p/notes (1 file(s), newest "),
        "{out}"
    );
    assert!(!env.snap(2).join("p/notes").exists());
    assert!(
        env.snap(1).join("p/notes/todo.md").exists(),
        "the earlier snapshot keeps its own rules: never re-pruned"
    );
    let text = std::fs::read_to_string(&log).unwrap();
    assert_eq!(text.lines().count(), 1, "{text}");
    assert!(
        text.contains("p/notes") && !text.contains("p/target"),
        "{text}"
    );

    // An hour later, still within a day of run 1: reported again.
    let (_, out) = env.run_script_at(Some(&utc_in(-46)));
    assert_eq!(env.verify_tag(3), "verify=rules-changed", "{out}");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);

    // Now: nothing in the last day, so the newest earlier one (3, which
    // already has the rule) is the baseline: quiet.
    let (code, out) = env.run_script();
    assert_eq!((code, env.verify_tag(4)), (0, "verify=ok".into()), "{out}");
    assert_eq!(std::fs::read_to_string(&log).unwrap().lines().count(), 2);

    // The login check reports the file until it's deleted.
    let check = Path::new(env!("CARGO_MANIFEST_DIR")).join("contrib/ghostvolumes-login-check");
    let login = || {
        let out = env
            .command(Path::new("/bin/sh"))
            .arg(&check)
            .arg("src")
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stderr).into_owned()
    };
    assert!(login().contains("2 event(s) in "), "{}", login());
    std::fs::remove_file(&log).unwrap();
    assert!(!login().contains("event(s)"), "{}", login());
}

#[test]
fn no_decision_file_means_nothing_pruned_and_ok() {
    let env = Env::new();
    env.write("src/main.rs", "fn main() {}");
    env.write("target/big.o", "o");
    env.git(&["init", "-q"]);
    let (code, out) = env.run_script();
    assert_eq!((code, env.verify_tag(1)), (0, "verify=ok".into()), "{out}");
    assert!(
        out.contains("note: no `+` decision matches anything"),
        "{out}"
    );
    assert!(env.snap(1).join("p/target/big.o").exists());
}

#[test]
fn a_decision_change_seen_only_by_a_crashed_run_is_still_reported() {
    // QS-1: the run that first sees `+ /notes` dies after `create`; the
    // recovery must compare with the snapshot before it.
    let env = Env::new();
    project(&env, "+ target\n");
    env.write(".gitignore", "target/\nnotes/\n");
    env.write("notes/todo.md", "mine");
    env.git(&["commit", "-qam", "ignore notes"]);
    let log = env.home.join(".local/state/ghostvolumes/events.log");
    env.run_script();
    std::fs::remove_file(&log).unwrap();

    env.write(".ghostvolumes-decisions", "+ target\n+ /notes\n");
    env.snapper(&[
        "create",
        "--read-write",
        "--description",
        "pruned",
        "--userdata",
        "verify=pending",
        "--print-number",
    ]);
    let (code, out) = env.run_script();
    assert_eq!(code, 0, "{out}");
    assert_eq!(env.verify_tag(2), "verify=recovered");
    let text = std::fs::read_to_string(&log).unwrap();
    // The recovery of 2 reported it (before deleting); run 3, within the
    // window of run 1, repeats it.
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines.len(), 2, "{text}");
    assert!(
        lines[0].contains(".snapshots/2/snapshot\twill prune (decisions changed): p/notes"),
        "{text}"
    );
    assert!(lines[1].contains(".snapshots/3/snapshot\twill prune (decisions changed): p/notes"));
}

#[test]
fn an_unwritable_events_log_never_costs_the_snapshot() {
    // QS-2: events.log can't be written (here: it's a directory).
    let env = Env::new();
    project(&env, "+ target\n");
    std::fs::create_dir_all(env.home.join(".local/state/ghostvolumes/events.log")).unwrap();
    let (code, out) = env.run_script();
    assert_eq!(
        (code, env.verify_tag(1)),
        (0, "verify=rules-changed".into()),
        "{out}"
    );
    assert!(out.contains("couldn't write the events log"), "{out}");
    assert!(!env.snap(1).join("p/target").exists(), "pruned and kept");
}

#[test]
fn a_failed_before_manifest_still_prunes_but_fails_and_pages() {
    // Q2: without the before-manifest there's no proof, but the guards
    // still hold: prune (or the snapshot keeps its volatile data for the
    // whole retention), skip the comparison, run health, tag `failed`.
    let env = Env::new();
    project(&env, "+ target\n");
    env.hook_ghostvolumes(
        r#"if [ "$1" = vcs-manifest ] && [ ! -e "$STATE/once" ]; then : >"$STATE/once"; exit 2; fi"#,
    );
    let (code, out) = env.run_script();
    assert_eq!(
        (code, env.verify_tag(1)),
        (1, "verify=failed".into()),
        "{out}"
    );
    assert!(out.contains("pruning without a before-manifest"), "{out}");
    assert!(
        !env.snap(1).join("p/target").exists(),
        "pruned anyway: {out}"
    );
    assert!(out.contains("ok: "), "health still ran: {out}");
    assert!(
        std::fs::write(env.snap(1).join("p/x"), "").is_err(),
        "locked"
    );
}

#[test]
fn changed_vcs_metadata_during_the_prune_is_manifest_changed_and_pages() {
    // The second manifest (after the prune) differs from the first.
    let env = Env::new();
    project(&env, "+ target\n");
    env.hook_ghostvolumes(
        r#"if [ "$1" = vcs-manifest ] && [ -e "$STATE/once" ]; then "$G" "$@"; echo extra; exit 0; fi
[ "$1" = vcs-manifest ] && : >"$STATE/once""#,
    );
    let (code, out) = env.run_script();
    assert_eq!(
        (code, env.verify_tag(1)),
        (1, "verify=manifest-changed".into()),
        "{out}"
    );
}

#[test]
fn a_baseline_that_isnt_a_real_snapshot_hides_nothing() {
    // L1: snapshot 1 replaced by a plain directory holding the new rules
    // would compare them with themselves; it's rejected, so everything is
    // reported (one summary line in the log).
    let env = Env::new();
    project(&env, "+ target\n");
    env.write(".gitignore", "target/\nnotes/\n");
    env.write("notes/todo.md", "mine");
    env.git(&["commit", "-qam", "ignore notes"]);
    env.run_script();
    let log = env.home.join(".local/state/ghostvolumes/events.log");
    std::fs::remove_file(&log).unwrap();
    let one = env.src.join(".snapshots/1");
    std::fs::rename(one.join("snapshot"), one.join("real")).unwrap();
    env.write(".ghostvolumes-decisions", "+ target\n+ /notes\n");
    std::fs::create_dir_all(one.join("snapshot/p")).unwrap();
    std::fs::copy(
        env.src.join("p/.ghostvolumes-decisions"),
        one.join("snapshot/p/.ghostvolumes-decisions"),
    )
    .unwrap();
    let (code, out) = env.run_script();
    assert_eq!(code, 0, "{out}");
    // Either way the change is reported: the fake lists 1 as read-only, so
    // prune rejects the path ("baseline unavailable"); real snapper sees a
    // writable plain dir and doesn't offer it (no baseline: all reported).
    let text = std::fs::read_to_string(&log).unwrap();
    assert!(
        text.contains("baseline unavailable (") || text.contains("changed): p/notes"),
        "{text}"
    );
    btrfs::make_writable(&one.join("real"));
}
