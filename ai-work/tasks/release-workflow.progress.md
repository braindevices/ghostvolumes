# Linear versioning + manual release workflow — progress

## Step 1 — Plan + progress scaffolding
**Status**: done
**Date**: 2026-07-23
### What was done
Wrote `release-workflow.plan.md` and this progress file, on branch
`claude-release-workflow` (branched from `main` at `fix the version`).
### Deviations from plan
None.
### Issues found / fixed
While testing `cargo-release` locally (before this branch existed),
a `git worktree` test session accidentally committed a throwaway
commit onto the real local `main` (worktrees share the repo's refs -
`git checkout -B main` inside the worktree was the real `main`, not a
disposable one). Caught it, confirmed `origin/main` was untouched, and
the user confirmed `git branch -f main b673700` to restore local
`main` to its correct prior state before any of this branch's work
began.

## Step 2 — Delete build_version_core.rs, strip build.rs
**Status**: done
**Date**: 2026-07-23
### What was done
Deleted `build_version_core.rs`. Removed `current_branch()`,
`latest_tag_version()`, the `include!`, and the three debug
`cargo:warning=...` printouts from `build.rs`. `GHOSTVOLUMES_VERSION`
is no longer emitted at all. Updated the doc comment above the
`Emitter` call to describe `VERGEN_GIT_DESCRIBE`/`VERGEN_GIT_BRANCH` as
debug metadata, not version derivation.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 3 — src/main.rs VERSION const update
**Status**: done
**Date**: 2026-07-23
### What was done
Deleted the `#[cfg(test)] mod build_version_check` block. `VERSION`
const now `concat!(CARGO_PKG_VERSION, " (", VERGEN_GIT_DESCRIBE, ", ",
VERGEN_GIT_BRANCH, ")")`. Doc comment rewritten accordingly.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 4 — Fix Cargo.lock drift
**Status**: done
**Date**: 2026-07-23
### What was done
`cargo check` resolved `Cargo.lock`'s stale `ghostvolumes` self-entry
(`0.3.2` vs. `Cargo.toml`'s `0.8.0`) - leftover from the earlier main
reset.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 5 — fmt/clippy/test + manual smoke test
**Status**: done
**Date**: 2026-07-23
### What was done
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` (306+5+4+2+4+25, all passing) all clean. Manual smoke
test after a forced build-script cache clear: `--version` →
`ghostvolumes 0.8.0 (v0.8.0-5-gb673700, claude-release-workflow)`.
### Deviations from plan
None.
### Issues found / fixed
An earlier `cargo build` right after the code edits printed stale
`cargo:warning=...` lines from the *old* build.rs - turned out to be
Cargo replaying cached build-script output, not a real bug. Forcing a
clean rebuild (`rm -rf target/debug/build/ghostvolumes-*`) confirmed
the new build.rs actually produces the new, warning-free output.

## Step 6 — release.toml + release.yml workflow
**Status**: done
**Date**: 2026-07-23
### What was done
Added `release.toml` (`allow-branch = ["main"]`,
`pre-release-hook = ["cargo", "test"]`,
`pre-release-commit-message = "chore(release): v{{version}}"`) and
`.github/workflows/release.yml` (`workflow_dispatch` with `level` and
`execute` inputs, `actions/checkout@v7` with `fetch-depth: 0`,
`taiki-e/install-action@v2` for `cargo-release`, `permissions:
contents: write`).

Verified in a disposable worktree (careful this time not to reuse the
name `main` for the test branch, after the Step 1 mishap):
`cargo release config` correctly picked up the custom `release.toml`
values; running `cargo release patch --no-publish` on the
non-`main` test branch correctly refused with `allow-branch`'s error
message, and (since dry-run continues past that to show the full
plan) also confirmed `pre-release-hook` really invokes `cargo test`
(a few BTRFS-dependent tests failed only because `/tmp` isn't a real
BTRFS mount - expected, unrelated to the workflow itself).
### Deviations from plan
None.
### Issues found / fixed
None (beyond the Step 1 worktree/branch-name mishap, already logged).

## Step 7 — Commit
**Status**: done
**Date**: 2026-07-23
### What was done
Three commits on `claude-release-workflow`: versioning simplification,
release workflow addition, and this plan/progress scaffolding.
### Deviations from plan
None.
### Issues found / fixed
None.
