//! Build-time metadata only: `vergen-gitcl` emits the git describe /
//! branch shown by `--version`. (Until the snapshot-prune redesign this
//! also compiled the LD_PRELOAD shim; that's gone.)

use vergen_gitcl::{Emitter, Gitcl};

fn main() {
    // `VERGEN_GIT_DESCRIBE` and `VERGEN_GIT_BRANCH` for main.rs's
    // `--version` string - purely informational debug/bug-report
    // metadata (exact commit + branch name), not part of the version
    // number itself; `CARGO_PKG_VERSION` is always trusted verbatim for
    // that, since real releases only ever bump it on `main`, in
    // lockstep with the release tag (see `release.toml`/`release.yml`).
    // `vergen-gitcl` (shells out to the `git` CLI) rather than
    // `vergen-gix`/`vergen-git2`: `git` is already a hard prerequisite
    // for this project's only supported install path (`cargo install
    // --git` needs it just to clone), so shelling out to it here costs
    // nothing extra in practice, unlike `vergen-gix`'s huge pure-Rust
    // `gix` dependency tree (~500 transitive crates vs. ~50) or
    // `vergen-git2`'s libgit2 C dependency. Doesn't fail the build if `.git` is
    // missing (e.g. a tarball export rather than the `cargo install
    // --git` checkout this project actually supports) - `Emitter`'s
    // default is to emit an idempotent placeholder instead of erroring,
    // so `env!("VERGEN_GIT_DESCRIBE")`/`env!("VERGEN_GIT_BRANCH")` in
    // main.rs are always defined, just not always meaningful.
    Emitter::default()
        .add_instructions(&Gitcl::all_git())
        .expect("failed to configure vergen-gitcl git instructions")
        .emit()
        .expect("failed to emit vergen-gitcl build instructions");
}
