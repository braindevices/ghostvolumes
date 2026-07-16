# Progress: Fold watched.d into roots.d

## Steps 1-6 — config.rs / merge.rs / cache.rs / scan.rs / init.rs / filenames.rs
**Status**: done
**Date**: 2026-07-16
### What was done
- `config.rs`: `RootsFile { default_watches: Option<Vec<String>>, roots:
  BTreeMap<String, RawRootEntry> }` via `#[serde(flatten)]`;
  `RawRootEntry { enabled: Option<bool>, watches: Option<Vec<String>> }`
  with `#[serde(deny_unknown_fields)]` so a typo'd per-root key (e.g.
  `enable`) is a parse error, not silently ignored. `WatchedFile`/
  `parse_watched` removed.
- `merge.rs`: `MergedConfig` now holds `Vec<ResolvedRoot>` (`{path,
  watches}`, already enabled-filtered and default-vs-override-resolved).
  `load_roots_dir` layers every `roots.d/*.toml` file in sorted order,
  last-file-wins per field for both `default_watches` and each root's
  `enabled`/`watches`. Added `MergedConfig::all_watched_names()` (union +
  dedupe across every resolved root) for `discover`'s non-root-scoped
  walk.
- `cache.rs`: `compile()` iterates `config.roots` directly (each root's
  `watches` already resolved) instead of cross-producting two flat
  lists — `compiled.tsv`'s own `(prefix, name)` row format is unchanged.
- `scan.rs`: `save_roots()` writes bare root-path entries (no
  `enabled`/`watches`) into `roots.d/00-auto.toml`; still never touches
  any other file.
- `init.rs`: writes `roots.d/00-defaults.toml` (`default-watches =
  [...]`) instead of `watched.d/00-defaults.toml` (`names = [...]`).
- `filenames.rs`: removed `WATCHED_D_DIR`/`DEFAULT_WATCHED_FILE_NAME`;
  renamed to `DEFAULT_WATCHES_FILE_NAME` (still `"00-defaults.toml"`,
  now under `ROOTS_D_DIR`).
- Also fixed two other consumers the plan didn't call out individually:
  `reload.rs` (root validation loop reads `root.path` now) and
  `main.rs`'s `Discover` command (`merged.all_watched_names()` in place
  of the removed `global_defaults` field).
### Deviations from plan
Batched into one progress entry/commit rather than six — the six files
are tightly coupled (one schema change threading through all of them)
and none compiles standalone until all six land together, so
intermediate per-file commits would each be red.
### Issues found / fixed
`tests/reload_cli.rs` and `src/reload.rs`'s own tests hand-wrote raw
`roots = [...]`/`names = [...]` TOML directly (not through the Rust
structs) — missed by the initial build, caught by `cargo test`; updated
to the new bare-table/`default-watches` shape.

## Steps 7-8 — README.md rewrite + repo-wide stale-reference grep
**Status**: done
**Date**: 2026-07-16
### What was done
- `README.md`'s Configuration section: single `roots.d/` directory,
  three example files, the new `default-watches`/per-root-table TOML
  shape, the last-file-wins-per-field merge rule, and the no-cascade
  disable note. `reload` command's one-line description updated too.
- `design.md`: one stale `roots.d`/`watched.d` mention (§ "project-roots.list
  is persistent user data") updated to `roots.d` alone.
- Grepped the whole repo for `watched.d`/`WATCHED_D`/`global_defaults`/
  `WatchedFile`/`DEFAULT_WATCHED`: remaining hits are our own new doc
  comments (expected) and historical `ai-work/tasks/*.progress.md`/
  `main-plan.md` records, left untouched per this project's convention
  of not rewriting completed-step history (`decision-model.plan.md`
  itself took the same posture: a new plan's header notes what it
  supersedes rather than editing the old doc in place).
### Deviations from plan
Batched into one entry, same reasoning as steps 1-6.
### Issues found / fixed
None beyond the one design.md line.

## Step 9 — fmt + clippy + full test pass
**Status**: done
**Date**: 2026-07-16
### What was done
`rustfmt` + `cargo clippy --all-targets` clean, full `cargo test` green
(175 lib tests + all integration suites). Manual smoke test via a
scratch `HOME`: `init` writes the new `roots.d/00-defaults.toml`
shape; hand-added `10-local.toml`/`00-auto.toml` entries (a per-root
`watches` override and a disabled root) parse and merge correctly,
confirmed by `reload` reaching the real BTRFS-validation step (which
then fails only because the sandbox has no BTRFS — expected).
### Deviations from plan
No CHANGELOG entry or `Cargo.toml` version bump on this branch — both
are superseded by the auto-version-derivation work done earlier this
session (`build.rs` now computes `--version` from the latest git tag +
branch, not from `Cargo.toml`). `Cargo.toml`'s version only needs
bumping at actual release time on `main`, when tagging; a CHANGELOG
entry only makes sense once a real version number exists to attach it
to. Both happen at merge/release time, not here.
### Issues found / fixed
None.
