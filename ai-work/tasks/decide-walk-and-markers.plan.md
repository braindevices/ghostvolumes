# `decide`: reuse convert's walk, add orphaned-marker resolution

Revises Phase 4 (`convert-project-model.plan.md`), per user feedback:
the shipped `decide` only wrote `--add`/`--deny` verbatim — it didn't
walk the filesystem the way `convert` does, so it couldn't find
undecided watched-name matches on its own, and it had no way to
resolve a `?` pending marker whose candidate doesn't currently exist on
disk (the walk can never discover something that isn't there).

## Design

**Reuse, don't reinvent, the walk+resolve engine.** `find_nested_candidates`
already just returns candidates — it never calls `materialize` itself.
`decision::resolve`/`ask_remember`/`confirm_override`/`record_decision`
don't know about materializing either. The only place `convert` and
`decide` actually diverge is the *terminal* action once a candidate's
decision is reached — so add one small enum, `Action` (`DecideAndConvert`
| `DecideOnly`), threaded through `resolve_candidate`/`ask_and_maybe_convert`,
rather than a bigger `walk_with_action(..., action_type)` abstraction —
there's no third mode in active use yet, and this keeps every existing
call site (candidate discovery, decision resolution, prompting) exactly
as-is.

- `Some(true)` (existing `+`): `DecideAndConvert` materializes (today's
  `convert` behavior); `DecideOnly` is a no-op — the decision already
  exists, nothing to change.
- Undecided, answered "yes"/matched: `DecideAndConvert` materializes
  *and* records; `DecideOnly` only records.
- `ask_remember`'s prompt wording becomes action-aware ("Convert" vs.
  "Decide") — asking "Convert ...?" when `decide` will never actually
  convert, even on "yes", would be misleading.

**Orphaned `?` markers** (a pending marker whose candidate doesn't
exist on disk right now — so the filesystem walk can never reach it):
resolved by reading the *boundary's own top-level* decision file text
directly (not walking every nested decision file in the tree — machine-
written markers always land there via `append_pending_marker`; a human
hand-typing a `?` line into some deeper nested file is an out-of-scope
edge case for now), extracting each `? <pattern>` line's pattern
(new `parse_pending_patterns` in `shim/decision_core.rs`), and
reconstructing the implied candidate path (`boundary.join(pattern
without its leading '/')` — correct for the anchored patterns this
tool itself always writes; a bare unanchored pattern degrades to being
treated as directly under the boundary, an acceptable approximation for
the rare hand-authored case). That reconstructed path needs no special
handling at all — `Path` operations don't require the path to exist, so
it's just handed to the *same* `resolve_candidate` used for real,
walk-discovered candidates (with `action = DecideOnly`), reusing 100%
of the existing per-candidate logic.

Order matters: write `--add`/`--deny` verbatim first (`record_decision`
using the human's own pattern as both the search key and the content —
an exact-string coincidence, not a re-derivation, that still toggles a
matching pending marker in place), *then* walk, *then* scan for
remaining markers. Anything the walk resolves already updates the
decision file, so the marker scan naturally sees fewer stragglers and
never double-asks about the same candidate.

`decide` gains `--max-depth` (mirrors `convert`, since a full-depth
walk is the norm) but not `--create` — `--create` means "materialize
this", which conflicts with `decide`'s "never touches the filesystem"
contract. `decide` also needs `config_dir` now (for ignore-pattern
resolution during its walk, previously unnecessary when it only wrote
patterns verbatim).

## Steps

1. `shim/decision_core.rs`: `parse_pending_patterns(text) -> Vec<String>`
   (`#[allow(dead_code)]`, shim-dead/CLI-alive, same convention as
   `parse_ignore_patterns`).
2. `src/convert.rs`: `Action` enum. `ask_remember`/`ask_and_maybe_convert`/
   `resolve_candidate` gain an `action` parameter; `convert_with_io`'s
   call site passes `Action::DecideAndConvert` (no behavior change for
   `convert`). Rewrite `decide`/`decide_with_io`: write `--add`/`--deny`
   verbatim, walk via `find_nested_candidates` (now needing
   `config_dir`/`max_depth`), resolve each candidate via
   `resolve_candidate(..., Action::DecideOnly, ...)`, then resolve
   remaining `? ` markers the same way.
3. `src/main.rs`: `Command::Decide` gains `#[arg(long)] max_depth:
   Option<u32>`; dispatch passes `config_dir`/`max_depth`.
4. Tests: rewrite `decide`'s test suite (walk discovers an undecided
   watched-name match and asks/records-only, never materializes; an
   existing `+` is a no-op, not a re-materialize; an orphaned `?`
   marker with no corresponding directory still resolves; `--add`/
   `--deny` still work verbatim and still toggle an exact-string-
   matching pending marker in place; `convert`'s own existing test
   suite stays green with `Action::DecideAndConvert` wired through
   unchanged).
5. `README.md`: update `decide`'s description for the new walk +
   marker-resolution behavior and `--max-depth`.
6. `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + full
   `cargo test` clean.
7. Commit on `claude-convert-project-model` (already the active
   branch).
