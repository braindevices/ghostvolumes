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

use vergen_gitcl::{Emitter, Gitcl};

const SHIM_FILE_NAME: &str = "libghostvolumes_shim.so";

/// The current branch name via `git rev-parse --abbrev-ref HEAD` -
/// `vergen-gitcl` emits `VERGEN_GIT_BRANCH` too, but only as a
/// `cargo:rustc-env` instruction for the *final crate* to read via
/// `env!()`; that's not readable back here, mid-build-script, so the
/// branch-suffix decision below needs its own independent git
/// invocation. `None` if detached (bare `git rev-parse --abbrev-ref
/// HEAD` prints the literal string "HEAD" in that case, e.g. exactly
/// at a tag via `cargo install --git --tag`) or if `git`/`.git` aren't
/// available at all - both treated as "no branch-based opinion,"
/// which resolves to the same empty, release-shaped suffix as `main`.
fn current_branch() -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let branch = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if branch.is_empty() || branch == "HEAD" {
        return None;
    }
    Some(branch)
}

/// This project's GitFlow-shaped branch model (`.github/workflows/ci.yml`:
/// `main` = release, `develop` = pre-release, plus `hotfix/*`/`feature/*`)
/// mapped onto SemVer pre-release suffixes, so `--version` distinguishes
/// which line of development a build came from at a glance - on top of
/// (not instead of) `VERGEN_GIT_DESCRIBE`'s own "how far from a tag"
/// signal, which alone can't tell `develop` and `main` apart if both
/// happen to sit the same distance from their nearest tag.
fn version_suffix(branch: Option<&str>) -> &'static str {
    match branch {
        Some("main") | Some("master") | None => "",
        Some("develop") => "-alpha",
        Some(b) if b.starts_with("hotfix/") => "-rc",
        Some(_) => "-dev",
    }
}

fn main() {
    // Emitted unconditionally, before the non-Linux early return below
    // - cheap, and avoids a future landmine where some as-yet-unwritten
    // non-Linux-reachable code references
    // `env!("GHOSTVOLUMES_SHIM_FILE_NAME")` and gets a confusing build
    // failure because this line never ran for that target.
    println!("cargo:rustc-env=GHOSTVOLUMES_SHIM_FILE_NAME={SHIM_FILE_NAME}");

    // `VERGEN_GIT_DESCRIBE` (and friends) for main.rs's `--version`
    // string - `vergen-gitcl` (shells out to the `git` CLI) rather than
    // `vergen-gix`/`vergen-git2`: `git` is already a hard prerequisite
    // for this project's only supported install path (`cargo install
    // --git` needs it just to clone), so shelling out to it here costs
    // nothing extra in practice, unlike `vergen-gix`'s huge pure-Rust
    // `gix` dependency tree (~500 transitive crates vs. ~50) or
    // `vergen-git2`'s libgit2 C dependency. Emitted unconditionally too,
    // same reasoning as above. Doesn't fail the build if `.git` is
    // missing (e.g. a tarball export rather than the `cargo install
    // --git` checkout this project actually supports) - `Emitter`'s
    // default is to emit an idempotent placeholder instead of erroring,
    // so `env!("VERGEN_GIT_DESCRIBE")` in main.rs is always defined,
    // just not always meaningful.
    Emitter::default()
        .add_instructions(&Gitcl::all_git())
        .expect("failed to configure vergen-gitcl git instructions")
        .emit()
        .expect("failed to emit vergen-gitcl build instructions");

    let suffix = version_suffix(current_branch().as_deref());
    println!("cargo:rustc-env=GHOSTVOLUMES_VERSION_SUFFIX={suffix}");

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
        "filenames_core.rs",
        "lock_core.rs",
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
