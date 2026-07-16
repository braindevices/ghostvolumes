# `convert`: project model, ignore patterns, dry-run, decide subcommand

Four phases, implemented and committed independently. Phase 1 fixes the
real problem behind a fresh false-alarm report of "convert converts its
own root": the original report was a test artifact (a stray
`+ <exact-path>` line left in a decision file from earlier manual
testing — removing it fixed it, no code bug there). But digging into
*why* it was confusing surfaced a real design problem: `convert <path>`
conflates two different things under one argument — "the project
boundary" and "a thing that might get converted" — and silently guesses
at project-root registration via `walkup_boundary`'s parent-fallback
rather than ever asking. Phases 2-4 are follow-on requests gathered in
the same conversation, sequenced after Phase 1 since two of them
(dry-run, `decide`) build directly on its debug-tracing and
project-registration machinery.

## Phase 1 — separate "the project" from "things to convert"

**`<path>` (the CLI's positional argument) is now purely "the project":
the decision-file/project-roots boundary. It is never itself added to
`candidates` and never itself converted.** This fully resolves the
"root gets folded into a subvolume" class of confusion without losing
the single-target use case (pre-create/convert one specific directory
directly) — that case moves to the new `--create` flag below, which is
unambiguous about intent in a way the overloaded positional argument
never could be.

**Project registration becomes an explicit, upfront step, not a side
effect.** Before touching any candidate: if `<path>` isn't already in
the project-roots list, ask "Register `<path>` as a project? [Y/n]"
(same TTY/non-interactive posture as every other prompt in this file —
non-interactive defaults to declining). **Declining aborts the whole
command** — registration is a hard prerequisite, not a soft
preference, so a script that can't be asked doesn't proceed to guess.

**Default behavior (no flags) walks `<path>`'s children exactly as
today** (`find_nested_candidates`'s existing watched-name-matching
walk) and resolves each discovered candidate exactly as today
(already-subvolume skip; an existing decision applies directly;
undecided → the existing ask-then-convert / non-interactive
pending-marker flow). This *includes* asking about brand-new undecided
matches by default — no separate opt-in flag needed for that, since
per-candidate consent already happens through the existing ask gate.

**`--create <relative-path>` (repeatable)**: explicitly names specific
subpaths (relative to `<path>`) to resolve/convert directly — the
direct replacement for what naming `<path>` itself used to do.
Bypasses `matches_a_watched_name` entirely: naming something via
`--create` *is* the explicit signal of intent that check used to
approximate for the positional argument. Goes through the exact same
per-candidate resolution as anything the walk discovers.

**Leveled debug tracing**, reusing the shim's own `GHOSTVOLUMES_DEBUG`
env var convention rather than inventing a separate one: with it set,
`resolve_candidate` prints (to stderr, alongside existing prompts) a
line per candidate explaining which branch fired and why (already a
subvolume; existing `+`/`-` decision found in which file for which
pattern; no decision, watched-name check passed/failed and why). Fixes
"debug is rather difficult because we use raw println" — would have
made the original stray-decision-file scenario immediately diagnosable
instead of requiring a manual `cat` of the decision file.

### Steps

1. `src/main.rs`: `Command::Convert` gains `#[arg(long = "create")]
   create: Vec<String>`. Dispatch resolves each `--create` value against
   `path` (join) before calling into `convert`.
2. `src/convert.rs` — `convert_with_io`/`convert`: new `create: &[PathBuf]`
   parameter.
   - Upfront: if `path` isn't in the parsed project-roots list, ask to
     register; on decline (or non-TTY), return an error/abort rather
     than `Ok(())`.
   - `candidates` seeding changes from `vec![path.to_path_buf()]` to
     `create.to_vec()` — `path` itself is never appended.
     `find_nested_candidates(path, ...)` still runs and extends
     `candidates` exactly as today.
   - `resolve_candidate`'s `candidate == top_level_path` special-casing
     (override-confirm, `matches_a_watched_name` gate) re-scopes to "is
     this one of the explicit `--create` paths"; the watched-name gate
     is dropped entirely for `--create` entries.
3. Debug tracing: a small `debug_trace(is_debug: bool, candidate: &Path,
   message: &str)` called at each branch point in `resolve_candidate`,
   gated on `GHOSTVOLUMES_DEBUG` (read once, threaded down like `is_tty`).
4. Tests: touches nearly every existing `convert::` test (most
   currently pass the target directly as `path`). New tests: upfront
   registration ask (register/decline-aborts/already-registered-skips);
   `--create` bypassing the watched-name gate; debug tracing emits
   expected lines per branch.
5. `README.md`: update the `convert` row and "How it works" for the new
   `<project-path> [--create <path>]...` shape and registration prompt.
6. `cargo fmt` + `cargo clippy --all-targets` + full `cargo test` clean.

## Phase 2 — configurable ignore patterns

Problem: the walk (`find_nested_candidates_inner`, `discover::walk`)
hardcodes skipping only `.git` — an incomplete, unmaintainable list
(`.hg`, `.svn`, etc. all walked into today). Needs to be configurable,
not exhaustively hardcoded.

**Reuses the exact same pattern grammar decision files already use**
(`name`, `/name`, `/a/b/**/name` — `decision_core.rs`'s
`pattern_matches`), for a different purpose: skip descending entirely
(never even check for a watched-name match), not decide conversion.

Three tiers, unioned (matching *any* tier skips):
- **Global**: `roots.d`'s schema gains `default-ignore = [".git", ".hg",
  ".svn", ".snapshots"]`, sibling to `default-watches` — same
  centralized-config precedent, since there's no single directory for
  a "global" file.
- **Volume root**: a `.ghostvolumes-ignore` file at the `roots.d`-
  registered root path itself.
- **Project root**: a `.ghostvolumes-ignore` file at the registered
  project-roots boundary.

Each of the volume-root/project-root files exists *only* at that one
boundary location (not walked-up through every intermediate directory
like decision files) — but patterns inside can still use `**` to reach
arbitrary depth, same as a single `.gitignore` at a repo root reaching
deep paths.

### Steps

1. `shim/decision_core.rs` (or a new small shared module): expose a
   thin `ignore_matches(patterns: &[String], dir: &Path, path: &Path) ->
   bool` wrapping `pattern_matches` for a flat pattern list (no `+`/`-`
   prefix parsing needed — ignore files are just bare patterns, one per
   line, `#` comments allowed, blank lines skipped).
2. `src/config.rs`/`merge.rs`: `RootsFile`/`ResolvedRoot` gain
   `default_ignore`/per-resolution `ignore: Vec<String>` analogous to
   `default_watches`/`watches` — but *not* per-root-overridable the same
   way (no per-root `["/path"] ignore = [...]` override; global-only,
   since per-root/per-project ignores are explicitly decentralized into
   `.ghostvolumes-ignore` files instead, per the design above).
3. `src/convert.rs`'s `find_nested_candidates_inner` and
   `src/discover.rs`'s walk: before recursing into a directory, check
   it against the union of (global `default-ignore`, the nearest
   `.ghostvolumes-ignore` at the volume root, the nearest one at the
   project-roots boundary if registered) — replaces the hardcoded
   `.git` check.
4. Tests: ignore-pattern matching unit tests; walk tests confirming
   `.hg`/`.svn`/a custom project-level ignore pattern are never
   descended into; confirm a `.ghostvolumes-ignore`'d directory is
   skipped even if it would otherwise match a watched name.
5. `README.md`: document `.ghostvolumes-ignore` alongside
   `.ghostvolumes-decisions`, and `default-ignore` alongside
   `default-watches` in the `roots.d` section.

## Phase 3 — dry-run mode

`convert --dry-run`: walks and resolves decisions exactly as normal,
but never calls `materialize`/writes to the decision file — prints what
*would* happen instead, reusing Phase 1's debug-tracing infrastructure
("would create: X", "would convert: X (existing + decision)",
"undecided: X, would ask (skipped — dry run)"). Never actually prompts
interactively either, since the entire point is zero side effects,
including not blocking on stdin.

### Steps

1. `src/main.rs`: `Command::Convert` gains `#[arg(long)] dry_run: bool`.
2. `src/convert.rs`: threads `dry_run` down to `resolve_candidate`/
   `ask_and_maybe_convert`; short-circuits before any `materialize`/
   `append_pending_marker`/`record_decision`/`register_project_root`
   call, printing the planned action instead via the same reporting
   path Phase 1's debug tracing uses.
3. Tests: dry-run leaves the filesystem and decision file completely
   untouched for every branch (already-subvolume, existing decision,
   undecided), while still printing the expected "would ..." line.
4. `README.md`: document `--dry-run`.

## Phase 4 — `decide` subcommand

`ghostvolumes decide <project-path> --add <pattern> --deny <pattern>`
(both repeatable) — a new subcommand, not a `convert` flag, matching
the existing `roots`/`projects` one-subcommand-per-concern precedent.
Pure decision-file bookkeeping: reuses the Phase 1 upfront project-
registration step and `record_decision`'s toggle-in-place logic, but
never walks the filesystem and never calls `materialize`. Backs the
"hand-author decisions ahead of time" workflow `decision-model.plan.md`
already documents as a first-class scenario, without requiring an
interactive `convert` session to populate the first decisions.

### Steps

1. `src/main.rs`: new `Command::Decide { path: String, #[arg(long)] add:
   Vec<String>, #[arg(long)] deny: Vec<String> }`.
2. `src/convert.rs` (or a new `src/decide.rs` reusing `convert.rs`'s
   private helpers via `pub(crate)`): same upfront registration
   ask/abort as Phase 1. For each `--add <pattern>`, `record_decision`
   with prefix `+`; each `--deny <pattern>`, prefix `-`. Patterns are
   used verbatim (no anchoring/broadening computed — the human is
   directly specifying the pattern, unlike `convert`'s "y"/"a" choices).
3. Tests: `--add`/`--deny` write/toggle correctly; registration
   ask/abort reused correctly; no filesystem mutation ever happens.
4. `README.md`: document the `decide` subcommand.

## Explicitly out of scope (all phases)

- Any change to `intercept`/the shim's own decision resolution — CLI-
  side only throughout.
- `discover`'s suggested-decision-line output shape, beyond Phase 2's
  ignore-pattern integration into its walk.
