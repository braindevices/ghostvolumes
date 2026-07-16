# Replace `#`-comment pending markers with a toggleable `?` decision marker

Fixes a real bug just observed live: once a human records a real
decision (`+`/`-`) for a candidate that was previously noted as
pending, the old `# <pattern>` pending comment is never cleaned up —
it sits alongside the new decision line forever, e.g.:

```
+ /**/build
+ /.venv
# /.cache
- /.cache
```

The `# /.cache` line is now pure stale clutter: the candidate *has*
been decided, but the file still shows it looking unresolved.

## Design

**`?` replaces `#` as the pending-marker prefix.** `#` stays a pure,
untouched, human-only comment forever. `?` means "the tool noted this
pattern as seen-but-undecided" — a machine-owned annotation, not a
comment. No changes needed to `decision_core.rs`'s `resolve()`/
`resolve_in_file()`/`parse_lines()` at all: `parse_lines` already
ignores any line that isn't exactly `+`/`-`-prefixed (blank, `#`, and
literally anything else all hit the same `_ => continue`), so a `?`
line is already inert for decision-resolution purposes today, with
zero parser changes. This is purely a write-side convention.

**Three outcomes, two of which are true in-place toggles:**
- "y" (just this path) and "n" (deny) both record the *same anchored
  pattern* `append_pending_comment`/the shim already wrote as `?
  <pattern>` — a real in-place line replacement: `? <pattern>` becomes
  `+ <pattern>` or `- <pattern>`, same line, same position.
- "a" (all matches of this name) records a *different, broader*
  pattern (`/**/name`) than the anchored pending marker
  (`/exact/path`) — not a toggle. The old anchored `? <pattern>` line
  (if present) is removed as superseded, and the new broader `+
  <pattern>` line is appended separately.
- Non-interactive (no TTY): unchanged in spirit — still writes `?
  <pattern>` (not `#`), still deduplicated so repeat runs don't pile
  up duplicate lines.

**New per-boundary decision-file lock**, separate from the existing
per-boundary subvolume-creation lock (`shim`'s `try_create_subvolume`
vs. `convert`'s directory swap). Decision files have only ever been
appended to until now (safe against concurrent appenders by
construction — a single `O_APPEND` `write()` each). Toggling a line in
place is the first read-modify-write on a decision file anywhere in
this design, and needs to be protected from racing a concurrent
appender (the shim, appending a pending marker for a *different*
candidate in the same file at the same moment). Both the shim's
pending-append and `convert`'s pending-append/toggle acquire this new
lock — reuses `lock_core.rs`'s existing `boundary_lock_path`/
`open_lock_file` verbatim, just under a distinct `locks/decisions/`
subdirectory instead of `locks/`, so it can never collide with the
subvolume-creation lock's own filenames for the same boundary.

**No retroactive migration.** Any `# <pattern>`-style pending comment
already written by an earlier build stays exactly as it is — inert,
human-visible clutter forever, same as any other pre-existing `#`
comment. Only newly-written pending markers use `?`. Accepted
explicitly: this project has no external users yet, and a human can
always hand-delete a stale old-style comment same as they always
could.

## Steps

1. `shim/decision_core.rs`: rename `pending_comment_line` →
   `pending_marker_line` (prefix `?` not `#`); rename
   `needs_pending_comment` → `needs_pending_marker` (same dedup logic,
   unchanged). Add `toggle_or_replace_pending(text: &str, pattern:
   &str, decision_line: &str) -> String` — finds an existing `?
   <pattern>` line (exact pattern match) and replaces it in place with
   `decision_line`; if no such line exists, appends `decision_line` at
   the end unchanged. Add `remove_pending(text: &str, pattern: &str) ->
   String` — removes an exact `? <pattern>` line if present, unchanged
   otherwise (for the "a" case's superseded-anchored-marker cleanup).
   Unit tests for both: toggle replaces in place preserving surrounding
   lines/order; toggle appends when absent; remove drops only the
   exact match; both no-op cleanly when absent.
2. `shim/preload.rs`: `append_pending_comment` → use
   `pending_marker_line`/`needs_pending_marker`; take the new
   `locks/decisions/<boundary>.lock` (via `boundary_lock_path` with a
   `locks_dir.join("decisions")`) around its read-check-append, still
   non-blocking `try_lock` (matches the shim's existing non-blocking
   posture elsewhere — never risk freezing a host build on a lock);
   silently skip (already its existing best-effort posture) if
   contended.
3. `src/convert.rs`: same lock (blocking this time, matching
   `materialize`'s existing blocking posture for the subvolume-creation
   lock — `convert` can afford to wait). Non-interactive path: same
   read-check-append as today, just via `pending_marker_line`/
   `needs_pending_marker`. Interactive path: "y"/"n" call
   `toggle_or_replace_pending` with their own pattern and decision line;
   "a" calls `remove_pending` with the *anchored* (just-this-path)
   pattern first, then a plain append of the broader `+` decision line
   (its pattern never coincides with an existing pending marker, so no
   toggle needed there).
4. `filenames.rs`/`filenames_core.rs`: no new constant needed —
   `LOCKS_DIR.join("decisions")` computed at each call site, mirroring
   how `COMPILED_CACHE_FILE_NAME` etc. are joined ad hoc today.
5. `README.md`: update the decision-file example to show `?` instead
   of `#` for the pending-note line, with a short note on what
   distinguishes it from a real `#` comment.
6. Tests: unit tests per step 1 above; `convert.rs` tests for all three
   answer paths confirming correct line management (toggle in place
   for y/n, remove+append for a); update the existing non-interactive
   pending-marker tests (`a_matching_candidate_is_left_alone_without_a_tty...`,
   `a_repeated_non_tty_run_does_not_duplicate_the_pending_comment`) to
   expect `?` instead of `#`; a live cross-check (mirroring the
   shim-vs-convert byte-for-byte comparison already done for the
   plain-pending case) confirming the real shim and non-interactive
   `convert` still produce identical output for the pending case, and
   that a subsequent interactive `convert` run correctly toggles what
   the shim wrote.
7. `cargo fmt` + `cargo clippy --all-targets` + full `cargo test` clean,
   `CHANGELOG.md` entry (next actual release, not this branch).
