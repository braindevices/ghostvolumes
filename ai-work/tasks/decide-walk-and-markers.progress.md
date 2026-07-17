# Progress: decide reuses convert's walk, resolves orphaned markers

## Step 1 — parse_pending_patterns
**Status**: done
**Date**: 2026-07-16
### What was done
`shim/decision_core.rs`: `parse_pending_patterns(text) -> Vec<String>`
— extracts every `? <pattern>` line's own pattern, in file order,
`#[allow(dead_code)]` (shim-dead, CLI-alive, same convention as
`parse_ignore_patterns`). 3 unit tests (extracts in order; empty when
none exist; empty for empty text).
### Deviations from plan
None.
### Issues found / fixed
None.

## Step 2 — Mode, and the walk-based decide/decide_with_io
**Status**: done
**Date**: 2026-07-16
### What was done
- Replaced the plan's originally-proposed `Action` enum (`DecideAndConvert`
  | `DecideOnly`) with a `Mode { decide: bool, convert: bool }` named-
  field struct, per a follow-up design refinement in the same
  conversation — decomposes the terminal action into two independent
  capabilities rather than a fixed set of named combinations, so a
  future "apply only what's already decided, ignore anything
  undecided" mode (`Mode { decide: false, convert: true }`) needs no
  further plumbing when it's ever wanted. `Mode::CONVERT`/`Mode::DECIDE`
  name the two combinations actually wired up today. Named fields
  (not two bare positional `bool`s) so a call site can't silently swap
  them with nothing catching it at compile time.
- `is_tty` stayed a separate parameter, deliberately — it answers "can
  we get a real answer right now" (mechanism), not "do we want to
  consider undecided candidates at all" (`mode.decide`, intent); with
  `mode.decide = true` but no TTY, a candidate still gets a `?`
  pending marker left behind (a breadcrumb for later) — a materially
  different outcome than `mode.decide = false`, which leaves nothing
  behind at all. They aren't the same axis.
- `ask_remember`/`ask_and_maybe_convert`/`resolve_candidate` all gained
  a `mode: Mode` parameter; `resolve_candidate`'s `Some(true)` branch is
  a no-op when `!mode.convert` (existing `+`, decide mode); its `None`
  branch is a silent no-op (no ask, no marker) when `!mode.decide`
  (not reachable from any command yet, but now fully implemented, not
  just plumbed). `ask_remember`'s prompt verb is `mode.convert`-aware
  ("Convert" vs. "Decide").
- Rewrote `decide`/`decide_with_io` per the plan: hand-author `--add`/
  `--deny` verbatim first, then walk via `find_nested_candidates`
  (needs `config_dir`/`max_depth` now, previously unnecessary), then
  resolve any remaining `? <pattern>` marker in the boundary's own
  top-level decision file by reconstructing its implied candidate path
  (`boundary.join(pattern without its leading '/')`) and handing it to
  the *exact same* `resolve_candidate` used for real, walk-discovered
  candidates — `Path` operations don't require the path to exist, so
  no special-casing was needed for the "candidate might not be real"
  case.
- `src/main.rs`: `Command::Decide` gains `#[arg(long)] max_depth:
  Option<u32>`; dispatch loads `config_dir` and passes both through.
### Deviations from plan
Two, both from mid-implementation user feedback (not scope creep — the
plan's `Action` design was superseded before any test relied on it):
1. `Action` → `Mode` (see above).
2. Confirmed (not changed) that `is_tty` stays a distinct parameter,
   after evaluating a suggestion to fold it away.
### Issues found / fixed
Introduced a duplicate `#[allow(clippy::too_many_arguments)]` on
`resolve_candidate` while doing the `Action` → `Mode` rename (it
already had one from the earlier dry-run work); caught immediately by
`cargo clippy`.

## Step 3 — tests
**Status**: done
**Date**: 2026-07-16
### What was done
8 old `decide` tests (verbatim add/deny, toggle-in-place, no-op
registration-only, abort-without-tty, refuses-plain-file, writes-to-a-
shallower-covering-project) mechanically updated for the new
`max_depth`/`config_dir` parameters — their assertions still hold
unchanged, since none of them have any cache rows configured (so the
new walk step trivially finds nothing, leaving the original verbatim-
write behavior the only thing that happens). Added 5 new tests for the
actually-new behavior: walk discovers an undecided candidate and
records-only without materializing; the same, non-interactively, left
as a pending marker; an existing `+` found via the walk is a no-op, not
a re-materialize (with real file content left untouched, proving no
`copy_and_swap` ran); an orphaned `?` marker whose candidate doesn't
exist on disk resolves correctly via the marker scan alone; an
orphaned marker matching a `--deny` pattern resolves during the
upfront verbatim-write step with a panicking `read_line` proving no
ask ever happens for it.
### Deviations from plan
None.
### Issues found / fixed
None — all new tests passed on the first run.

## Step 4 — verification and docs
**Status**: done
**Date**: 2026-07-16
### What was done
`cargo fmt`, `cargo clippy --all-targets --all-features -- -D
warnings` clean. Full `cargo test` green: 269 lib tests (up from 261).
Live smoke test reproducing the user's exact reported scenario (a
decision file with an existing `+`, a human comment, and two `?`
markers for names with no corresponding directory, plus `node_modules`
existing undecided on disk) confirmed: the walk found and left a
pending marker for `node_modules`; the existing `+ /.venv` was
correctly left alone (not re-materialized); both orphaned markers
(`.cache`, `venv`) were resolved via the marker scan; nothing was ever
converted. `README.md` updated for the new three-step behavior and
`--max-depth`.
### Deviations from plan
None.
### Issues found / fixed
None.

## Follow-up: generalize decide's marker-scan to convert too
User live-tested against their own `~/test/project-tracked` fixture
(`convert <path> --dry-run` with an existing `+ /venv2` decision, where
`venv2` isn't a watched name and doesn't exist on disk) and found
`convert` never surfaced `venv2` as a candidate at all — silently
never materializing a decision it had already recorded. Root cause:
`find_nested_candidates` can only discover directories that already
exist and match a watched name; an anchored `+` (or `?`) decision for
an unwatched, not-yet-created name is exactly the same shape of gap
`decide`'s marker-scan had already fixed for `?` markers specifically
— just not generalized to `+` decisions or to `convert`.
### What was done
- `shim/decision_core.rs`: renamed/generalized `parse_pending_patterns`
  (`?`-only) into `parse_anchored_exact_patterns` (`+` and `?`,
  filtered to anchored-and-wildcard-free — a single concrete location,
  not a matching rule). `-` lines excluded deliberately (a missing,
  already-denied candidate needs no proactive action).
- `src/convert.rs`: new shared `decision_file_anchored_candidates(boundary)`
  helper, reading the boundary's own decision file and reconstructing
  each qualifying pattern's implied path — an anchored `+` decision is
  the *persisted* equivalent of `--create`. Wired into both
  `convert_with_io` and `decide_with_io`'s candidate gathering
  (alongside `--create` and the filesystem walk), deduped via a
  `BTreeSet` before the existing shallowest-first sort. This also
  *simplified* `decide_with_io` — its separate step-3 marker-scan loop
  is gone, folded into the same unified candidate list and single
  resolution loop `convert` already used.
### Deviations from plan
None from the original decide-walk-and-markers plan — this is a direct
generalization of the same mechanism to `convert`, prompted by live
testing rather than a new design discussion.
### Issues found / fixed
None — new tests (proactive creation of a missing anchored `+` for an
unwatched name; a wildcarded anchored pattern correctly produces no
phantom candidate; dry-run reports "would create" for it; `decide`
surfaces the same candidate but never materializes it) all passed on
the first run. `cargo test`: 273 lib tests (up from 269). Live smoke
test reproducing the user's exact scenario (three anchored `+`
decisions, none yet on disk) confirmed dry-run correctly reports
"would create" for all three real, concrete decisions and invents
nothing for the wildcarded `/**/venv` pattern.

### Open design question raised, not yet acted on
User noted `is_tty = false` is the main reason `Mode.decide` would
ever be `false` today, but a future `--yes`/auto-approve flag would be
a *third* way to resolve an undecided candidate (ask a human /
auto-approve without asking / leave pending) — meaning "how an
undecided candidate gets resolved" may eventually want to be a proper
enum rather than the current `is_tty: bool` + `Mode.decide: bool`
pairing. Not implemented — no `--yes` flag exists yet on any command,
so there's nothing concrete to wire it to; noted here so the next time
an auto-approve flag is actually requested, this note is the starting
point rather than re-deriving it from scratch.

## Follow-up: auto-recording decisions for already-existing subvolumes
User asked for a further automatic behavior: when convert/decide
encounter a watched-name (or, after further discussion, *any*)
directory that's already a real subvolume with no recorded decision,
the decision should get recorded rather than silently skipped forever.
### Design evaluation (before implementing)
First draft (rejected after evaluation, at the user's prompting):
silently auto-write `+` with no prompt at all. Rejected because it
breaks the one invariant the whole project is built on ("every
conversion is backed by a committed record, never a silent guess") —
this would have been the only place a decision gets written with zero
signal from the user, it conflates "happens to be a subvolume" with
"user wants this approved forever", and it overlaps with `discover
--save`'s existing, deliberately-opt-in role for the same scenario.
User confirmed the corrected design: treat it as its own kind of
undecided candidate (same TTY/no-TTY split as any other), but default
to **yes** on an empty TTY answer (unlike every other ask in this
file, which defaults to declining) since a hand-made subvolume is
overwhelmingly likely to have been made on purpose, and never call
`materialize` either way (nothing to convert - and doing so would be
actively wrong: `copy_and_swap`'s final `remove_dir_all` can't remove
a real subvolume, that needs `BTRFS_IOC_SNAP_DESTROY`).
### What was done
- New `ask_about_existing_subvolume` prompt, distinct from
  `ask_remember` — states the reason ("is already a subvolume with no
  recorded decision"), defaults to yes on empty/`y`/`yes`, records `-`
  on anything else (a real decline, not silently forgotten — same
  "prevents re-asking" role `-` already has elsewhere, just without
  affecting the subvolume itself here).
- `resolve_candidate`'s `is_subvolume` check moved to *after* computing
  `existing_decision` (previously the very first check, short-
  circuiting before any decision lookup) so it can tell "already
  decided, skip" apart from "undecided, ask/mark-pending" for an
  existing subvolume. Gated on `mode.decide` (skips entirely, matching
  `Mode`'s existing meaning) and `dry_run` (reports instead of asking),
  same conventions as every other branch.
- **Separately generalized, per explicit follow-up feedback**: the
  filesystem walk (`find_nested_candidates_inner`) no longer requires a
  candidate's name to be on the watch list at all if it's already a
  real subvolume — a subvolume is itself direct evidence someone
  already decided to convert it, whether or not its name is (or ever
  was) configured as watched. Never descended into either way, same as
  a watched-name match.
### Deviations from plan
The walk-level generalization (not requiring a watched name for an
already-existing subvolume) was a follow-up the user added mid-
implementation, not part of the original ask — folded in immediately
since it's the same underlying principle and touches the exact same
code path.
### Issues found / fixed
An unused `target` binding in a new test caught by `cargo clippy`
(the assertion checks the decision file directly, not the target path)
— removed. Otherwise green on the first full test run: 9 new tests
(existing-decision no-op with content assertion; no-TTY pending-marker;
TTY default-yes; TTY explicit decline; dry-run report; walk discovers
an unwatched-but-already-subvolume name; walk still never candidates a
plain unwatched-name directory; a direct unit test of the not-yet-
wired-to-any-command `Mode { decide: false, convert: true }` skip).
280 lib tests total, fmt/clippy clean.
