# Fix project-boundary resolution: same-volume coverage, no nesting

Follow-on fix to `convert-project-model.plan.md` Phase 1, found during
design review (not yet released beyond this branch). `walkup_boundary`
and `ensure_project_registered` don't correctly handle multiple
registered projects in the same filesystem lineage, and conflate path
containment with volume (BTRFS root) containment. Decided after
extensive back-and-forth to **not support nested projects at all** —
decision/ignore files already self-distribute (closest-file-wins
walk-up), so a hierarchy of registered projects was never buying
anything; the only thing project registration needs to provide is a
single, correct stopping boundary per filesystem lineage.

## The bugs

1. **Multiple same-volume registered projects in one lineage**: the old
   `longest_matching_prefix` over `rows ∪ project_roots` picks the
   *deepest* registered project as the boundary — but `resolve()` stops
   *at* the boundary, so a shallower parent project's decisions are
   never even looked at. Given projects `/a/b`, `/a/b/c`,
   `/a/b/c/d/e`, `/a/b/c/d/e/f/g` all registered (no rows), a candidate
   under the deepest one should still merge decisions all the way up to
   `/a/b` — the shallowest.
2. **Path containment ≠ volume containment**: given rows `/`,
   `/a/b1/c1`, `/a/b3/c1` and project `/a`, converting
   `/a/b3/c1/d1` finds `/a` as a path-ancestor and (wrongly) treats it
   as covering — but `/a` is on volume `/` while `/a/b3/c1/d1` is on
   volume `/a/b3/c1`, a different BTRFS root entirely. They must be
   treated as unrelated. Symmetric bug the other direction: project
   `/a/b1/c1/d1` (volume `/a/b1/c1`) is not a real "nested conflict"
   under registering `/a/b1` (volume `/`) even though the paths nest.
3. **`ensure_project_registered`'s coverage check is exact-match, not
   ancestor-or-self** — registering a project nested under an
   already-covering one goes unnoticed, permanently narrowing decision
   resolution for everything under it (bug #1's practical trigger).
4. No detection of an orphaned decision file above an about-to-be-
   registered project (a forgotten parent registration) — silently
   proceeds rather than warning.

## Design

**`same_volume(rows, a, b) -> bool`**: `longest_matching_prefix(rows, a)
== longest_matching_prefix(rows, b)` (`None == None` counts as same).

**No nested projects, enforced at registration time** (not at query
time — query time can then assume the invariant holds):
`ensure_project_registered(path, ...)` now has four branches, in order:
1. `path` already covered (ancestor-or-self *and* same-volume) by an
   existing registered project → no-op, use it.
2. Not covered, and registering `path` would create a same-volume
   nesting conflict (an existing registered project is a *descendant*
   of `path`, same volume) → warn, list the conflicting project(s),
   ask "unregister them and register `path` as the new parent instead?"
   (default **no**) — confirmed unregisters each (reusing
   `projects::unregister`) then registers `path`; declined aborts.
3. Not covered, no nesting conflict, but a decision file exists at some
   ancestor of `path` (scanned up to `path`'s own volume boundary, or
   to `/` if `path` has no configured volume) with nothing registered
   covering it → warn (a parent may have been forgotten), ask "continue
   and register `path` as its own project anyway?" (default **no**);
   declined aborts with guidance to register the ancestor first.
4. Otherwise → today's plain "Register `path` as a project? [Y/n]"
   (default yes).
No TTY aborts in every branch that would otherwise ask, with a message
naming whichever conflict (if any) is relevant.

**`decision_boundary(rows, project_roots, project_path) -> PathBuf`**
replaces `walkup_boundary`. No longer takes `candidate` — computed
*once* per `convert`/`decide` invocation, not per-candidate (nesting
being disallowed means there's at most one covering project, a fixed
value for the whole run). Finds the covering project (ancestor-or-self
+ same-volume), else falls back to `project_path` itself — never to
`rows`/volume directly, since that would let decision resolution wander
into broad, incidental root territory `project_roots` exists to avoid.

**Consequence**: `register_project_root`'s call at the end of
`ask_and_maybe_convert` becomes dead — `ensure_project_registered`
already guarantees the resolved boundary is registered before any
candidate is processed. Remove it, along with the now-unused
`project_roots`/`project_roots_path` parameters threaded through
`resolve_candidate`/`ask_and_maybe_convert`.

**`is_ignored`'s project-root tier** currently reads `project_path`'s
own `.ghostvolumes-ignore` directly. Since `project_path` might now
resolve to a shallower covering project (branch 1 above), this tier
should read the resolved `decision_boundary`'s ignore file instead —
same reasoning as decisions, for consistency ("the ignore file has the
same logic").

## Steps

1. `src/convert.rs`: add `same_volume`; replace `walkup_boundary` with
   `decision_boundary` (no `candidate` param); rewrite
   `ensure_project_registered`'s four branches (needs `rows` threaded
   in, plus a new `nearest_ancestor_decision_file(path, limit)` helper
   bounded by volume); thread the precomputed `boundary` into
   `find_nested_candidates`/`_inner` (renaming their `project_path`
   param to `boundary` for the ignore lookup, keeping the walk's own
   recursion cursor separate) and into `resolve_candidate` (which drops
   `project_roots`/`project_roots_path`/`project_path` entirely);
   `ask_and_maybe_convert` drops the trailing `register_project_root`
   call and its now-unused params.
2. Tests: unit tests for `same_volume`/`decision_boundary` (including
   the exact nested-chain and cross-volume scenarios from the design
   discussion); rewrite/replace the old `walkup_boundary_*` tests;
   `ensure_project_registered` tests for all four branches (nesting
   warn/confirm/decline, orphan warn/confirm/decline, ancestor coverage
   no-op, plain ask unchanged); end-to-end `convert()` tests confirming
   decisions merge from a shallower same-volume parent project and that
   a cross-volume path-ancestor project is correctly ignored.
3. `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + full
   `cargo test` clean.
4. `README.md`: no user-facing command/flag changes, but the "How it
   works"/decision-file section should note projects can't nest and
   what happens when `convert`/`decide` finds an ambiguous ancestor.
5. Commit on `claude-convert-project-model` (already the active
   branch).

## Explicitly out of scope

- `discover` — unaffected, has no project-registration concept.
- Splitting `project-roots.list` into per-volume files — considered and
  rejected; the list is small, human-curated, linear-scanned once per
  invocation now (not per-candidate), no performance case for it.
