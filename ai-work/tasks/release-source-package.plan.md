# Downloadable, ai-work/-free release source package

Continues on `claude-release-workflow` (same branch, same theme).

## Constraint established first (see conversation)

`cargo install --git <url>` always clones the whole tracked git tree
— there is no supported way to exclude a directory from what that
pulls down (`Cargo.toml`'s `include`/`exclude` only govern `cargo
package`/`publish`; `.gitattributes export-ignore` only affects `git
archive`, not `git clone`/checkout). Decided: `cargo install --git`
keeps including `ai-work/` and that's fine - not worth fighting. The
actual ask is a separate, real downloadable source package (since
`cargo install` doesn't accept a tarball URL, this is download-then-
`cargo install --path`, not literally `cargo install <url>`) that
*does* exclude `ai-work/`.

## Design

1. **`.gitattributes`** (new, repo root): `ai-work/** export-ignore`.
   This is the actual mechanism - `git archive` (which is what both
   GitHub's automatic per-tag "Source code (zip/tar.gz)" links *and*
   a manual `git archive` invocation use) honors `export-ignore` and
   drops matching paths from the produced archive. Verifiable locally
   right now with a plain `git archive HEAD | tar -t`, no tag/CI/GitHub
   round-trip needed - part of the steps below.

2. **`.github/workflows/release.yml`**: after a real (non-dry-run)
   `cargo release` execution, additionally create a GitHub Release for
   the new tag via `gh release create v<version> --generate-notes`
   (`gh` CLI is preinstalled on GitHub-hosted runners; needs
   `GH_TOKEN: ${{ github.token }}` on that step - `contents: write`,
   already granted, covers this).
   - Only runs when `inputs.execute == 'true'` - a dry run shouldn't
     create a Release for a tag that was never pushed.
   - The new version number isn't otherwise captured from
     `cargo release`'s own output, so read it back from `Cargo.toml`
     (`cargo release` already bumped and committed it by this point in
     the job) via a small `grep`/`sed` step, feeding `v$version` to
     `gh release create`.
   - `--generate-notes` uses GitHub's own commit-range changelog
     (commits since the previous tag) - no separate changelog
     mechanism needed for now.
   - A GitHub Release page (vs. just a bare tag) is what makes GitHub
     auto-attach the "Source code (zip)"/"Source code (tar.gz)" links
     prominently and discoverably for users, rather than requiring
     them to know the `/archive/refs/tags/vX.Y.Z.tar.gz` URL by hand.

3. **README**: short "Installing" section covering both real paths -
   `cargo install --git <url> --tag vX.Y.Z` (includes `ai-work/`,
   simplest, needs `git`+`cargo` only) and "download the source
   archive from the Releases page, extract, `cargo install --path
   <dir>`" (excludes `ai-work/`, needs a manual download step first).

## Steps

1. Add `.gitattributes`; verify locally with `git archive HEAD |
   tar -tf - | grep ai-work` (expect no output) vs. the same without
   the attribute (expect it listed) - a real before/after check, not
   just trusting the mechanism.
2. Extend `release.yml` with the post-release `gh release create`
   step (guarded on `inputs.execute`).
3. Add the README "Installing" section.
4. `cargo fmt`/`clippy`/`test` (no Rust changes expected here, but
   run anyway per the standing rule) + re-verify the `.gitattributes`
   check from step 1 against the final tree.
5. Commit on `claude-release-workflow`.

Not doing (out of scope unless asked): crates.io publishing, a
separate changelog file/tool, binary release artifacts (still
source-only per earlier decision).
