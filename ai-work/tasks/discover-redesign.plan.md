# Redesign `discover`: respect decisions, cover unwatched/unconverted, advise don't write

`discover` currently only reports a watched-name-and-already-a-subvolume
match, with no regard for whether a decision already covers it, and
`--save` writes raw decision-file lines directly, bypassing project
registration and the no-nested-projects invariant entirely. Live
testing against a real, partially-decided tree surfaced three problems:
it re-suggests things that are already fully decided (pure noise,
`.venv`/`build` already recorded for `project-tracked`), it silently
ignores an already-existing subvolume with an unwatched name, and it
has no way to safely propose registering a project for a batch of
independent, possibly-nested matches in one command.

## Design

Three tiers, all filtered to *undecided only* — `decision::resolve`
against `<path>` (the discover invocation's own argument) as the
walk-up boundary, reusing the exact same logic `convert`/`decide` use
against their own registered boundary. Nothing already covered by a
`+`/`-` anywhere between a match and `<path>` is reported at all — this
is the core fix; everything else follows from it.

1. **Watched name, already a subvolume** — a confident candidate.
   Suggests `ghostvolumes decide <dir> --add <name>`.
2. **Unwatched name, already a subvolume** — same underlying gap
   `convert`/`decide`'s own walk just got fixed for (a subvolume is
   evidence of an already-made decision, regardless of the watch
   list), but discover can't ask interactively the way `convert`/
   `decide` can, so it can't default to yes on its own. Suggests
   `ghostvolumes decide <dir> --add <name>   # or --deny <name>`,
   presenting both options rather than picking one.
3. **Watched name, not yet a subvolume** — report-only, no command
   suggested: purely informational awareness ("these exist, watched,
   untouched"), not a real decision-file marker of any kind (`-` here
   would misrepresent "nobody decided" as "a human declined", the one
   thing `-` means everywhere else in this project).

**No batch registration/writing in discover at all** — it never
touches the project-roots list or any decision file. Two suggested
groups happening to be nested is fine: whichever `decide` command runs
second will hit that project's own existing nesting-conflict detection
(`ensure_project_registered`) and handle it exactly the way it already
does today, in whatever order a human chooses to run them.

`--save` is removed — "write this raw content to the nearest file"
doesn't fit a world where decisions need to go through registration/
boundary resolution to land in the right place; that's `decide`'s job,
done safely already.

## Steps

1. `src/discover.rs`: `MatchKind` enum (`ApprovedCandidate` |
   `UnwatchedSubvolume` | `NotYetConverted`); `DiscoveredMatch` gains a
   `kind` field. `walk_inner` checks both `is_watched` and
   `is_subvolume` independently (not just watched-name-gated
   subvolume-checking) and classifies into the three kinds, filtered by
   an `is_undecided` check (`decision::resolve` against `start`).
   Never descends into any of the three kinds, matching the existing
   "never descend into a match" convention. `ProjectSuggestion` gains
   three name lists (one per kind) instead of one flat list;
   `group_by_parent` buckets accordingly. `format_decisions` →
   `format_report`, rendering the new advisory/command-suggestion
   shape instead of raw decision-file syntax.
2. `src/main.rs`: `Command::Discover` drops `--save` (and the
   write-decision-file dispatch logic that went with it).
3. Tests: full rewrite — the three-kind classification (including the
   "watched AND subvolume" vs "watched only" vs "subvolume only"
   matrix); "already decided, anywhere up to `start`" correctly
   suppresses a match at every tier; report format renders the right
   command per tier; no `--save` surface left to test.
4. `README.md`: rewrite discover's description and the commands table
   row.
5. `cargo fmt` + `cargo clippy --all-targets -- -D warnings` + full
   `cargo test` clean.
6. Commit on `claude-convert-project-model` (already the active
   branch).
