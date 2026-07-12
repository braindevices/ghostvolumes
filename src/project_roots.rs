//! Project-roots list: a plain-text file of registered project-root
//! paths, giving the decision-file walk-up a narrower stopping
//! boundary. Fully implemented in `shim/project_roots_core.rs` (this
//! file just pulls it in verbatim) since it's already dependency-free
//! and shared with the LD_PRELOAD shim — see that file's doc comment
//! for why.

include!("../shim/project_roots_core.rs");
