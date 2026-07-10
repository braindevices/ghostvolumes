//! Git-tracked gate (§4): never touch a path git already tracks.
//! Applied, per the plan, at all three call sites (cd-hook, LD_PRELOAD,
//! `convert`). Fully implemented in `shim/git_core.rs` (this file just
//! pulls it in verbatim) since it's already dependency-free and shared
//! with the LD_PRELOAD shim — see that file's doc comment for why.

include!("../shim/git_core.rs");
