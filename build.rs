//! Compiles `shim/preload.rs` via bare `rustc` (never `cargo build` —
//! see plan §8.1 for why) into `$OUT_DIR/libghostvolumes_shim.so` —
//! named for identifiability (in `LD_PRELOAD`, `ps`, `/proc/*/maps`,
//! `preload_guard`'s own error message, etc.), not a generic
//! `preload.so` some other tool could also be using. `src/init.rs`
//! embeds the result via `include_bytes!`. This is the only place
//! `rustc` is invoked for the shim: `ghostvolumes init` (run by the
//! user after install) just extracts the already-compiled bytes.
//!
//! `SHIM_FILE_NAME` is defined once, here, and exposed to the rest of
//! the crate via `cargo:rustc-env` — `env!("GHOSTVOLUMES_SHIM_FILE_NAME")`
//! in `src/filenames.rs`/`src/init.rs` reads it back at their own
//! compile time. This is the one filename `build.rs` is a genuine
//! authority for (it's the thing that writes it), so it's the one
//! filename constant that belongs here rather than in
//! `src/filenames.rs` alongside the others — a `const` reference can't
//! cross from `build.rs` into the main crate's own compilation any
//! other way, since a build script runs as an entirely separate
//! program *before* the crate it's building even starts compiling.

use std::env;
use std::path::PathBuf;
use std::process::Command;

const SHIM_FILE_NAME: &str = "libghostvolumes_shim.so";

fn main() {
    // Emitted unconditionally, before the non-Linux early return below
    // - cheap, and avoids a future landmine where some as-yet-unwritten
    // non-Linux-reachable code references
    // `env!("GHOSTVOLUMES_SHIM_FILE_NAME")` and gets a confusing build
    // failure because this line never ran for that target.
    println!("cargo:rustc-env=GHOSTVOLUMES_SHIM_FILE_NAME={SHIM_FILE_NAME}");

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
    let shim_so = PathBuf::from(&out_dir).join(SHIM_FILE_NAME);

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
