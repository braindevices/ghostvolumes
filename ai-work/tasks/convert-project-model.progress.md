# Progress: convert project model, ignore patterns, dry-run, decide subcommand

## Phase 1 — separate "the project" from "things to convert" + debug tracing
**Status**: done
**Date**: 2026-07-16
### What was done
- `main.rs`: `Command::Convert` gains `#[arg(long = "create")] create:
  Vec<String>`; dispatch joins each against the absolutized project
  path before calling `convert::convert`.
- `convert.rs`: `path` is never added to `candidates` (was
  `vec![path.to_path_buf()]`, now `create.to_vec()`). New
  `ensure_project_registered` asks "Register `<path>` as a project?
  [Y/n]" upfront (default yes on empty interactive answer, unlike
  every other prompt in this file — registering is low-stakes and
  reversible; but a missing TTY still aborts rather than presuming
  yes) — declining or no TTY aborts the whole command.
  `resolve_candidate`'s old `candidate == top_level_path` checks
  (override-confirm, watched-name gate) now key off `create.contains(candidate)`
  instead. `walkup_boundary` simplified — `project_path` is always a
  valid ancestor-or-self fallback now, so the old "candidate is its
  own boundary" special case is gone.
- Removed `matches_a_watched_name` entirely: with `path` never a
  candidate, every candidate is either explicit (`--create`, no name
  check needed) or walk-discovered (already guaranteed to match by
  construction) — the gate had become permanently unreachable dead
  code.
- Added `debug_trace`/`debug_enabled` (`GHOSTVOLUMES_DEBUG`, same
  convention as the shim), called at each branch in `resolve_candidate`
  explaining which boundary/decision drove the outcome. Removed all
  the ad-hoc unconditional `println!`s added while debugging the
  original report.
- Rewrote nearly every `convert::` test for the new
  `(path, create, ...)` signature and upfront-registration
  requirement; added a `register_project` test helper (writes directly
  to the project-roots file, bypassing the interactive ask for tests
  not specifically testing that ask) plus dedicated tests for
  `ensure_project_registered` itself, an end-to-end abort-on-decline
  test, and a test confirming `--create` needs no cache row at all.
### Deviations from plan
None structurally. One simplification beyond the plan text: since
`path` is never a candidate, `matches_a_watched_name` turned out to be
fully dead code after the refactor (not just re-scoped), so it was
deleted rather than kept for `--create` bypass logic — there was no
longer any caller needing it.
### Issues found / fixed
Live end-to-end smoke test (`GHOSTVOLUMES_DEBUG=1 convert
<project-tracked> --create .cache`) confirmed: --create resolved with
no cache row, the walk found `build` independently, debug tracing
printed the boundary/reasoning for both, and `project-tracked` itself
was never converted (inode unchanged) — reproducing and confirming the
fix for the originally reported scenario.

## Phase 2 — configurable ignore patterns
**Status**: done
**Date**: 2026-07-16
### What was done
- `shim/decision_core.rs`: `parse_ignore_patterns` (bare patterns, one
  per line, `#`/blank skipped) and `ignore_matches` (wraps
  `pattern_matches` for a flat pattern list) — `#[allow(dead_code)]`,
  same convention as `cache_core.rs`'s shim-dead-but-CLI-alive
  functions, since the shim never walks a directory tree.
- `src/filenames.rs`: `IGNORE_FILE_NAME = ".ghostvolumes-ignore"`,
  CLI-only (not in `shim/filenames_core.rs`).
- `src/config.rs`/`merge.rs`: `RootsFile`/`MergedConfig` gain
  `default_ignore`/`ignore` — merged last-file-wins exactly like
  `default-watches`, but global-only (no per-root override; per-root/
  per-project ignores are `.ghostvolumes-ignore` files instead).
- `src/convert.rs`: new `is_ignored` helper unions all three tiers
  (global `default-ignore`, nearest volume root's own
  `.ghostvolumes-ignore` via `cache::longest_matching_prefix`,
  `project_path`'s own `.ghostvolumes-ignore` — no project-roots-list
  search needed since Phase 1 already guarantees `project_path` is
  registered). `find_nested_candidates`/`_inner` gain `global_ignore`
  and (since `start` was always identical to the project path already)
  are renamed to thread `project_path` through instead of a redundant
  second name for the same value; the hardcoded `.git` check is gone,
  replaced by `is_ignored`, checked before the watched-name match.
  `convert`/`convert_with_io` gain a `config_dir` parameter, loading
  `merge::load_all(config_dir)?.ignore` — a plain, separate load from
  `compiled.tsv`'s rows, since ignore patterns are CLI-only and the
  shim never needs them.
- `src/discover.rs`: `walk`/`walk_inner` gain `ignore_patterns`,
  checked the same way; global-tier only (see Deviations below) —
  hardcoded `.git` check removed here too.
- `src/init.rs`: `00-defaults.toml`'s skeleton (renamed
  `DEFAULT_WATCHES` → `DEFAULTS_TOML`, since it's no longer just
  watches) now ships `default-ignore = [".git", ".hg", ".svn",
  ".snapshots"]` alongside `default-watches`.
- Tests: `decision_core.rs` unit tests for both new functions;
  `config.rs`/`merge.rs`/`cache.rs` updated for the new field (every
  `MergedConfig { .. }` literal needed `ignore: Vec::new()`); `init.rs`
  test for the new default-ignore skeleton; `discover.rs` tests updated
  (`.git` no longer special without a matching pattern - added a test
  proving a `.git` directory *is* walked into without one) plus new
  custom-ignore-pattern and ignore-beats-watched-name tests;
  `convert.rs` gained a `config_path` test helper and three end-to-end
  tests, one per tier, each confirming the ignored directory stays
  plain even though it also matches a watched name.
- `README.md`: documented `default-ignore` and a new "Ignoring
  directories entirely" section.
### Deviations from plan
`discover::walk` only honors the *global* `default-ignore` tier, not
volume-root/project-root `.ghostvolumes-ignore` files. `discover` walks
an arbitrary starting path (`~` by default) with no project-boundary
concept at all — unlike `convert`, it has no natural "the project" or
even "the one volume root in play" (a single `discover` run can span
multiple configured roots). Wiring in the other two tiers would need
`discover` to gain the kind of root/project awareness it deliberately
doesn't have today (`merge.rs`'s own doc comment already calls out that
`all_watched_names()` exists specifically because "discover's
pre-adoption walk isn't root-scoped"). Scoped down to the tier that
actually fits its design rather than force-fitting the other two.
### Issues found / fixed
None beyond the expected mechanical fallout (every existing
`MergedConfig` literal and every `convert`/`convert_with_io` test call
site needed updating for the new field/parameter) — `cargo test` was
green throughout once each file's callers were updated in turn.

## Phase 3 — dry-run mode
**Status**: done
**Date**: 2026-07-16
### What was done
- `src/main.rs`: `Command::Convert` gains `#[arg(long)] dry_run: bool`.
- `src/convert.rs`: `convert`/`convert_with_io` gain `dry_run: bool`.
  `ensure_project_registered` short-circuits right after its coverage
  check when `dry_run` is set — prints `would register: <path> as a
  project (skipped — dry run)` and returns, never reaching the
  nesting-conflict/orphan-ancestor/plain-ask branches (so a dry run
  never prompts, even for a path that would otherwise trigger one of
  those). `resolve_candidate` short-circuits each of its three
  side-effecting branches: an existing `+` decision reports
  `would create`/`would convert` (via new `report_would_materialize`,
  mirroring `materialize`'s own create-vs-migrate split) instead of
  calling `materialize`; an explicit `--create` override of a `-`
  decision reports `would ask to override ...` instead of calling
  `confirm_override`; an undecided candidate reports `undecided: ...
  (skipped — dry run)` instead of calling `ask_and_maybe_convert`. All
  three dry-run messages are unconditional `println!`s (the primary
  output of the command), not gated by verbosity.
- Confirmed via `decision_boundary`'s own doc comment that no special
  "simulated boundary" logic was needed: since a confirmed registration
  in every one of `ensure_project_registered`'s branches ends with
  `path` itself as the sole covering project, `decision_boundary`'s
  existing fallback (`project_path` itself, when nothing covers it)
  already produces the exact right boundary for the rest of the dry-run
  preview with zero extra code.
### Deviations from plan
The plan (written before Phases 1-2/the nested-project-boundaries fix/
leveled-verbosity refactor) said dry-run would thread down to
`ask_and_maybe_convert` and short-circuit before
`register_project_root`; that call was already removed as dead code by
the nested-project-boundaries fix, and `ask_and_maybe_convert` itself
turned out not to need any `dry_run` awareness at all — every branch
that could reach it is short-circuited one level up in
`resolve_candidate` first, so it's simply never called when `dry_run`
is set. `dry_run` is threaded as a plain parameter (alongside `is_tty`)
rather than a global/`OnceLock` flag like the leveled-verbosity work —
unlike verbosity, it's genuinely scoped to one command's own call
graph, not a cross-cutting, environment-configured concern used
everywhere.
### Issues found / fixed
None — went green on the first full test run after the mechanical
signature-threading updates.

## Phase 4 — decide subcommand
**Status**: not started
**Date**:
### What was done
### Deviations from plan
### Issues found / fixed
