//! Decision-file parsing and matching (replaces the git-tracked gate).
//! Fully implemented in `shim/decision_core.rs` (this file just pulls
//! it in verbatim) since it's already dependency-free and shared with
//! the LD_PRELOAD shim — see that file's doc comment for why.

include!("../shim/decision_core.rs");
