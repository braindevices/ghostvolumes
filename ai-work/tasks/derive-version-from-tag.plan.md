# Derive `GHOSTVOLUMES_VERSION` from the latest tag on main/master

`compute_version` (`build_version_core.rs`) currently trusts
`Cargo.toml`'s `version` field verbatim on `main`/`master`/detached
HEAD, on the assumption a human bumps it by hand at tag time. That
assumption broke in practice: `Cargo.toml` is stuck at `0.3.2` while
tags `v0.4.0`..`v0.8.0` were cut without ever bumping it, so
`GHOSTVOLUMES_VERSION` (`0.3.2`) and `VERGEN_GIT_DESCRIBE`
(`v0.8.0-1-ga85de1e`) visibly disagree in `--version` output.

Fix: on `main`/`master`/`None`, format the *latest reachable tag*
directly (`{major}.{minor}.{patch}`) instead of trusting
`cargo_pkg_version`. `cargo_pkg_version` remains the fallback only when
no tag is reachable at all (fresh repo, shallow/tagless CI checkout,
or a `cargo install --git` checkout — Cargo's git fetch never pulls
`refs/tags/*`, a separate, already-known limitation this change does
not touch).

Working directly on `main` (not a feature branch) for this one,
deliberately: `compute_version`'s own branching logic is keyed off the
literal git branch name, and a manual end-to-end smoke test
(`cargo build` + checking real `--version` output) is only meaningful
when actually checked out on `main`. The unit tests below don't need
this — they call `compute_version(Some("main"), ...)` with an explicit
branch string, not derived from git state.

## Design

- `build_version_core.rs::compute_version`: `Some("main") |
  Some("master") | None` arm becomes:
  ```rust
  match tag {
      Some((major, minor, patch)) => format!("{major}.{minor}.{patch}"),
      None => cargo_pkg_version.to_string(),
  }
  ```
- Update the arm's doc comment: drop the "human bumps Cargo.toml at
  tag time" framing, replace with "trusts the latest tag directly;
  `cargo_pkg_version` is only the no-tag fallback."
- Tests (`build_version_core.rs`'s `#[cfg(test)] mod tests`):
  - Replace `main_uses_cargo_pkg_version_unchanged` (currently passes
    by coincidence since its fixture has tag == pkg version) with a
    case where tag and `cargo_pkg_version` *disagree*, proving the tag
    wins: `compute_version(Some("main"), Some((0, 8, 0)), "0.3.2") ==
    "0.8.0"`. Same for `None` branch (detached HEAD).
  - Add `main_falls_back_to_cargo_pkg_version_without_a_tag`:
    `compute_version(Some("main"), None, "0.3.2") == "0.3.2"`.
- `Cargo.toml`: bump `version` to `0.8.0` to match the latest tag —
  now purely a fallback value, so it should still reflect reality.

## Steps

1. This plan + progress file.
2. `build_version_core.rs`: implement the arm change + doc comment,
   replace/add the three test cases above.
3. `cargo fmt --check` + `cargo clippy --all-targets -- -D warnings` +
   `cargo test` clean.
4. Bump `Cargo.toml` version to `0.8.0`.
5. Manual smoke test: `cargo build` on `main`, run `./target/debug/ghostvolumes --version`,
   confirm it now reads `0.8.0 (v0.8.0-1-g<hash>)` (or whatever the
   tip commit's describe output is at that point).
6. Commit on `main` directly (deliberate exception to the usual
   `claude-<task>` branch convention — see rationale above).
