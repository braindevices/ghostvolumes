# Linear versioning + manual release workflow

Two changes, agreed on across the last several turns of design discussion:

1. **Simplify `compute_version` to linear flow.** The GitFlow-shaped
   branch-conditional version scheme (`develop` → `-alpha`, `hotfix/*`
   → `-rc`, etc.) is being dropped in favor of: `GHOSTVOLUMES_VERSION`
   is just `CARGO_PKG_VERSION`, unconditionally, on every branch. Real
   releases only ever happen on `main` (via the new release workflow
   below), which keeps `Cargo.toml` and the release tag in lockstep by
   construction — so there's no more drift for branch logic to correct
   for, and no separate pre-release tags are being cut for
   `develop`/`hotfix/*`/`feature/*` (those branches still exist and
   still drive `ci.yml`'s existing triggers - that's an organizational/
   CI-lane choice, decoupled from versioning now).
2. **Put the branch name in the debug/bug-report string, not the
   version number.** `vergen-gitcl` already emits `VERGEN_GIT_BRANCH`
   for exactly this - the version string becomes
   `<CARGO_PKG_VERSION> (<VERGEN_GIT_DESCRIBE>, <branch>)`, e.g.
   `0.8.1 (v0.8.0-3-ga85de1e, feature/foo)`. Purely informational,
   no semver meaning.
3. **Add a manual `workflow_dispatch` release job** using `cargo-release`
   (confirmed installed locally, v1.1.3) instead of a hand-rolled
   script - the user doesn't want to maintain custom bump logic.
   Verified via real dry-runs in a disposable worktree today:
   `cargo release patch --no-publish` correctly proposes 0.8.0→0.8.1,
   skips the whole package/verify/upload chain with `--no-publish`
   (there's nothing to publish to - not on crates.io), and its default
   `tag-name = "{{prefix}}v{{version}}"` already produces bare `vX.Y.Z`
   matching this repo's tag convention with no override needed.
   Confirmed NOT auto-triggered on every merge (per earlier discussion:
   tag/commit noise, redundant with what `git describe` already gives
   for free, self-trigger risk) - `workflow_dispatch` only, human
   decides when and what level.

## Design

### `build_version_core.rs` → deleted entirely
`parse_tag`/`compute_version`/their tests all only existed to support
the branch-conditional scheme. Nothing in the linear model needs them.

### `build.rs`
- Delete `current_branch()` and `latest_tag_version()` (both git
  shell-outs existed only to feed `compute_version`).
- Delete the `include!("build_version_core.rs")` line.
- Delete the three `println!("cargo:warning=...")` debug lines (they
  reference `current_branch()`/`latest_tag_version()`, which are gone;
  they were the user's own ad hoc debug printout, no longer needed now
  that the real fix has landed).
- `GHOSTVOLUMES_VERSION` env var is no longer emitted at all — no
  longer needed, since `main.rs` can read `CARGO_PKG_VERSION` directly
  via `env!()`, same as any other crate.

### `src/main.rs`
- Delete the `#[cfg(test)] mod build_version_check { include!(...) }`
  block (nothing left to test).
- `VERSION` const becomes:
  ```rust
  const VERSION: &str = concat!(
      env!("CARGO_PKG_VERSION"),
      " (",
      env!("VERGEN_GIT_DESCRIBE"),
      ", ",
      env!("VERGEN_GIT_BRANCH"),
      ")"
  );
  ```
  Doc comment updated to describe this as debug/bug-report metadata,
  not a semver derivation.

### `Cargo.lock`
Fix the pre-existing drift found while testing (`ghostvolumes` self-
entry pinned at stale `0.3.2` vs. `Cargo.toml`'s `0.8.0`) - leftover
from the earlier main reset. `cargo check` resolves it.

### `.github/workflows/release.yml` (new)
- Trigger: `workflow_dispatch` with inputs `level` (choice:
  patch/minor/major/rc/beta/alpha/release, default `patch`) and
  `execute` (boolean, default `false` - dry-run unless explicitly
  flipped, mirroring `cargo-release`'s own `-x`/`--execute` safety
  default).
- `actions/checkout@v7` with `fetch-depth: 0` (full history + tags -
  the one workflow in this repo that actually needs it).
- Install `cargo-release` via `taiki-e/install-action@v2` (prebuilt
  binary, avoids a multi-minute `cargo install` compile on every run).
- Configure git identity for the release commit
  (`github-actions[bot]`).
- Run `cargo release ${{ inputs.level }} --no-publish` plus `-x` only
  when `inputs.execute == 'true'`.
- `permissions: contents: write` (needed to push the commit + tag).

### `release.toml` (new, repo root)
- `allow-branch = ["main"]` - refuses to run anywhere else (this is
  cargo-release's built-in equivalent of the hand-rolled script's
  branch check from before).
- `pre-release-hook = ["cargo", "test"]` - `--no-publish` skips
  cargo-release's own build-verify step entirely, so this is the
  actual safety gate before tagging.
- `pre-release-commit-message = "chore(release): v{{version}}"`.
- Leave `tag-name`/`push`/`push-remote` at their defaults (already
  correct, confirmed via `cargo release config`).

## Steps

1. This plan + progress file.
2. Delete `build_version_core.rs`; strip its `include!`s and the dead
   git-shelling functions from `build.rs`; drop the debug
   `cargo:warning` printouts.
3. Update `src/main.rs`'s `VERSION` const + doc comment; delete the
   now-empty test module.
4. Fix `Cargo.lock`'s stale self-entry.
5. `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` +
   `cargo test`, then a manual `cargo build` + `--version` smoke test
   confirming the new format.
6. Add `release.toml` + `.github/workflows/release.yml`; validate the
   workflow's shape with a dry-run `cargo release patch --no-publish`
   locally against this branch (already spot-checked in a disposable
   worktree during design - re-confirm against the real changes).
7. Commit on `claude-release-workflow`, one commit per logical step
   as usual.
