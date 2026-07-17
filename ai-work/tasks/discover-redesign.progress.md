# Progress: redesign discover (respect decisions, three kinds, advisory-only)

## Step 1 — respect existing decisions, classify into three kinds
**Status**: done
**Date**: 2026-07-16
### What was done
`src/discover.rs`: `MatchKind` enum (`ApprovedCandidate` |
`UnwatchedSubvolume` | `NotYetConverted`); `DiscoveredMatch` gains a
`kind` field. `walk_inner` now checks `is_watched`/`is_subvolume`
independently (previously only watched-name-gated subvolume checking,
same gap `convert`/`decide`'s own walk already had fixed) and
classifies accordingly, filtered by a new `is_undecided` check —
`decision::resolve` against `start` (the discover argument) as the
walk-up boundary, the exact same logic `convert`/`decide` use against
their own registered boundary. Nothing already decided anywhere
between a match and `start` is reported at all anymore. Never descends
into any of the three kinds (matches the existing "never descend into
a match" convention).
### Deviations from plan
None.
### Issues found / fixed
None — reasoned through and verified via the full test rewrite in Step
2 rather than incrementally.

## Step 2 — advisory output, remove --save
**Status**: done
**Date**: 2026-07-16
### What was done
- `ProjectSuggestion` gains three name lists (one per kind) instead of
  one flat list; `group_by_parent` buckets accordingly, each list
  sorted/deduped independently.
- `format_decisions` → `format_report`: renders a `ghostvolumes decide
  <dir> --add <name>...` command per approved-candidate group; both
  `--add`/`--deny` options (not a single pick) for unwatched
  subvolumes, since discover can't ask interactively the way
  `convert`/`decide` can to default to yes on its own; a plain,
  command-free informational line for not-yet-converted watched names
  — deliberately never real decision-file syntax (`-` there would
  misrepresent "nobody decided" as "a human declined").
- `src/main.rs`: `Command::Discover` drops `--save` and the
  write-decision-file dispatch logic entirely — discover never touches
  a decision file or the project-roots list itself anymore; it only
  ever tells you what `decide` command to run.
### Deviations from plan
None.
### Issues found / fixed
None — full test rewrite (21 tests) passed on the first run.

## Step 3 — reasonable default --max-depth
**Status**: done
**Date**: 2026-07-16
### What was done
Per a follow-up request: `Command::Discover`'s `max_depth` changed
from `Option<u32>` (unlimited by default) to a plain `u32` defaulting
to `3` — discover walks an arbitrary, unregistered path ($HOME by
default), unlike `convert`/`decide` which operate on one already-scoped
registered project, so an unbounded-by-default walk there is a real
cost/risk `convert`/`decide` don't share. `convert`/`decide` keep their
existing unlimited default, unchanged.
### Deviations from plan
Not in the original plan — a follow-up request after the redesign
landed.
### Issues found / fixed
**Flagged to the user, not something I changed unilaterally**: the
depth-counting convention (`depth` = how many directory levels have
been recursed *into* to reach the current directory, checked before
reading its entries) means a 4-level-deep example
(`test/aa/pp-with-subvol/bb/ca/build`) needs `--max-depth 4` to be
found, not the new default of `3` — verified live: default `3` finds
nothing for that exact tree, `--max-depth 4` finds both `bb/ca` and
`bb/cc`. The default is exactly what was asked for ("for example 3");
this is just the concrete consequence of that specific number given
how depth is counted, surfaced so the number can be adjusted if it
doesn't match the intended scanning depth in practice.

## Step 4 — merge nested suggestions into their shallowest ancestor
**Status**: done
**Date**: 2026-07-16
### What was done
Follow-up from a live run against a real tree: discover suggested
`ghostvolumes decide` commands for both
`/root/test/aa/pp-with-subvol` and its own descendant
`/root/test/aa/pp-with-subvol/bb/cc`, side by side — flagged by the
user as a "critical problem" since registering both would violate the
no-nested-projects invariant. First pass (superseded, see below) only
added a `NOTE:` under the deeper suggestion; asked which severity was
actually wanted, the user asked for a real merge — one project
suggested per lineage, not two cross-referenced ones.
`src/discover.rs` gains `merge_nested_suggestions(Vec<ProjectSuggestion>)
-> Vec<ProjectSuggestion>`, called between `group_by_parent` and
`format_report` (`src/main.rs`'s discover dispatch). Reuses
`shallowest_ancestor_suggestion` (kept from the superseded NOTE-based
attempt) to find each nested suggestion's merge target, then folds its
three name lists into the ancestor's, re-expressing each name as
`decision::anchored_pattern(ancestor_path, child_path.join(name))`
(e.g. `/bb/cc/build`) so the resulting single `decide` command still
resolves the exact original location. A 3+-level chain folds correctly
in one pass with no ordering dependency, since
`shallowest_ancestor_suggestion` already finds the true shallowest
ancestor directly for every level, not just the immediate parent.
`format_report` no longer computes or prints anything nesting-related
itself — merging is expected to have already happened by the time it's
called.
### Deviations from plan
Not in the original plan — a follow-up request after a live run
surfaced the gap, then refined again after the first (NOTE-only) fix
didn't match the severity the user actually wanted.
### Issues found / fixed
None. The two NOTE-based tests were replaced with
`merge_nested_suggestions`-based equivalents (three-level chain folds
both descendants into the shallowest ancestor with correctly anchored
names; unrelated siblings are left as separate suggestions) plus a
`format_report` test confirming a folded-in anchored pattern renders
as an extra `--add` on the same command line. All passed on the first
run. Live smoke test against the user's own reported tree confirmed
the merged output: a single `ghostvolumes decide /root/test --add
/aa/pp-with-subvol/bb/cc/build --add /aa/pp-with-subvol/build` instead
of two separate, conflicting suggestions.

## Step 5 — flag drift between a recorded decision and the filesystem
**Status**: done
**Date**: 2026-07-16
### What was done
Follow-up feature request: discover only ever reported *undecided*
matches, so a decision that's actually on record but disagrees with
reality (a hand-made subvolume appearing after a `-` was recorded; a
`+` recorded but `convert` never actually run) was silently invisible.
Scoped to on-disk mismatches only, per explicit choice — decisions
recorded for paths that don't exist on disk at all are out of scope,
since the walk only ever visits directories that exist; that would
need a separate anchored-pattern scan like `convert`'s
`decision_file_anchored_candidates`, not implemented here.
`src/discover.rs`: two new `MatchKind` variants, `DeniedButExists`
(recorded `-`, but it's a real subvolume anyway) and
`ApprovedNotConverted` (recorded `+`, but still plain). A new
`classify(resolved, is_watched, is_subvolume) -> Option<MatchKind>`
function replaces the old `is_undecided`-gated branch in `walk_inner` —
it now looks at `decision::resolve`'s full three-way result (`None` /
`Some(true)` / `Some(false)`) crossed with actual subvolume-ness,
instead of only checking whether anything was decided at all; the two
consistent combinations (`+`-and-subvolume, `-`-and-plain) still
report nothing. `ProjectSuggestion` gains matching fields, folded by
`merge_nested_suggestions`/`fold_nested_child` (re-anchored) exactly
like the other three kinds. `format_report` prints `DRIFT: recorded as
denied ('-') but already a subvolume ...` with an override `--add`
command for the first kind, and an informational `approved ('+') but
not yet converted - run to materialize: ghostvolumes convert <path>`
for the second.
### Deviations from plan
Not in the original plan — a follow-up feature request. Scope
(on-disk-only, no off-disk anchored-pattern scan) was confirmed with
the user before implementing rather than assumed.
### Issues found / fixed
One existing test (`a_denied_watched_subvolume_is_also_not_reported`)
was asserting the *old*, now-wrong behavior — it recorded `-` against
an actual subvolume and expected silence; split into two tests: the
genuinely-consistent case (`-` and still plain — stays silent) and the
drift case (`-` and already a subvolume — now flagged). Everything
else passed on the first run, including two new live-BTRFS unit tests
and two new `format_report` tests.

## Verification
`cargo fmt` + `cargo clippy --all-targets --all-features -- -D
warnings` clean throughout. Full `cargo test`: 296 lib tests. Live
smoke tests: `ApprovedNotConverted` confirmed via the actual CLI binary
against a plain `build` directory with a `+ build` decision file
(prints the `ghostvolumes convert` suggestion); `DeniedButExists`
confirmed via the real-BTRFS unit test fixture (no `btrfs-progs` CLI
available in this environment to drive it through the binary
end-to-end, so the equivalent coverage comes from the unit test that
already exercises the identical `btrfs::create_subvolume` code path).
Earlier smoke test reproducing the user's own reported tree
(`project-tracked` fully decided, `.cache` undecided-and-watched-and-
plain, `aa/pp-with-subvol/bb/{ca,cc}/build` undecided-and-watched-and-
plain) confirmed: already-decided `.venv`/`build` produce zero output;
`.cache` and both `build`s correctly show as "not yet converted
(informational only)"; a real subvolume with an unwatched name (tested
separately, since faking one requires real BTRFS) correctly produces
the `--add`-or-`--deny` clarification form; the nested-suggestion merge
(Step 4) confirmed live against the same tree.

## Step 6 — don't let the start path absorb unrelated nested suggestions
**Status**: done
**Date**: 2026-07-16
### What was done
Live run against a real, much broader tree (`discover ~/ --max-depth
9`) surfaced a sharper version of the Step 4 problem: `/root/` itself
had its own unrelated finding (a leftover artifact directly inside
it), which — being the shallowest path in the whole report — became
the merge target for *everything* underneath it, including
`aa/pp-with-subvol` and its own nested `bb/cc`, several directory
levels away and with nothing to do with the top-level finding. The
underlying issue: `merge_nested_suggestions` treated "technically
nested" (true, since `/root/` is a filesystem ancestor of everything)
as equivalent to "should become one project" (false — the two
findings share nothing except being under the same broad, arbitrary
directory being surveyed).
Resolved by excluding the discover start path itself from the pool of
paths eligible to *absorb* another suggestion, by default.
`merge_nested_suggestions` gains two parameters, `start: &Path` and
`root_is_project: bool`; the candidate pool passed to
`shallowest_ancestor_suggestion` filters out `start` unless
`root_is_project` is set. `start`'s own findings are still reported as
their own group either way — this only controls whether something
else can fold into it. `src/main.rs`'s `Command::Discover` gains a
`--root-is-project` boolean flag (default off) threaded through.
Considered and rejected: a more general `--no-project <path>`
(repeatable) exclusion list for arbitrary intermediate containers,
not just `start` — the live case was specifically about the start
path itself, and `--root-is-project` covers it with less surface area;
can revisit if an intermediate (non-start) container turns out to
cause the same problem in practice.
### Deviations from plan
Not in the original plan — surfaced by a live run at a broader scope
than the tree Step 4 was fixed against. Design (exclude `start`
by default, opt back in via `--root-is-project`) was proposed by the
user directly and confirmed before implementing.
### Issues found / fixed
None. Two new tests (start path with its own finding doesn't absorb
an unrelated nested suggestion by default; `--root-is-project` restores
the old merge-into-start behavior) passed on the first run; the two
existing `merge_nested_suggestions` tests were updated to pass an
ancestor path that isn't itself one of the suggestions, preserving
their original intent.

## Verification
`cargo fmt` + `cargo clippy --all-targets --all-features -- -D
warnings` clean throughout. Full `cargo test`: 298 lib tests. Live
smoke test: `discover ~/ --max-depth 9` against the real home directory
(now containing `~/test/aa/pp-with-subvol`, `~/test/project-tracked`,
and a large amount of incidental `.cargo`/`.vscode-server`/`go/pkg/mod`
clutter) confirmed `/root/` now reports only its own direct finding
(`.cache`, informational) and no longer absorbs `aa/pp-with-subvol`
(which correctly stands on its own, with `bb/cc` still folded into it
per Step 4) or any of the unrelated cache-directory findings scattered
throughout the tree.

## Step 7 — `--no-project <path>` for known containers below `start`
**Status**: done
**Date**: 2026-07-16
### What was done
Follow-up: Step 6 only protects `start` itself from absorbing a nested
suggestion. A known container found *below* `start` (e.g. a workspace
folder holding many unrelated repos, several levels deep in a huge
`discover ~/` scan) could still end up as an accidental merge target
if it happens to have its own direct finding — `--root-is-project`
doesn't help there, since that only concerns `start`.
`merge_nested_suggestions` gains a fourth parameter, `no_project:
&[PathBuf]` — always excluded from the merge-candidate pool
regardless of `root_is_project`, exact-path match only (not prefix —
excluding a shallower path already keeps the walk from reaching deeper
by-name matches, since discover doesn't record standalone "path" nodes
without a finding). `src/main.rs`'s `Command::Discover` gains
`--no-project <path>` (repeatable, absolutized like `path`/`--create`
elsewhere).
### Deviations from plan
Not in the original plan — user follow-up request right after Step 6
landed.
### Issues found / fixed
None. One new test (a container below `start`, not `start` itself,
correctly stays un-merged when named via `--no-project`) plus the four
existing `merge_nested_suggestions` tests updated for the new
parameter, all passed on the first run.

## Verification
`cargo fmt` + `cargo clippy --all-targets --all-features -- -D
warnings` clean throughout. Full `cargo test`: 299 lib tests.

## Step 8 — `--ignore <path>` to skip a directory entirely
**Status**: done
**Date**: 2026-07-16
### What was done
Follow-up: a huge `discover ~/ --max-depth 9` run surfaces a lot of
incidental clutter from tool caches (`.cargo`, `.vscode-server`, `go/
pkg/mod`) that the user knows in advance aren't worth scanning at all
— distinct from `--no-project`, which still visits and reports a
container's own finding, just won't merge into it. `--ignore <path>`
skips the walk entirely: no stat, no report, no descent.
Considered reusing the existing name-pattern `ignore_patterns`
mechanism (`decision::ignore_matches`) by converting each `--ignore
<path>` into an anchored pattern via `decision::anchored_pattern` —
rejected: discover's current `ignore_matches` call site anchors
against the *current* directory being walked (`dir`), not `start`, so
a multi-component anchored pattern can never actually match there
(`rel` between `dir` and `dir.join(name)` is always exactly one
component); shoehorning an absolute path through that grammar would've
been silently broken for anything more than one level deep.
Implemented as a genuinely separate, simpler mechanism instead:
`discover::walk`/`walk_inner` gain an `ignore_paths: &[PathBuf]`
parameter, checked via plain equality against the candidate path
right alongside the existing `ignore_matches` check — no pattern
grammar involved. `src/main.rs`'s `Command::Discover` gains `--ignore
<path>` (repeatable, absolutized like `--no-project`/`path`).
### Deviations from plan
Not in the original plan — user follow-up request right after Step 7
landed.
### Issues found / fixed
None. Two new tests (an ignored path's own contents are never found,
even nested several levels down; an unrelated sibling is unaffected)
passed on the first run; all prior `walk(...)` call sites (17 of them)
updated for the new trailing parameter via a scripted find/replace,
verified by the full suite passing unchanged otherwise.

## Verification
`cargo fmt` + `cargo clippy --all-targets --all-features -- -D
warnings` clean throughout. Full `cargo test`: 301 lib tests. Live
smoke test: `discover ~/ --max-depth 9 --ignore ~/.vscode-server
--ignore ~/.cargo` against the real home directory confirmed both
directories produce zero output and the remaining report shrinks to
just the genuinely relevant findings (`~/test/aa/pp-with-subvol`,
`~/test/project-tracked`, a handful of other real project caches).

## Step 9 — condense README, extract discover.md guide
**Status**: done
**Date**: 2026-07-16
### What was done
The `discover` bullet in README's "How it works" had grown into a
single ~250-word paragraph across Steps 4-8, each follow-up appending
another clause. Extracted into a new dedicated `discover.md` guide
(the three undecided kinds, the two drift kinds, the merge algorithm,
`--root-is-project`/`--no-project`/`--ignore`/`--max-depth`, a worked
example) and condensed the README bullet to a short summary + link,
matching the pattern `FAQ.md`/`design.md` already establish for
"detail lives in its own doc, README stays a quick reference."
Applied the same treatment to `convert`/`decide`, which had grown
similarly dense over the session: moved the "existing subvolume
defaults to yes," "what `--dry-run` prints," "`--create` vs. an
anchored decision," and "`decide`'s exact order of operations" detail
into new `FAQ.md` entries, condensing both README bullets to 2-3
sentences each with a `See FAQ.md` pointer.
### Deviations from plan
Not in the original plan — a follow-up documentation request.
### Issues found / fixed
None — a docs-only change; verified no broken internal links
(`discover.md`, `FAQ.md` cross-references) and that no factual detail
was dropped, only relocated. README word count: 2247 → 1955.

## Step 10 — condense "How it works" too, extract how-it-works.md
**Status**: done
**Date**: 2026-07-16
### What was done
Follow-up: "How it works" was still dense (4 long bullets) even after
Step 9's convert/decide trims, and duplicated content already existed
between it and the Configuration section's "Decision files"/"The
project-roots list" subsections. Extracted all of it into a new
`how-it-works.md` guide (intercept/convert/decide mechanics, decision
file syntax, ignore tiers, the project-roots/no-nesting rules),
mirroring `discover.md`'s structure. README's "How it works" section
is now 4 one-line bullets + a link; the Configuration section is now
just the `roots.d` TOML reference (its genuinely distinct topic —
setup, not decision-recording behavior).
Fixed two cross-references that pointed at the now-removed README
anchors: `FAQ.md`'s pre-authoring-decisions entry now points at
`how-it-works.md#decision-files`, and `discover.md`'s nesting
explanation now points at `how-it-works.md#projects-cant-nest`.
Verified via repo-wide grep that no other doc or anchor still points
at the removed `README.md#decision-files`/`README.md#the-project-roots-list`
sections.
### Deviations from plan
Not in the original plan — a follow-up documentation request.
### Issues found / fixed
None — docs-only change. README word count: 1955 → 1010 (across two
condensing passes, original was 2247).
