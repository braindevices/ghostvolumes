# Derive `GHOSTVOLUMES_VERSION` from the latest tag on main/master — progress

## Step 1 — Plan + progress scaffolding
**Status**: done
**Date**: 2026-07-22
### What was done
Wrote `derive-version-from-tag.plan.md` and this progress file.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 2 — `compute_version` change + tests
**Status**: done
**Date**: 2026-07-22
### What was done
`main`/`master`/`None` arm now formats the latest reachable tag
directly (`format!("{major}.{minor}.{patch}")`), falling back to
`cargo_pkg_version` only when no tag is reachable. Doc comment
updated to drop the "human bumps Cargo.toml" framing. Replaced
`main_uses_cargo_pkg_version_unchanged` with
`main_uses_latest_tag_even_when_cargo_pkg_version_disagrees` (proves
tag wins even when it disagrees with `cargo_pkg_version`) and added
`main_falls_back_to_cargo_pkg_version_without_a_tag`.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 3 — fmt/clippy/test clean
**Status**: done
**Date**: 2026-07-22
### What was done
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, and
full `cargo test` all clean; the two new/changed
`build_version_core.rs` tests pass explicitly.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 4 — Bump `Cargo.toml` version
**Status**: done
**Date**: 2026-07-22
### What was done
Bumped `Cargo.toml` `version` from `0.3.2` to `0.8.0` to match the
latest tag (`v0.8.0`). `Cargo.lock`'s matching entry updated by the
subsequent `cargo build`.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 5 — Manual smoke test (`cargo build` + `--version`)
**Status**: done
**Date**: 2026-07-22
### What was done
On `main`: `cargo build`, then `./target/debug/ghostvolumes --version`
→ `ghostvolumes 0.8.0 (v0.8.0-1-ga85de1e)`. Both halves now agree
(latest tag `v0.8.0`, one commit past it), confirming the fix.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 6 — Commit
**Status**: blocked
**Date**:
### What was done
Held per explicit instruction: implement but do not commit — changes
left staged/uncommitted on `main` for the user to review and commit
themselves. Working tree currently has: `Cargo.toml`, `Cargo.lock`,
`build_version_core.rs` modified; the plan and this progress file
untracked.
### Deviations from plan
None.
### Issues found / fixed
None.
