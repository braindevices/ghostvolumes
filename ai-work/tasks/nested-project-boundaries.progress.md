# Progress: fix project-boundary resolution (same-volume, no nesting)

## Step 1 — implementation
**Status**: done
**Date**: 2026-07-16
### What was done
- `src/convert.rs`: added `same_volume(rows, a, b)` (`longest_matching_prefix`
  equality, `None == None` counts as same). Replaced `walkup_boundary`
  with `decision_boundary(rows, project_roots, project_path)` — no
  `candidate` parameter anymore, computed once per `convert` run:
  finds the *shortest* (outermost) registered project that's both an
  ancestor-or-self of `project_path` and same-volume, falling back to
  `project_path` itself (never to a bare row/volume prefix).
- `ensure_project_registered` gained a `rows` parameter and a fourth
  branch, checked in order: (1) already covered by a same-volume
  ancestor project → no-op; (2) would nest over an already-registered
  same-volume descendant project → warn, list it, ask to unregister +
  register the new parent (default no); (3) an orphaned decision file
  exists at some ancestor up to the path's own volume boundary (new
  `nearest_ancestor_decision_file` helper) → warn, ask to register
  anyway (default no); (4) plain "Register as a project? [Y/n]"
  (default yes), unchanged from before.
- `register_project_root`'s call at the end of `ask_and_maybe_convert`
  removed as dead code — `ensure_project_registered` now guarantees the
  boundary is already registered before any candidate is touched.
  `resolve_candidate`/`ask_and_maybe_convert` dropped their now-unused
  `project_roots`/`project_roots_path`/`project_path` parameters as a
  result (down from ten arguments to six/four).
- `is_ignored`'s project-root ignore-file tier now reads the resolved
  `boundary`'s `.ghostvolumes-ignore`, not `project_path`'s own file
  directly — consistent with decisions, since `boundary` may be a
  shallower covering project.
### Deviations from plan
None.
### Issues found / fixed
First draft of `decision_boundary` used `.find()` instead of
`.min_by_key(|p| p.len())` — picked whatever project happened to be
first in the (unordered) `project_roots` list rather than the
shallowest of a nested chain, caught immediately by
`decision_boundary_merges_all_the_way_to_the_shallowest_of_a_nested_chain`.

## Step 2 — tests
**Status**: done
**Date**: 2026-07-16
### What was done
Added: `same_volume`/`decision_boundary` unit tests (including the
exact nested-chain and cross-volume scenarios from the design
discussion); `nearest_ancestor_decision_file` tests (finds closest,
none-when-absent, stops at the limit even if one exists further up);
`ensure_project_registered` tests for all four branches plus the
already-covered-by-ancestor and different-volume-descendant cases;
two end-to-end `convert()` tests — one confirming a decision recorded
at a shallower same-volume parent project governs a candidate found
while converting a deeper, also-registered child path, one confirming
a path-ancestor project on a *different* volume is correctly ignored
(the inner path gets its own separate registration and decision
instead of inheriting or being blocked by the outer one).
### Deviations from plan
None.
### Issues found / fixed
None beyond the `decision_boundary` bug caught in Step 1.

## Step 3 — fmt/clippy/test verification
**Status**: done
**Date**: 2026-07-16
### What was done
`cargo fmt`, `cargo clippy --all-targets --all-features -- -D
warnings` clean, full `cargo test` green (237 lib tests, up from 221
before this fix — 16 new tests).
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 4 — README update
**Status**: done
**Date**: 2026-07-16
### What was done
Added a "Projects can't nest" paragraph under "The project-roots list"
documenting the four-branch decision tree; updated the `convert` "How
it works" bullet to point at it instead of describing the old plain
Y/n-only ask.
### Deviations from plan
None.
### Issues found / fixed
None.
