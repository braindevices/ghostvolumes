# Downloadable, ai-work/-free release source package — progress

## Step 1 — .gitattributes + verify export-ignore
**Status**: done
**Date**: 2026-07-23
### What was done
Added `.gitattributes` (`ai-work/** export-ignore`). Verified with
`git archive --worktree-attributes HEAD | tar -tf -`: `ai-work/`
present before the attribute existed, absent after.
### Deviations from plan
None.
### Issues found / fixed
`.gitattributes` isn't committed yet at the point of testing, so
plain `git archive HEAD` wouldn't see it (only committed content at a
ref is considered) - used `--worktree-attributes` to make `git
archive` also consult the working tree's `.gitattributes` for this
local check.

## Step 2 — release.yml: gh release create
**Status**: done
**Date**: 2026-07-23
### What was done
Added a `Create GitHub Release` step, gated on `inputs.execute`,
reading the post-bump version back out of `Cargo.toml` (`cargo
release` already committed it earlier in the job) and running
`gh release create "v$version" --generate-notes`. Verified the `sed`
extraction against the real `Cargo.toml` (`0.8.0`).
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 3 — README Installing section
**Status**: done
**Date**: 2026-07-23
### What was done
Extended the existing `## Install` section (didn't add a new heading,
so no doctoc TOC regeneration needed) with: how to pin `--tag
vX.Y.Z`, a note that `cargo install --git` includes `ai-work/`, and
the download-tarball-then-`cargo install --path` alternative that
excludes it.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 4 — fmt/clippy/test + re-verify
**Status**: done
**Date**: 2026-07-23
### What was done
`cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`,
`cargo test` all clean (no Rust changes this round, ran anyway per
standing practice). Re-ran the `.gitattributes` archive check against
the final tree - `ai-work/` still correctly excluded, everything else
(including the new `release.yml`) present.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 5 — Commit
**Status**: done
**Date**: 2026-07-23
### What was done
Committed `.gitattributes` + `release.yml` + `README.md` changes on
`claude-release-workflow`, then this plan/progress scaffolding.
### Deviations from plan
None.
### Issues found / fixed
None.
