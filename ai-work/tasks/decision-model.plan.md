# Replace the git-tracked gate with an explicit approve/deny decision model

Supersedes plan §4's "Git-tracked gate" and its use at all three call
sites (LD_PRELOAD, cd-hook, `convert`). Along the way, cd-hook
(`ensure`) itself is also removed entirely (§7), and per-project
`.ghostvolumes.toml`/`projects.d` is replaced by decision files (§3,
§7) — the small "which paths are known project roots" list that
replaces `projects.d`'s boundary-marking role is a new, separate,
plain-text file, not a TOML config directory. Larger in scope than
just the gate replacement, but all part of the same simplification
direction. Not yet released, so this is a breaking behavior change
with no migration concern — accepted explicitly.

## Problem with the current design

`is_git_tracked()` (`shim/git_core.rs`, shells out to `git ls-files`) is
the sole safety check before the LD_PRELOAD hook silently converts a
directory into a subvolume. Three problems, in the user's own words:
1. Git isn't the only VCS — this only ever protected git repos.
2. Shelling out to `git`'s CLI is a fragile external dependency to lean
   on for a correctness-relevant decision.
3. The failure modes are asymmetric: skipping a conversion that
   should've happened costs an optimization; wrongly converting a path
   that shouldn't have been loses snapshot coverage for whatever's
   inside it (a subvolume's contents aren't included in a snapshot of
   its parent) — real, if latent, data-loss risk on restore.

Building a *correct* multi-VCS version of this check is real, ongoing
complexity for something that would still only ever be a heuristic —
"detect which VCS, then ask it" gives false confidence, not actual
safety. Decision: **drop VCS detection entirely.** The only safety net
is an explicit, recorded human decision.

## Goals

- KISS: minimal new machinery, reuse what already exists wherever
  possible (the log file, `convert`'s copy-and-swap, the existing
  per-project config model).
- Highest transparency: every subvolume ever created traces back to
  either a recorded `+` decision or an explicit env-var opt-out of
  asking at all — never an unexplained heuristic.
- Default to the safe outcome (skip) whenever asking isn't possible.

## How this plays out in practice

Worth stating explicitly, since it's a direct (and correct) consequence
of "nothing gets decided without a prior decision existing somewhere,"
not a gap: **on a totally fresh project with no decision file at all,
running with `intercept` provides zero benefit over a plain build** —
every candidate is undecided, so the shim can only skip-and-comment
(§4); there's nothing for it to actually convert yet. `intercept` only
starts earning its keep once a project already has *some* decisions,
which happens one of three ways:
1. **Brand new, nothing yet:** build plain (no `intercept` needed at
   all), then run `ghostvolumes convert <project-root>` once — its
   recursive walk (§6) finds everything and resolves it interactively
   in one pass, populating the decision file for the first time.
   `intercept` becomes useful starting with the *next* build.
2. **Self-authored ahead of time:** someone who already knows the
   project's conventions hand-writes `+`/`-` rules (§5) before ever
   building — `intercept` does useful work on the very first run in
   this case, since decisions already exist to act on.
3. **Inherited via git:** once either of the above has happened once
   and the decision file is committed, anyone else cloning/pulling the
   repo gets those decisions for free — their own first build with
   `intercept` benefits immediately, and `convert` on the project root
   is only needed afterward for anything genuinely new since.

## Design

### 1. Decision files — gitignore-style, nestable

A plain-text file (name TBD — using `.ghostvolumes-subvol` as a
placeholder below), one per directory, same idea as `.gitignore`:
lines are `+ <pattern>` (convert) or `- <pattern>` (never convert).
Pattern forms, resolved relative to the *file's own directory*:
- `/name` — anchored: that exact single location only.
- `name` — unanchored: matches at any depth under this directory.
- `/a/b/**/name` — anchored prefix, arbitrary depth after it.

Deliberately **not** a full gitignore clone: no `!` negation, no
character classes, no mid-pattern `**`. Just these three forms.

**Resolution rule** (simpler than real gitignore precedence, which
layers files and allows negation — not worth cloning that complexity):
walking from a candidate path up to the project root, the **closest
enclosing file that has any matching pattern wins**, full stop. Within
one file, the **last matching line wins** (lets a user add a narrow
override after a broad rule).

**Auto-added decisions always append to the top-level (project-root)
file only.** Nesting is a manual/advanced feature a user can hand-author;
the tool itself never creates a nested file.

These files live in the project tree and are meant to be committed —
same model as `.gitignore`: one person's decision benefits the whole
team, and the file itself is a transparent, reviewable record.

### 2. Shared pattern matcher

New `shim/decision_core.rs` (same `include!()`/`mod` sharing pattern as
`cache_core.rs`/`xdg_core.rs`): parses one decision file's lines and
matches a candidate path against them. One shared walk-up/resolution
function, used identically by the CLI (`reload`, discovering decision
file locations to compile as project-root markers — §3; `convert`'s
recursive walk — §6) and the shim (§3, §4). Dependency-free,
hand-written — this pattern subset doesn't need a crate.

### 3. Resolved live, not compiled

**Revised: decisions are never baked into `compiled.tsv` or any other
precompiled cache.** `compiled.tsv` exists at all because the shim
can't parse TOML (no dependency resolution) and BTRFS validation is
expensive, so both get done once, centrally, at `reload` time — neither
reason applies to decision files. They're already in a dependency-free,
hand-parseable format (that's the whole point of the gitignore-style
syntax), and `shim/decision_core.rs` (§2) is dependency-free and shared
by both sides already. So the shim just reads decision files directly,
the same way `is_git_tracked` used to walk up looking for `.git` —
except now it's plain file reads instead of a subprocess spawn, so
it's cheaper than what it replaces, not more expensive.

**How the shim knows where to stop walking up.** `decide()` already has
`CACHE_ROWS` loaded in memory for the existing name check, and that
same data trivially supports a second query: the *longest* (most
specific) row prefix that is an ancestor of the candidate path — a
plain `max_by_key(len)` over rows already in hand, no new file I/O on
the hot path. That becomes the walk-up's stopping boundary. One small
addition to `cache_core.rs`, a sibling to the existing `names_for` — a
`longest_matching_prefix`-style helper over the same rows.

**Where the narrower rows come from — a plain-text, live-read list, not
TOML, not compiled, not a filesystem scan.** A first draft of this plan
had `reload` walk every root's entire filesystem tree looking for
decision files to auto-discover project boundaries — rejected: that's
exactly the kind of unbounded, expensive operation this design
otherwise goes out of its way to avoid (it's why `discover`'s own
tree-walk is an explicit, occasionally-run command, not something
`reload` does on every invocation). A second draft routed a "project
roots" list through TOML + `reload`'s compile step, matching
`roots.d`'s own pipeline — also unnecessary: that pipeline exists for
`roots.d` specifically because BTRFS validation is genuinely expensive
and needs to happen centrally; a registered project-root path needs no
such validation at all (it's just an arbitrary path *within* an
already-validated root, inheriting validity from that). **So it follows
decision files' own philosophy instead: a plain-text file, one path per
line, read live — no TOML, no `reload` involvement, no compilation.**
The shim reads it once per process (a `OnceLock`, same cost profile as
`CACHE_ROWS`) and unions it with `roots.d`'s compiled entries when
computing `longest_matching_prefix` — one more small file read at
process startup, nothing on the hot path. Populated two ways, neither
touching `reload` or TOML at all:
1. **Explicitly, for proactive use** — `ghostvolumes register <path>`
   (name TBD), for someone who wants the walk-up boundary benefit from
   the very first build, before any decision has ever been recorded.
   Just appends the path directly to the plain-text file.
2. **Automatically, as a free side effect of `convert`** — since
   `convert` (§6) already resolves and writes to a decision file at a
   specific path, it also silently, idempotently appends that path to
   the same file if it isn't already there — no extra question, a
   natural side effect of recording any decision at all.

A project that's never been registered either way still works
correctly — it just falls back to the broader `roots.d` boundary,
meaning a longer (but still bounded, still cheap-per-level) walk-up for
deeply-nested candidates until something registers it. Real but
bounded, not a correctness gap.

Using the *longest* match (not just the broader `roots.d` entry) is
also the more precise choice, not merely convenient: `roots.d` entries
can be very broad (potentially a whole `roots = ["/"]`-style Snapper
setup spanning many unrelated projects), and walking all the way up to
one for every candidate would be both slower (many more levels to
check when nothing's found) and semantically loose (a decision file
sitting at a broad, shared root would apply to everything under it).
Stopping at the nearest registered project-root marker when one exists
avoids both.

**This only runs for candidates that already passed the existing
`compiled.tsv` root/name filter** — that filter is unchanged, still
centralized, still the first-line cheap rejection (a handful of string
comparisons) for the overwhelming majority of system-wide `mkdir`
calls that aren't anywhere near a configured root. Only once a
candidate is confirmed to match a watched name under a configured root
does the shim walk up from it, resolving decision files, bounded by
that same matched root (no need to go further up — decision files
above the root the tool manages aren't relevant). This keeps the hot
path exactly as cheap as it is today; the walk-up cost only applies to
an already-rare, already-filtered subset.

**No per-process caching of decision resolution — it's redone fresh on
every intercepted call, unlike `CACHE_ROWS` (which *is* a `OnceLock`,
loaded once per process).** This is what makes the shim never need to
"wait" for or be "told about" a decision: every single `mkdir` call
resolves synchronously, once, against whatever the decision files
currently say at that exact instant, and acts immediately — found a
decision, act on it; found nothing, skip, exactly as always. There is
no waiting, retrying, or timeout anywhere in this path, for any call,
ever. Once a human hand-edits a comment into a real decision (§5), any
*later* `mkdir` call for that pattern — in any future process, any
future run — simply sees it, the same way any call always sees
whatever's currently on disk. Nothing needs to be reloaded or waited
on for that to work, because nothing was ever cached across calls to
begin with.

**Performance, made concrete:** a walk-up is a handful of `stat()`/
`open()` calls (2-4 typically, bounded by how deep the match is under
its root) — microseconds, mostly hitting warm dentry/page cache since
a build keeps revisiting the same project structure. This *replaces*
what `is_git_tracked` used to cost for this exact same already-filtered
subset — which, on a match, forked and exec'd an entire `git`
subprocess — so it's strictly cheaper than what it replaces, not a new
expense. What actually bounds the cost isn't parallelism, it's volume,
and the volume reaching this point is already small by construction:
gated by the same root-match-then-name-match double filter the old
check relied on too. A worst case (something creating many thousands
of matching directories in a tight loop) would still cost less in
aggregate than the old per-call subprocess spawn for the same volume.
**Not doing yet:** in-process memoization of already-checked ancestors,
in case a single process calls `mkdir`/`mkdirat` many times for paths
sharing ancestors within its own lifetime — no evidence yet this
matters, easy to add later if profiling ever shows otherwise.

The immediate, practical payoff: there's nothing to keep in sync.
When a human hand-edits a decision into the file (§5), the very next
intercepted call anywhere that walks up past it sees the change
immediately, because it's reading the live file, not a snapshot. No
recompilation step, no cache-invalidation logic, no "how do we know
when to recompile" question at all.

### 4. Shim behavior (`decide()` in `shim/preload.rs`)

Replaces the `GitTracked` branch entirely. New decision branches (all
resolved live via `decision_core.rs`'s walk-up, §3):
- A recorded `+` for this path → `Accept` (as today).
- A recorded `-` → `Skip (denied)`.
- No decision found in any decision file along the walk-up:
  - `Skip (undecided)` — log this as an *important* (always-on, not
    debug-gated) line, same log file as today.
  - **Also append a comment line noting the candidate to the bottom of
    the resolved project's own decision file** (the same file the
    walk-up just read, no separate queue file at all) — e.g.
    `# /packages/foo/node_modules`. Best-effort dedup: a quick read of
    the file first, skip appending if that exact candidate is already
    present as either an active decision or an existing pending
    comment. Not airtight under heavy concurrency (a check-then-write
    race across processes), but harmless if it isn't — a duplicate
    comment is just an extra line to ignore or delete, not a
    correctness problem.
- **`GHOSTVOLUMES_AUTO_YES=1`** (env var, name TBD) set → skip the
  lookup entirely, always `Accept`, like the tool's original fully-
  automatic behavior. Nothing gets recorded — the env var itself is
  the standing approval. Documented as not recommended.

**Revised invariant: the shim never writes an *active* `+`/`-` decision
line, ever, in any mode — only inert comment lines noting undecided
candidates.** Only a human, hand-editing the file, ever turns a comment
into a real decision. This is a deliberate loosening of the earlier
"the shim never writes a decision file at all" rule, now that there's
no separate queue file to write to instead — the shim still never
*decides* anything, it just leaves a breadcrumb in the one file that
already exists for this purpose.

**Concurrent appends need no locking**, for the same reason established
earlier for the (now-removed) separate queue file: the file is opened
`O_APPEND`, and POSIX guarantees the seek-to-end-and-write happens as
one atomic kernel operation, so multiple shim instances (in independent
subprocesses of a parallel build) appending "at the same time" never
land at the same offset and clobber each other. That atomicity is per
single `write()` syscall, not per logical line, so the implementation
discipline that matters: build the whole comment line as one buffer and
issue exactly one `write_all` for it. Same technique the shim's
existing log-file writer already relies on. Concurrent *reads* (the
walk-up resolution happening in another process at the same moment)
never see a torn line either, for the same reason in reverse.

### 5. No *new* interactive command — the decision file *is* the UI

**Revised, once more, and further simplified: no `ghostvolumes decide`
(or `review`) subcommand, no separate pending-queue file, no *new*
interactive prompting anywhere in this design.** (§6 below covers
`convert`, an already-existing, already-deliberate CLI command with no
new UI needed — this section is specifically about the
notification/reactive side having no interactive component at all, not
a claim that nothing in the whole design can ever ask anything.) The
undecided comment lines appended in §4 land directly in the project's
own decision file — the same file the human already knows how to
hand-edit (nesting, custom anchors, etc. were already an expected
manual/advanced feature). Reviewing a decision is nothing more than
opening that file in a normal text editor, turning
`# /packages/foo/node_modules` into `+ /packages/foo/node_modules` (or
`-`, or broadening it to `+ /packages/foo/**/node_modules`), and
saving. No ghostvolumes-specific tooling is involved in making the
actual decision this way.

**Notification is free, because the file is meant to be committed.**
Since decision files live in the project tree and are meant to be
version-controlled (§1), the shim appending a new comment line makes
the file show up as modified under a plain `git status`/`git diff` —
already part of how anyone working in the repo naturally checks in.
No custom notification mechanism is required for this to work at all;
it's a direct consequence of putting the pending marker in the same
file that's already under version control.

`intercept` (below) still prints a short, optional courtesy notice
when it knows a comment was just added, purely because not everyone
checks `git status` immediately after every build — but this is
genuinely optional polish, not the load-bearing notification path.

**Nested same-name matches (e.g. `/build/build`), mostly resolved by
§6:** by the time a human acts, the whole build already ran unimpeded,
so `/build/build` can already exist as a plain directory *nested
inside* the still-plain `/build`. Editing the `/build` comment alone
doesn't retroactively convert anything — a decision only changes what
happens on the *next* `mkdir` for a matching path, which may never come
again for something that already exists. But `convert`'s walk (§6)
closes this in the common case: since it descends *into* whatever path
it's given, running `ghostvolumes convert /build` (the suggested,
covering-ancestor command from `intercept`'s notice) also discovers
and asks about `/build/build` in the same invocation. The
residual gap is narrower now: only if a human hand-edits the decision
file directly (§5) *without* ever pointing `convert` at a covering
ancestor afterward would a nested match go unconverted — accepted as a
rare, self-correcting case (the next `convert` run touching that
subtree catches it).

**Considered and deferred:** a persistent bottom status bar so the
shim could surface prompts live during a build without disrupting its
output. Real terminal-multiplexing work (pty allocation, reserved-region
redraw, resize handling) — moot anyway now that there's no prompting
UI at all to move earlier; noted here only because it was seriously
considered along the way.

**Why the shim can never be the one prompting, stated plainly** (this
is *why* it only ever leaves a comment behind, never asks anything): a
wrapped command can spawn many subprocesses that each independently
trigger the shim, and most of *their* own stdin/stdout/stderr are
whatever the build script itself set up — piped into another command,
redirected to a file, `/dev/null`, whatever. There's frequently no
usable terminal-connected fd to write to *at all*, regardless of mode.
The design never relies on any of those processes' stdio — the shim
only ever writes to plain files, and the only "interactivity" anywhere
in this design is a human opening a file in their own editor, on their
own schedule, entirely outside any ghostvolumes-controlled process.

**Considered and rejected, earlier in this design process: an explicit
`ghostvolumes decide`/`review` subcommand** with a custom TTY-checked
prompt loop (or an editor-launching wrapper). Dropped once it became
clear the decision file itself, already expected to be hand-edited,
could serve as the entire review surface with no additional tooling —
simpler, and consistent with treating the file as the one source of
truth rather than layering a second interface on top of it.

**Considered and rejected: rely purely on the log file, don't write to
the decision file at all.** The shim already logs "SKIP (undecided)" as
an always-on line regardless of debug mode, so in principle `intercept`
could scan *that* instead of writing anything to the decision file, and
just tell the human which paths to run `convert` on. Simpler for the
shim (no dedup-check-before-write step at all) — but it gives up
something real: the log file is personal, unshared XDG state, while the
decision file lives in the project tree and is meant to be committed.
Writing only to the log makes "there's a pending decision" purely local
and ephemeral — if whoever ran the build doesn't act on it immediately,
or their log rotates, there's no team-visible trace anything was ever
found. The comment-in-decision-file approach gets `git status`/`git
diff` visibility for free specifically because it lands in a committed
file — the same property that makes the whole decision-file design
transparent and shareable to begin with (§1). Rejected: that's worth
more than the small implementation simplification.

**One entry point, and it never prompts for anything:**
`ghostvolumes intercept -- <cmd>` (new) sets `LD_PRELOAD` for the child
process only (works standalone, no shell-rc setup required), execs with
stdio fully inherited, waits — completely normal passthrough while
`<cmd>` runs, no redirection, no flags. `<cmd>` is a genuinely separate
process — the parent (`intercept` itself) never has `LD_PRELOAD` set on
itself, so the shim only ever loads into the child and its descendants,
never the parent. Only *after* `<cmd>` exits (full foreground control
back, no longer racing with anything) does it check whether this run
appended any new comment lines, and if so, print a short notice
**naming the single covering `ghostvolumes convert <project-root>`
command** (the decision file's own location — the
`longest_matching_prefix`, §3) rather than one command per pending
path — since `convert`'s walk (§6) now resolves every pending candidate
under that root in one invocation, that's strictly more useful than
listing individual paths, and it's what actually closes the
nested-match gap too. Hand-editing the file directly remains available
as an alternative (e.g. to pre-decide several at once before ever
running `convert`), but the printed notice always suggests the one
concrete command.

**No cd-hook (`ensure`) at all — removed entirely, not just reduced to
a notice.** It previously did three things, all of which have a home
elsewhere now: proactive pre-creation of names on `cd` (replaced by
just running `convert` explicitly, extended in §6 to create a
not-yet-existing path directly as an empty subvolume — see below);
automatic repo-local-*config* registration on `cd` (unnecessary now
that there's no rich per-project config file left to register — a
project's naming/approve-deny conventions are entirely decision files
now; only a *lightweight path* still needs registering, via `register`
or `convert`'s own side effect — see §3); and the two things added
*during* this redesign, a pending-decision notice and applying an
already-`+` decision to a still-plain directory, both of which turn out
to already be fully covered by `intercept`'s notice (above) and
`convert`'s existing "already `+`, still plain → convert directly"
resolution case
(§6) respectively. Nothing was left over needing a replacement.

#### What actually applies an edited decision to an already-existing directory

Editing a comment into `+ /packages/foo/node_modules` doesn't convert
anything by itself — a decision only changes what happens on the
*next* `mkdir` for that path, and the directory in question already
exists as plain (that's why it was undecided in the first place).
**No new mechanism needed to close this loop**: `convert`'s own
resolution logic (§6) already handles "a `+` decision exists but the
target is still plain → convert directly" as one of its ordinary
candidate outcomes. The human edits the file, then runs
`ghostvolumes convert <project-root>` (the same command `intercept`'s
notice already suggests) whenever they choose to — no cd-triggered
automatic step required.

### 6. `ghostvolumes convert <path>` becomes a recursive walk-and-resolve, with an interactive "remember this?" step

`convert` is fundamentally different from the reactive shim path and
from `intercept`'s notice: it's a deliberate, explicit, human-run CLI
invocation, not something injected into an arbitrary subprocess tree —
none of the "no TTY guarantee" reasoning in §5 applies to it.

**Generalized: `<path>` is a starting point for a recursive walk, not
necessarily a single leaf.** Reuses `discover`'s existing tree-walking
conventions (skip `.git`, optional `--max-depth`) to find every
candidate under `<path>` matching a watched name under a configured
root — `<path>` itself included, if it happens to match. Today's
existing behavior (name one specific already-matching directory, get
just that one converted) becomes the trivial special case where the
walk finds exactly one candidate — same command, same syntax, more
capable underneath.

**Also replaces cd-hook's old proactive pre-creation entirely:** if
`<path>` doesn't exist at all yet (not even as a plain directory),
`convert` just creates it directly as a fresh, empty subvolume instead
of requiring something existing to copy-and-swap — then asks the same
"remember this?" question as any other case below. No automatic,
cd-triggered background creation; pre-creating something ahead of time
is now always an explicit, deliberate `convert` invocation.

For each candidate found, **processed shallowest first**:
- Already a subvolume → nothing to do, skip silently.
- A `+` decision already exists → convert directly, no need to ask
  again (already consistent with what's happening).
- A `-` decision already exists → skip, respecting it.
- **Undecided or doesn't exist yet** → convert it (creating fresh and
  empty if it didn't exist, copy-and-swap if it already existed as
  plain), then (if `stdin` is a TTY — skip asking and record nothing
  otherwise, same "couldn't ask isn't the human said no" reasoning as
  elsewhere) ask whether to remember this,
  exactly the "3-state" idea from earlier in this design process, now
  relocated to a context where asking is actually safe instead of
  needing a whole separate subcommand:
  1. **No** — just this one-time conversion, nothing recorded (default
     if the human doesn't answer).
  2. **Yes, just this path** — appends `+ /relative/path` (anchored) to
     the project's top-level decision file.
  3. **Yes, every match of this name** — appends
     `+ /<containing-dir>/**/name` (anchored to that candidate's own
     containing directory, `**` for any depth below it — scoped to
     what was actually acted on, not a bare unanchored pattern that
     could silently cover an unrelated same-named directory elsewhere
     never actually looked at).

**Shallowest first matters for the same reason it did for the old
pending-queue design:** if choice 3 gets picked for a shallow candidate,
any deeper candidate the newly-added pattern now covers gets skipped
(not re-asked) — it was undecided a moment ago, but the answer just
given already resolves it.

**Also registers the project boundary, silently, as a side effect (§3):**
whenever any decision gets recorded above, `convert` also ensures its
own resolved project root is present in the centralized project-roots
list (idempotent — a no-op if already there), triggering a reload if
it wasn't. No extra question asked; this is what makes explicit,
proactive `register` calls optional rather than mandatory — most
projects get registered "for free" the first time anyone actually
converts something in them.

**This closes what was previously an accepted gap, not just documented
it:** since the walk descends *into* whatever `<path>` was given,
running `ghostvolumes convert /build` also discovers and asks about
`/build/build` nested beneath it, in the very same invocation — no
separate manual `discover`/`convert` pass needed afterward, as long as
`convert` gets pointed at a covering ancestor (which is exactly what
`intercept`'s notice already suggests, above).

**Overriding an existing `-` decision:** if a candidate the walk finds
already carries a `-`, the walk normally just skips it silently (see
above) — but if the human explicitly named *that exact path* as
`<path>` (not merely as something the walk happened to pass through),
that's a deliberate override attempt and should surface plainly (e.g.
"this path is marked to never be converted — continue anyway? [y/N]")
rather than silently doing nothing. Confirming the override doesn't
automatically flip the recorded decision to `+` — that's still the
separate "remember this?" step above, asked independently once the
override is confirmed.

### 7. Removed

- `shim/git_core.rs`, `src/git.rs`, and every call site (`decide()`,
  and wherever `ensure`/`convert` currently check git-tracked status).
- The `GitTracked` decision variant.
- **`ensure`/cd-hook entirely** — the shell-init snippet no longer
  installs a `cd` wrapper/`chpwd` hook, only the `LD_PRELOAD` export.
  All three of its old responsibilities (proactive pre-creation,
  repo-local-config registration, plain-directory warnings) are gone
  or superseded — see §5/§6.
- **Per-project `.ghostvolumes.toml`/`projects.d` entirely** — decision
  files are the entire per-project mechanism now (which names, which
  patterns, approve/deny). The small "known project-root paths" list
  that replaces its boundary-marking role (§3) is a new, separate,
  plain-text file (not TOML, not part of `merge.rs`'s pipeline at all).
  `roots.d` (broad, BTRFS-validated mountpoints) and `watched.d`
  (global default names) are unrelated to any of this and unchanged.
- **The "proactive" marker entirely** — `compiled.tsv`'s third column
  (`is_proactive`) and `cache_core::proactive_project_for` existed only
  for `ensure`'s proactive pre-creation, which is now gone (§5). No
  remaining consumer; `compiled.tsv` reverts to plain `(prefix, name)`
  rows, matching `cache_core::names_for`'s existing signature change.
- `discover`'s suggested output changes shape accordingly: a decision-
  file line to add (e.g. `+ /relative/path`), not a `projects.d` TOML
  block — small adjustment to what it prints, not a redesign of its
  own tree-walking logic.

## Non-goals for this pass

- Multi-VCS detection of any kind (explicitly rejected above).
- Full gitignore-spec pattern support (negation, character classes,
  mid-pattern `**`).
- A persistent "interactive session" mode beyond `intercept`'s
  per-invocation scope (no separate `ghostvolumes shell` wrapper) —
  confirmed there's no such separate concept; "interactive shim mode"
  and `intercept` were the same idea.
- A live, in-build status bar for prompting during `intercept` — moot
  now that there's no prompting UI anywhere in this design at all.
- **`--ask-now`/`--log` (rejected, not deferred):** the only real
  benefit — later occurrences of a recurring pattern within one
  *already-running* build resolving directly instead of needing
  retroactive `convert` — only applies to patterns that recur multiple
  times in a single build (e.g. monorepos). For any build where each
  pattern occurs once, it's identical in outcome to editing the
  decision file after the fact, just asked sooner — not judged worth
  the added complexity, especially now that there's no review command
  to make "sooner" even mean anything.
- **A `ghostvolumes decide`/`review` subcommand of any kind (rejected,
  not deferred; see §5):** the decision file, already expected to be
  hand-edited, serves as the entire review surface — a second,
  ghostvolumes-specific interface on top of it would be redundant.
- A separate pending-queue file (rejected; see §4/§5) — undecided
  candidates are comment lines in the decision file itself, so there's
  nothing separate to keep in sync, compact, or reason about.
- **`reload` auto-discovering project boundaries via a full filesystem
  walk (considered, then rejected; see §3):** would have made
  `register` unnecessary, but at the cost of an unbounded, expensive
  scan on every `reload` — exactly the kind of operation this design
  otherwise avoids (`discover`'s own tree-walk is deliberately an
  explicit, occasionally-run command, not something `reload` does
  automatically).
- **Routing the project-roots list through TOML + `reload`'s compile
  step, matching `roots.d`'s own pipeline (considered, then rejected;
  see §3):** that pipeline exists for `roots.d` specifically because
  BTRFS validation is genuinely expensive and needs to happen
  centrally — a registered project-root path needs no such validation
  at all. Follows decision files' own philosophy instead: a plain-text
  file, read live, no compilation, no `reload` involvement.

## Open bikeshed items (not load-bearing, decide during implementation)

- Decision file's actual name (`.ghostvolumes-subvol` is a placeholder).
- `GHOSTVOLUMES_AUTO_YES`'s final env var name.
- Exact wording/format of the pending-comment line and its (optional)
  one-time explanatory header.
- `register` subcommand's final name, and the project-roots list
  file's actual name/location (plain path-per-line, XDG data dir).

## Build order (incremental, tested, one commit per step)

1. `shim/decision_core.rs`: pattern parsing + matching, unit-tested
   standalone (no shim/CLI wiring yet).
2. `cache_core.rs`: add the `longest_matching_prefix`-style helper over
   `CACHE_ROWS` (sibling to `names_for`), unit-tested standalone — this
   is what bounds the walk-up (see §3).
3. Decision-file walk-up + resolution (closest-file-wins,
   last-line-wins, bounded by the prefix from step 2), unit-tested
   standalone — one function usable from both the CLI and the shim.
4. Project-roots file: plain-text, one path per line, read live by
   both the CLI and the shim (a `OnceLock`, same cost profile as
   `CACHE_ROWS`) — no TOML, no `reload` involvement at all (§3).
5. `ghostvolumes register <path>` (name TBD) subcommand: appends
   `<path>` to that file directly (dedup check first).
6. Wire the walk-up resolution into `decide()`, replacing `GitTracked`.
   Remove `git_core.rs`/`git.rs` and all call sites in this step.
   Also drop `compiled.tsv`'s "proactive" marker column and
   `cache_core::proactive_project_for` (dead once `ensure` is gone —
   §7) — `compiled.tsv` reverts to plain `(prefix, name)` rows.
7. Pending-comment appending on undecided (shim side): best-effort
   dedup read, then append `# /pattern` to the resolved decision file.
8. Extend `convert`: remove its git-tracked hard refusal; generalize
   `<path>` into a recursive walk (reusing `discover`'s tree-walking
   conventions), including creating a not-yet-existing `<path>` as a
   fresh empty subvolume directly (replacing cd-hook's old proactive
   creation); resolve every candidate found shallowest-first
   (already-subvolume/`+`/`-`/undecided per §6), with the
   post-conversion "remember this?" TTY-checked prompt for undecided
   ones, including the existing-`-`-decision override-confirmation
   case; also silently registers its own resolved project root (§6).
9. `ghostvolumes intercept -- <cmd>` subcommand: plain wrapper (set
   `LD_PRELOAD` for the child only, exec with inherited stdio, wait),
   plus the end-of-run notice (naming the covering `convert
   <project-root>` command) if this run appended any comment lines.
10. Remove `ensure`/cd-hook entirely, including its wiring into
    `shell-init`'s printed snippet (no more `cd` wrapper/`chpwd` hook,
    only the `LD_PRELOAD` export remains). Update `discover`'s suggested
    output to a decision-file line instead of a `projects.d` TOML block.
11. `GHOSTVOLUMES_AUTO_YES` env var support in the shim.
12. Update `design.md` and `main-plan.md` to reflect the new model in
    place of the git-tracked gate, cd-hook's removal, and the
    `.ghostvolumes.toml`/`projects.d` → decision-file consolidation.
