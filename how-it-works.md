# How it works — guide

The core loop: a directory gets a `+`/`-` decision recorded once, then
every future build reuses that decision automatically, with no prompt
and no guessing.

```
ghostvolumes convert <path>              # first time: ask, record a decision
ghostvolumes intercept -- <build cmd>    # every time after: apply recorded decisions, no prompt
```

## `ghostvolumes intercept -- <cmd>`

Runs `<cmd>` with the shim active for that command only, via
`LD_PRELOAD` scoped to the child process. It intercepts `mkdir`/
`mkdirat` and converts a directory into a subvolume — but only if a
`+` decision is already recorded for it. It never prompts (it can run
inside arbitrary subprocess trees with no guaranteed terminal), so an
undecided directory is left alone and a `?`-prefixed pending marker is
appended to the decision file for later review.

## `ghostvolumes convert <path> [--create <relative-path>]... [--max-depth N] [--dry-run]`

`<path>` is the project (a decision-file/project-roots boundary) —
it's never itself converted. If it isn't already covered by a
registered project, `convert` asks once upfront before touching
anything (see [Projects can't nest](#projects-cant-nest)) — declining
aborts the whole command.

Every directory under `<path>` that's either a watched name or already
a real subvolume gets resolved automatically, skipping anything
matching a configured ignore pattern (see [Ignoring directories
entirely](#ignoring-directories-entirely)). `--create <relative-path>`
(repeatable) additionally names a specific target directly, bypassing
the watched-name check — useful for a one-off you don't want on the
global watch list. An anchored `+`/`?` decision already recorded for
an unwatched name, or for a name that doesn't exist on disk yet, gets
resolved too — recording an anchored decision is the *persisted*
equivalent of `--create`.

For each candidate: "yes"/"all" converts (creating fresh or migrating
in place) and records `+`; "no" converts nothing and records `-`. An
already-a-subvolume candidate with no recorded decision is asked about
too, but defaults to **yes** on an empty answer (unlike every other
prompt) — there's nothing left to convert, only a decision to record,
and a hand-made subvolume is overwhelmingly likely to have been made
on purpose. Non-interactively (no TTY), it converts nothing and leaves
a `?` marker instead, same as `intercept`.

`--dry-run` prints what a real run would do, without prompting or
touching anything:

```
would register: <path>
would create/convert: <name>
undecided: <name> (skipped — dry run)
would ask to override the '-' decision for <name>
```

Set `GHOSTVOLUMES_DEBUG=debug` to see why each candidate resolved the
way it did.

## `ghostvolumes decide <path> [--max-depth N] [--add <pattern>]... [--deny <pattern>]...`

`convert`'s own walk-and-resolve engine, minus ever touching the
filesystem — an existing `+` is a no-op instead of a re-materialize,
and a freshly-answered "yes" only records the decision. No `--create`
(naming something to materialize conflicts with `decide`'s whole
contract). Three things happen, in order:

1. Each `--add`/`--deny` pattern is recorded verbatim — no anchoring
   or broadening computed, since you're specifying the pattern
   directly.
2. The filesystem is walked exactly like `convert`, resolving (not
   converting) anything undecided that step 1 didn't already cover.
3. Any `?` pending marker still left in the project's own decision
   file — one whose candidate doesn't exist on disk at all, so step
   2's walk could never reach it — gets resolved the same way.

A pattern that exactly matches an existing pending `?` marker (from
`--add`/`--deny`, or from asking about it in steps 2/3) toggles that
line in place instead of adding a second one.

## Decision files

Per-project, committed to the repo they live in — one
`.ghostvolumes-decisions` file per directory, gitignore-style:

```gitignore
# .ghostvolumes-decisions at a project root
+ node_modules                 # matches this name at any depth
+ /dist                        # anchored: this exact location only
+ /packages/*/**/node_modules  # anchored prefix, arbitrary depth after it
- vendor                       # never convert, at any depth
? /build/should-review-this    # pending: noted, not yet a decision
# a real comment, for humans only - never touched by any of the above
```

| Pattern | Meaning |
|---|---|
| `name` | Any depth under this file's directory, by final path component |
| `/name` | Anchored: exact location only |
| `/a/b/**/name` | Anchored prefix, arbitrary depth after it |

Resolution walking up from a candidate: the closest enclosing file
with a matching pattern wins; within one file, the last matching line
wins. `#` is the one prefix reserved for humans — nothing in this tool
ever writes or rewrites a `#` line. A later real decision for the
*same* pattern replaces a `?` line in place rather than leaving both
around.

## Ignoring directories entirely

`convert`'s and `discover`'s walks never even check an ignored
directory for a watched-name match, let alone descend into it — same
pattern grammar a decision file uses, but no `+`/`-`/`?` prefix, since
there's nothing to decide here, only whether to walk in at all. Three
tiers, unioned (matching *any* skips):

| Tier | Where |
|---|---|
| Global | `default-ignore` in `roots.d` |
| Volume root | `.ghostvolumes-ignore` at a `roots.d`-configured root's own path |
| Project root | `.ghostvolumes-ignore` at a registered project's own path |

Unlike decision files, a `.ghostvolumes-ignore` file exists *only* at
that one boundary location — it's never walked up through every
intermediate directory — though a `**` pattern inside it can still
reach arbitrary depth from there, the same way a single `.gitignore`
at a repo root reaches deep paths. `discover` (not tied to any one
registered project) only honors the global tier.

## Projects can't nest

The project-roots list (`project-roots.list` under the XDG data
directory, one path per line) tells the shim where to stop walking up
when resolving decisions. `convert` registers this automatically;
`ghostvolumes projects register <path>` sets it up by hand ahead of
time if needed.

At most one registered project can ever cover a given path — decision
(and ignore) files already self-distribute via their own
closest-file-wins walk-up, so a hierarchy of registered projects
wouldn't buy anything beyond a single, correct stopping boundary. Two
projects that are path-ancestor/descendant of each other but sit on
*different* BTRFS volumes are treated as unrelated, not nested. Before
registering a new project, `convert`/`decide` check, in order:

- Already covered by an existing, same-volume project? No-op — that
  project's decisions already apply.
- Would registering it *nest over* an already-registered, same-volume
  descendant project? Warns and asks whether to unregister the
  descendant(s) and register the new, broader project instead
  (default: no).
- A decision file exists at some ancestor with nothing registered
  covering it (a parent registration possibly forgotten)? Warns and
  asks whether to continue and register the narrower path anyway
  (default: no).
- Otherwise, the usual "Register `<path>` as a project? [Y/n]" ask.

A missing TTY at any of these aborts rather than guessing.

This is genuine, persistent user data (unlike the disposable,
regenerate-anytime `compiled.tsv`), so backing it up or syncing it
across machines (a dotfile manager, a disk migration) is fine. Just
don't hand-edit it directly — use `ghostvolumes projects
register`/`unregister` instead, so a live edit never races the shim's
or CLI's own reads and writes of it. Run `ghostvolumes projects
unregister` (no path) any time to interactively prune entries that no
longer exist, including ones that arrived already-stale via a
synced/copied-in list. `ghostvolumes projects list` shows what's
currently registered.
