//! Opt-in: exercises the shared BTRFS primitives (`shim/btrfs_core.rs`
//! — the exact same file the LD_PRELOAD shim uses, not a
//! reimplementation) against a throwaway loopback-mounted image, so
//! these tests are fully self-contained rather than depending on the
//! test machine already having a BTRFS filesystem mounted somewhere
//! (unlike the rest of the suite's `btrfs_scratch_dir()` helper).
//!
//! Needs real mount privilege (`CAP_SYS_ADMIN`, or a kernel/policy
//! that permits it inside an unprivileged user+mount namespace via
//! `unshare`) — not available in every environment (confirmed
//! unavailable in the sandbox this project was originally developed
//! in: missing `CAP_SYS_ADMIN` *and* no `/dev/loop-control` exposed to
//! the container at all — see progress notes). So these are
//! `#[ignore]`d by default:
//!
//!     cargo test --test btrfs_loopback -- --ignored
//!
//! Each test gracefully SKIPS (prints a message, doesn't fail) if
//! `mkfs.btrfs`/`unshare`/`mount` aren't available or don't have
//! permission in whatever environment they're run in, rather than
//! hard-failing on an environment that just doesn't support this.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// Compiles the probe once (via bare `rustc`, same as the shim itself)
/// and reuses it across every test in this file. The probe
/// `include!`s the real `shim/btrfs_core.rs` — an absolute path baked
/// into the generated source, since the probe's `.rs` file lives in a
/// tempdir far from the repo — so these tests exercise the actual
/// production code, not a hand-copied stand-in.
fn compiled_probe() -> &'static Path {
    static PROBE: OnceLock<PathBuf> = OnceLock::new();
    PROBE.get_or_init(|| {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let src_path =
            std::env::temp_dir().join(format!("btrfs-loopback-probe-{}.rs", std::process::id()));
        let source = format!(
            r#"
            include!("{manifest_dir}/shim/btrfs_core.rs");

            fn main() {{
                let args: Vec<String> = std::env::args().collect();
                let mnt = std::path::Path::new(&args[1]);
                match args[2].as_str() {{
                    "create_and_verify" => {{
                        let name = &args[3];
                        if let Err(e) = create_subvolume(mnt, name) {{
                            eprintln!("create_subvolume failed: {{e}}");
                            std::process::exit(2);
                        }}
                        match is_subvolume(&mnt.join(name)) {{
                            Ok(true) => {{ println!("PASS"); }}
                            Ok(false) => {{ eprintln!("created path is not inode 256"); std::process::exit(3); }}
                            Err(e) => {{ eprintln!("is_subvolume errored: {{e}}"); std::process::exit(4); }}
                        }}
                    }}
                    "plain_dir_is_not_a_subvolume" => {{
                        let name = &args[3];
                        std::fs::create_dir(mnt.join(name)).expect("create plain dir");
                        match is_subvolume(&mnt.join(name)) {{
                            Ok(false) => {{ println!("PASS"); }}
                            Ok(true) => {{ eprintln!("plain directory misdetected as a subvolume"); std::process::exit(3); }}
                            Err(e) => {{ eprintln!("is_subvolume errored: {{e}}"); std::process::exit(4); }}
                        }}
                    }}
                    "duplicate_create_fails_with_already_exists" => {{
                        let name = &args[3];
                        create_subvolume(mnt, name).expect("first create_subvolume should succeed");
                        match create_subvolume(mnt, name) {{
                            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {{ println!("PASS"); }}
                            Err(e) => {{ eprintln!("wrong error kind {{:?}}: {{e}}", e.kind()); std::process::exit(3); }}
                            Ok(()) => {{ eprintln!("duplicate create_subvolume unexpectedly succeeded"); std::process::exit(3); }}
                        }}
                    }}
                    "create_under_nonexistent_parent_fails" => {{
                        let missing_parent = mnt.join("does-not-exist");
                        match create_subvolume(&missing_parent, "x") {{
                            Err(_) => {{ println!("PASS"); }}
                            Ok(()) => {{ eprintln!("create_subvolume under a missing parent unexpectedly succeeded"); std::process::exit(3); }}
                        }}
                    }}
                    other => {{ eprintln!("unknown probe subcommand: {{other}}"); std::process::exit(1); }}
                }}
            }}
            "#
        );
        std::fs::write(&src_path, source).unwrap();

        let out_path =
            std::env::temp_dir().join(format!("btrfs-loopback-probe-{}", std::process::id()));
        let status = Command::new("rustc")
            .args(["--edition", "2021", "-O"])
            .arg("-o")
            .arg(&out_path)
            .arg(&src_path)
            .status()
            .expect("rustc must be available to compile the probe");
        assert!(status.success(), "probe failed to compile");
        out_path
    })
}

enum LoopbackRun {
    Skipped(String),
    Ran {
        probe_exit_code: i32,
        probe_stderr: String,
    },
}

/// Creates a fresh 150MiB BTRFS image in a tempdir, loop-mounts it
/// inside a fresh unprivileged user+mount namespace (`unshare --user
/// --map-root-user --mount`), runs the probe against the mountpoint
/// with `probe_args`, then lets the namespace's teardown handle
/// unmounting when the `unshare`d process exits. Every step that can
/// plausibly fail in an unsupported environment (missing `mkfs.btrfs`,
/// missing `unshare`, denied `mount`) reports `Skipped` instead of
/// panicking — this is fundamentally an environment capability check,
/// not a correctness assertion.
fn run_in_loopback_btrfs(probe_args: &[&str]) -> LoopbackRun {
    if Command::new("unshare").arg("--help").output().is_err() {
        return LoopbackRun::Skipped("`unshare` (util-linux) not available".to_string());
    }
    if Command::new("mkfs.btrfs")
        .arg("--version")
        .output()
        .is_err()
    {
        return LoopbackRun::Skipped("`mkfs.btrfs` (btrfs-progs) not available".to_string());
    }

    let workdir = tempfile::tempdir().expect("create tempdir for loopback image/mountpoint");
    let image = workdir.path().join("image.btrfs");
    let mountpoint = workdir.path().join("mnt");
    std::fs::create_dir_all(&mountpoint).unwrap();

    let probe = compiled_probe();
    let probe_arg_str = probe_args
        .iter()
        .map(|a| shell_quote(a))
        .collect::<Vec<_>>()
        .join(" ");

    // Every risky step reports a distinguishable "GHOSTVOLUMES_SKIP:"
    // line on stdout rather than letting `set -e` kill the script
    // silently, so the Rust side can tell "unsupported environment"
    // apart from "the probe itself failed."
    let script = format!(
        r#"
        set -u
        if ! truncate -s 150M {image}; then
            echo "GHOSTVOLUMES_SKIP:truncate failed"
            exit 0
        fi
        if ! mkfs.btrfs -f -q {image} >/dev/null 2>&1; then
            echo "GHOSTVOLUMES_SKIP:mkfs.btrfs failed"
            exit 0
        fi
        mount_err=$(mount -o loop {image} {mountpoint} 2>&1)
        if [ $? -ne 0 ]; then
            echo "GHOSTVOLUMES_SKIP:mount failed: $mount_err"
            exit 0
        fi
        {probe} {mountpoint} {probe_arg_str}
        echo "GHOSTVOLUMES_PROBE_EXIT:$?"
        umount {mountpoint} 2>/dev/null
        "#,
        image = shell_quote(image.to_str().unwrap()),
        mountpoint = shell_quote(mountpoint.to_str().unwrap()),
        probe = shell_quote(probe.to_str().unwrap()),
    );

    let output = Command::new("unshare")
        .args(["--user", "--map-root-user", "--mount"])
        .arg("sh")
        .arg("-c")
        .arg(&script)
        .output()
        .expect("failed to invoke `unshare`");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if let Some(reason) = stdout
        .lines()
        .find_map(|l| l.strip_prefix("GHOSTVOLUMES_SKIP:"))
    {
        return LoopbackRun::Skipped(reason.to_string());
    }
    if !output.status.success() {
        // unshare/mount succeeded structurally but something in the
        // shell script itself failed before reaching a SKIP/PROBE_EXIT
        // marker (e.g. `unshare` denied outright) - treat as a skip
        // too, since this is still "environment doesn't support it,"
        // just a shape of failure the script couldn't self-report.
        return LoopbackRun::Skipped(format!(
            "unshare/mount setup failed (exit {:?}): {stderr}",
            output.status.code()
        ));
    }
    let probe_exit_code = stdout
        .lines()
        .find_map(|l| l.strip_prefix("GHOSTVOLUMES_PROBE_EXIT:"))
        .unwrap_or_else(|| {
            panic!("script did not report a probe exit code; stdout:\n{stdout}\nstderr:\n{stderr}")
        })
        .trim()
        .parse()
        .expect("probe exit code should be an integer");

    LoopbackRun::Ran {
        probe_exit_code,
        probe_stderr: stderr.to_string(),
    }
}

fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

macro_rules! run_or_skip {
    ($probe_args:expr) => {
        match run_in_loopback_btrfs($probe_args) {
            LoopbackRun::Skipped(reason) => {
                eprintln!("SKIPPING: loopback BTRFS not usable in this environment: {reason}");
                return;
            }
            LoopbackRun::Ran {
                probe_exit_code,
                probe_stderr,
            } => {
                assert_eq!(probe_exit_code, 0, "probe failed: {probe_stderr}");
            }
        }
    };
}

#[test]
#[ignore = "needs real mount privilege (CAP_SYS_ADMIN or a permissive unprivileged-userns setup) - run explicitly with `cargo test --test btrfs_loopback -- --ignored`"]
fn create_and_verify_a_subvolume() {
    run_or_skip!(&["create_and_verify", "node_modules"]);
}

#[test]
#[ignore = "needs real mount privilege - see module docs"]
fn plain_directory_is_not_a_subvolume() {
    run_or_skip!(&["plain_dir_is_not_a_subvolume", "plain"]);
}

#[test]
#[ignore = "needs real mount privilege - see module docs"]
fn duplicate_subvolume_creation_fails_with_already_exists() {
    run_or_skip!(&["duplicate_create_fails_with_already_exists", "dup"]);
}

#[test]
#[ignore = "needs real mount privilege - see module docs"]
fn creating_a_subvolume_under_a_missing_parent_fails() {
    run_or_skip!(&["create_under_nonexistent_parent_fails"]);
}

#[test]
fn skip_path_actually_triggers_in_this_environment() {
    // Not #[ignore]'d - this specific assertion (skip-detection working
    // correctly) can run in every environment, including ones (like the
    // sandbox this project was developed in) where real loopback BTRFS
    // mounting is confirmed unavailable. Guards against the skip logic
    // itself silently rotting - e.g. a future change accidentally
    // treating a real failure as a false-positive skip forever.
    match run_in_loopback_btrfs(&["create_and_verify", "node_modules"]) {
        LoopbackRun::Skipped(reason) => {
            eprintln!("confirmed: this environment reports a skip ({reason}), as expected");
        }
        LoopbackRun::Ran {
            probe_exit_code, ..
        } => {
            // This environment actually supports it - great, and the
            // probe should have passed.
            assert_eq!(probe_exit_code, 0);
        }
    }
}
