# Decision Model — Progress

Tracks implementation of `ai-work/tasks/decision-model.plan.md`. Each
step: implement → test → fix → commit → update this file → commit this
file, one unit at a time, never proceeding while red.

Branch: `claude-decision-model-redesign`

---

## Step 1 — `shim/decision_core.rs`: pattern parsing + matching
**Status**: done
**Date**: 2026-07-11
### What was done
Implemented `parse_lines` (splits a decision file's text into `+`/`-`
lines, ignoring comments/blanks), `pattern_matches` (the three pattern
forms: `/name` anchored, `name` unanchored-any-depth, `/a/b/**/name`
anchored-prefix-then-arbitrary-depth), `resolve_in_file` (last-matching-
line-wins), and `resolve` (walk-up from a candidate's parent to a given
boundary, stopping at the closest file with any matching line — a file
existing with no matching line does not count, the walk keeps going).
11 unit tests, all passing. `src/decision.rs` (thin `include!` wrapper,
same pattern as `git.rs`) added and wired into `main.rs`'s module list
so `cargo test` exercises it. `#[allow(dead_code)]` on the two public
functions since nothing calls them yet (wired into `decide()` later).
### Deviations from plan
Folded Step 3 (decision-file walk-up + resolution) into this same
step/file — `resolve()`'s walk-up and `resolve_in_file()`'s per-file
matching are tightly coupled and were natural to write and test
together rather than as two separate passes. Step 3 marked done here
too; no separate commit for it.
### Issues found / fixed
One clippy `collapsible_if` in `resolve()`'s walk-up loop — merged into
a single `if let ... && let ...` conditional.

## Step 2 — `cache_core.rs`: `longest_matching_prefix` helper
**Status**: done
**Date**: 2026-07-11
### What was done
Added `longest_matching_prefix(rows, path)`, a sibling to `names_for`/
`proactive_project_for` over the same `(prefix, name, is_proactive)`
rows: the longest ancestor-or-self prefix, `None` if nothing matches.
4 unit tests (narrower-wins, no-match, exact-prefix-itself,
component-boundary respect — mirroring `names_for`'s own test style).
`#[allow(dead_code)]` since it's not called yet.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 3 — Decision-file walk-up + resolution
**Status**: done (folded into Step 1 — see above)
**Date**: 2026-07-11
### What was done
### Deviations from plan
### Issues found / fixed

## Step 4 — Project-roots file (plain-text, live-read)
**Status**: done
**Date**: 2026-07-11
### What was done
New shared module (`shim/project_roots_core.rs` + `src/project_roots.rs`,
same pattern as the others): `parse()`, `needs_append()` (pure, callers
do the actual single-line `O_APPEND` write), `path_in()` (resolves the
file under the XDG data dir). 7 unit tests.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 5 — `ghostvolumes register <path>` subcommand
**Status**: done
**Date**: 2026-07-11
### What was done
New `src/register.rs`: `register(list_path, path)` reads the existing
file, skips the write if `project_roots::needs_append` says the path is
already present, else creates any missing parent directories and does
a single `O_APPEND` `writeln!`. Wired into `main.rs`: `Command::Register
{ path }` arm resolves `project_roots::path_in(&xdg::data_dir()?)` and
calls it. 4 unit tests (new path, idempotent re-register, appends
alongside existing entries, creates missing parent dirs).
### Deviations from plan
None.
### Issues found / fixed
None — `cargo fmt` reformatted the `OpenOptions` builder call onto
multiple lines, no functional change.

## Step 6 — Wire walk-up into `decide()`; remove `git_core.rs`/`git.rs`; drop proactive marker
**Status**: done
**Date**: 2026-07-11
### What was done
`decide()` (`shim/preload.rs`) now resolves the candidate via
`decision_core::resolve()`, bounded by a new `walkup_boundary()`
helper (longest matching prefix across `CACHE_ROWS` and the
registered project-roots list, reusing `cache_core::longest_matching_prefix`
over a combined row set rather than duplicating its logic). New
`Decision` variants `Denied`/`Undecided` replace `GitTracked`;
`Accept` now only fires on a recorded `+` (previously the default
fallthrough). `Undecided` is logged via `log_important` (always-on,
not debug-gated) per plan §4, in addition to the existing debug line.
`shim/preload.rs`'s `mod git_core;` removed (the shim itself no
longer needs it). `tests/shim_ld_preload.rs` rewritten: added a
`write_decision()` helper; most "matching name" tests now write an
explicit `+` decision file since undecided is skip-by-default now;
`git_tracked_path_is_never_converted` replaced by
`denied_decision_is_never_converted` and
`undecided_candidate_stays_plain_and_logs_an_always_on_notice`.
Picked `.ghostvolumes-decisions` as the decision file's final name
(one of the plan's open bikeshed items).
### Deviations from plan
Did not delete `shim/git_core.rs`/`src/git.rs` yet, and did not drop
`compiled.tsv`'s proactive marker column / `cache_core::proactive_project_for`
yet, even though the plan's Step 6 wording says to do both here.
`convert.rs`, `discover.rs`, and `main.rs` still call
`git::is_git_tracked` (their own replacement logic is explicitly
Step 8's and Step 10's job), and `ensure.rs` still calls
`cache::proactive_project_for` (dead only once `ensure` itself is
removed, Step 10). Deleting either now would force a premature,
throwaway rewrite of those call sites ahead of the steps that already
own that work. Both will be deleted once each loses its last
consumer, in Steps 8/10.
### Issues found / fixed
`shim/decision_core.rs`'s `resolve()` used a Rust 2024 let-chain
(`if let ... && let ...`), which compiles fine under `cargo test`
(the crate is edition 2024) but failed under the shim's standalone
`rustc --edition 2021` build — never caught until this step actually
wired `decision_core` into `shim/preload.rs`'s `mod` list and compiled
it standalone for the first time. Fixed by rewriting as
`.and_then(...)` followed by a single `if let`, avoiding the let-chain
entirely (and sidestepping the `collapsible_if` clippy lint the
original nested-if form would've re-triggered).

## Step 7 — Pending-comment appending on undecided
**Status**: done
**Date**: 2026-07-11
### What was done
`decision_core.rs` gains three small, independently unit-tested
functions: `anchored_pattern()` (candidate's path relative to the
walk-up boundary), `pending_comment_line()` (the `# <pattern>` text),
`needs_pending_comment()` (dedup check against a decision file's
current text). `Decision::Undecided` now carries the resolved
boundary `PathBuf` (computed once in `decide()`, reused rather than
recomputed) so `handle_intercept`'s new `append_pending_comment()` can
read the project's top-level decision file, skip the write if that
exact comment is already present, and otherwise append `# <pattern>\n`
with a single `write_all` (`O_APPEND`, same concurrency-safe
discipline as the log file). Two new integration tests: a fresh
undecided candidate gets the comment appended; a second run against
the same candidate doesn't duplicate it.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 8 — Extend `convert`: recursive walk, remember prompt, empty-create, side-effect registration
**Status**: done
**Date**: 2026-07-11
### What was done
`convert.rs` rewritten per plan §6. `<path>` is a starting point:
`find_nested_candidates()` reuses `discover`'s tree-walking
conventions (skip `.git`, optional `--max-depth`, never descend into
a match) to find every candidate under it matching a watched name
under a configured root (`cache::names_for`, root-scoped). `<path>`
itself is always a candidate regardless of match status, and
`create_empty()` makes it a fresh subvolume directly if it doesn't
exist yet (replacing cd-hook's old proactive pre-creation). All
candidates are sorted by path-depth (shallowest first) before
resolving each one: already-subvolume → skip; `+` → convert directly;
`-` → skip unless it's the literal `<path>` argument (then
`confirm_override()`); undecided → `materialize()` then
`ask_remember()`'s 3-state prompt (no / just this path / every match
of this name), TTY-gated. Recording a decision also idempotently
registers the resolved project root (`walkup_boundary()`) into the
project-roots list, in-memory and on disk (`register::register`).
`ask_remember`/`confirm_override` take injectable `is_tty`/`read_line`
for testability without a real terminal. Promoted `DECISION_FILE_NAME`
(now shared in `decision_core.rs`) and `resolve`/`anchored_pattern`/
`resolve_in_file` out of `#[allow(dead_code)]` now that `convert.rs`
is a genuine caller. `main.rs`'s `Convert` command gained
`--max-depth` and now resolves/passes `cache_path`/`project_roots_path`.
### Deviations from plan
None beyond the bikeshed/ordering deviations already recorded under
Step 6.
### Issues found / fixed
`walkup_boundary()`'s first draft fell back to `top_level_path` in
every no-data case, including when the candidate *is*
`top_level_path` itself — but a path can't be its own decision-file
walk-up boundary (`decision::resolve` requires the boundary be at or
above the candidate's *parent*). Caught by a failing test
(`a_minus_decision_on_the_literal_argument_is_not_overridden_without_a_tty`):
an existing `-` decision on the literal argument, with an empty cache
and no registered project roots, incorrectly converted anyway instead
of prompting for override. Fixed by falling back one level further
(the candidate's own parent) specifically in that case; added a
dedicated `walkup_boundary` unit test for it too.

## Step 9 — `ghostvolumes intercept -- <cmd>` subcommand
**Status**: done
**Date**: 2026-07-11
### What was done
New `src/intercept.rs`. `intercept()` sets `LD_PRELOAD` on the child's
environment only, inherits stdio, waits, then diffs each possible
decision-file boundary's full text before/after
(`candidate_boundaries()`: union of `compiled.tsv` row prefixes and
the registered project-roots list) and prints one notice per changed
root naming `ghostvolumes convert <project-root>`. Full-text
comparison, not mtime (coarse resolution risk for sub-second runs).
`intercept_with_notifier()` takes an injectable `notify` callback so
the notice logic is unit-tested directly (no stderr capturing).
`main.rs` gained `Intercept { cmd: Vec<String> }` with
`trailing_var_arg`/`allow_hyphen_values`, propagating the child's exit
code via `std::process::exit`.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 10 — Remove `ensure`/cd-hook entirely; update `discover`'s suggested output
**Status**: done
**Date**: 2026-07-11
### What was done
Deleted `src/ensure.rs`, `src/registration.rs`, `src/pathmatch.rs`,
`src/git.rs`, `shim/git_core.rs` — the last two `git.rs` call sites
(`discover.rs`, `main.rs`'s `discover` command) removed here, after
`convert.rs`/`preload.rs` lost theirs in Steps 6/8. `config.rs` and
`merge.rs` lost `ProjectsFile`/`ProjectEntry`/`RepoLocalFile`/
`load_projects_dir`/`MergedConfig::projects` entirely — only
`roots.d`/`watched.d` remain. `compiled.tsv` reverts to plain
`(prefix, name)` rows: dropped the "proactive" column and
`cache_core::proactive_project_for`, rippling the row-type change
through `shim/cache_core.rs`, `shim/preload.rs`, `src/cache.rs`'s
`compile()`, `src/convert.rs`, `src/intercept.rs`. `discover.rs`
dropped the git-tracked gate entirely (`group_and_gate` →
`group_by_parent`, `format_toml` → `format_decisions`, rendering
`+ name` decision-file lines instead of `projects.d` TOML blocks);
`--save` now appends idempotently to each suggested project's own
decision file instead of one central `projects.d/local.toml` + reload.
`shellinit.rs`'s snippets now emit only the `LD_PRELOAD` export, no
`cd`/`chpwd` hook. `main.rs` lost `mod ensure/registration/pathmatch/git`
and `Command::Ensure`. `init.rs`/`reload.rs`'s test helper stopped
creating an empty `projects.d` dir. `build.rs`'s rerun-if-changed list
updated to match the actual current shared-file set.
### Deviations from plan
None beyond what Step 6 already flagged as deferred to this step.
### Issues found / fixed
`tests/cli_scaffold.rs`'s help-text assertion still expected `ensure`
in `--help` output — updated to drop it and cover `register`/
`intercept` instead.

## Step 11 — `GHOSTVOLUMES_AUTO_YES` env var support
**Status**: done
**Date**: 2026-07-11
### What was done
`decide()` checks the new `auto_yes_enabled()` (same non-empty/not-"0"
parsing convention as `GHOSTVOLUMES_DEBUG`) right after the
`AlreadySubvolume` check — set → bypass the decision-file lookup
entirely, always `Accept`, nothing recorded. Read live per call, no
`OnceLock` caching. Two new integration tests: `=1` bypasses an
otherwise-undecided candidate (and creates no decision file); `=0`
does not bypass.
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 12 — Update `design.md` and `main-plan.md`
**Status**: done
**Date**: 2026-07-11
### What was done
`design.md` rewritten in place (it's the kept-current summary, not a
historical record): "What it does" now describes `intercept` +
`convert` instead of LD_PRELOAD + cd-hook; new "Key decisions" entry
on the git-tracked gate's replacement by decision files,
`GHOSTVOLUMES_AUTO_YES`, and the shim's never-prompts invariant;
shared-file list updated; known compromises/gotchas updated (the
fresh-project zero-benefit case replaces the cd-hook non-interactive-
shell gap; the LD_PRELOAD-stripping gotcha now notes the shim spawns
no subprocess at all).

`main-plan.md` kept as the historical record of the original design,
unedited except for pointer notes added at every section the redesign
superseded (§1, §2, §4, §5, §6, §7, §8.0, §8.1, §8.2, §9) — each names
what changed and points at this doc, rather than deleting the
historical reasoning trail.
### Deviations from plan
Interpreted "update main-plan.md" as annotate-with-pointers rather
than rewrite-in-place, given decision-model.plan.md's own header
already establishes that convention ("Supersedes plan §4's...") and
main-plan.md's role (per design.md's own front matter) is the
historical original-design record, not a living doc — rewriting it in
place would lose the "why" reasoning trail design.md itself calls out
as worth preserving.
### Issues found / fixed
None.
