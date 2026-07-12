//! Compiles `shim/preload.rs` via bare `rustc` (never `cargo build` —
//! see plan §8.1 for why) into `$OUT_DIR/preload.so`. `src/init.rs`
//! embeds the result via `include_bytes!`. This is the only place
//! `rustc` is invoked for the shim: `ghostvolumes init` (run by the
//! user after install) just extracts the already-compiled bytes.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let target = env::var("TARGET").unwrap();
    // On non-Linux, src/main.rs's Command::new mod (and everything
    // under it, including init.rs's include_bytes! of the shim) is
    // entirely cfg'd out — see plan §8.3 — so there's nothing to embed
    // and no point invoking rustc for a shim that will never be linked
    // in. This also means it's fine that the shim source itself relies
    // on Linux-only things (/proc/self/fd, BTRFS ioctls).
    if !target.contains("linux") {
        return;
    }

    let manifest_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let out_dir = env::var("OUT_DIR").unwrap();

    for file in [
        "preload.rs",
        "cache_core.rs",
        "decision_core.rs",
        "project_roots_core.rs",
        "btrfs_core.rs",
        "xdg_core.rs",
    ] {
        println!("cargo:rerun-if-changed=shim/{file}");
    }

    let shim_src = PathBuf::from(&manifest_dir).join("shim/preload.rs");
    let shim_so = PathBuf::from(&out_dir).join("preload.so");

    let mut cmd = Command::new("rustc");
    cmd.args([
        "--edition",
        "2021",
        "--crate-type",
        "cdylib",
        "-O",
        "-C",
        "panic=abort",
    ]);
    cmd.arg("--target").arg(&target);
    // musl targets default to fully static linking, which cdylib is
    // incompatible with (a static shim contradicts LD_PRELOAD's whole
    // premise of loading into the host process's own libc) - see plan
    // §8.1's libc-matching requirement.
    if target.contains("musl") {
        cmd.arg("-C").arg("target-feature=-crt-static");
    }
    cmd.arg("-o").arg(&shim_so).arg(&shim_src);

    let status = cmd
        .status()
        .expect("failed to invoke rustc to compile the LD_PRELOAD shim (shim/preload.rs)");
    if !status.success() {
        panic!("rustc failed to compile shim/preload.rs (see output above)");
    }
}
