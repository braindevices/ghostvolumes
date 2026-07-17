# Condense source code comments

## Problem

124 of 183 rustdoc (`///`/`//!`) comment blocks across `src/`+`shim/`
exceed 3 lines (longest: 98 lines, `src/convert.rs:1`). A further 65
plain `//` comment blocks over 3 lines exist too (mostly file-header
explanations). Concentrated in `src/convert.rs` (38 blocks combined),
`shim/preload.rs` (27), `shim/decision_core.rs` (31).

## Scope (confirmed with user)

- Both `///`/`//!` doc comments and plain `//` comments.
- Rust source only — `documents/*.md`/`README.md` already condensed
  this session (Steps 9-12), out of scope for now.
- Trim aggressively: drop historical/rationale narrative entirely
  (git history and `documents/design.md`/`ai-work/tasks/*.plan.md`
  already preserve it) rather than relocating it anywhere. Do **not**
  drop active correctness-relevant notes a future reader needs to not
  break the code (safety invariants on `unsafe` blocks, "order matters
  because X" notes describing *current* required behavior) — those
  aren't history, they're load-bearing.
- Target: most comment blocks ≤3 lines at ~80 columns. Some
  legitimate exceptions expected (e.g. a table-like list of cases);
  err toward shorter.

## Approach

No logic changes — comment text only, so no test behavior changes
expected. Still run `cargo fmt`/`cargo clippy --all-targets --all-features
-- -D warnings`/`cargo test` after each group to catch any accidental
breakage (e.g. a doctest inside a doc comment, though a scan shows none
in this codebase).

Split into 5 groups by file, each independently condensable (no
cross-file coordination needed), verified and committed as its own
step:

1. `src/convert.rs` alone (largest/most critical single file).
2. `shim/preload.rs` + `shim/decision_core.rs` (shim's core logic).
3. Remaining `shim/*.rs` (`debug_core.rs`, `filenames_core.rs`,
   `cache_core.rs`, `btrfs_core.rs`, `lock_core.rs`,
   `project_roots_core.rs`, `xdg_core.rs`).
4. `src/discover.rs`, `src/main.rs`, `src/merge.rs`, `src/config.rs`.
5. Remaining `src/*.rs` (`preload_guard.rs`, `shellinit.rs`,
   `projects.rs`, `intercept.rs`, `cache.rs`, `filenames.rs`,
   `reload.rs`, `scan.rs`, `test_support.rs`, `btrfs.rs`,
   `mountinfo.rs`, `project_roots.rs`, `xdg.rs`, `init.rs`,
   `decision.rs`, `debug.rs`, `atomic_write.rs`).

## Steps

1. Group 1: `src/convert.rs`.
2. Group 2: `shim/preload.rs`, `shim/decision_core.rs`.
3. Group 3: remaining `shim/*.rs`.
4. Group 4: `src/discover.rs`, `src/main.rs`, `src/merge.rs`, `src/config.rs`.
5. Group 5: remaining `src/*.rs`.
6. Final full-repo verification pass (`fmt`+`clippy`+`test`) and a
   re-run of the block-length survey to confirm the guideline is met.
