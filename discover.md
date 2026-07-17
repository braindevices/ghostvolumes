# `ghostvolumes discover` — guide

`discover` is a read-only survey of an arbitrary starting path (`$HOME`
by default) — unlike `convert`/`decide`, it needs no project
registration and never writes anything itself. Its whole job is to
point you at the right `decide`/`convert` command to run yourself.

```
ghostvolumes discover [path] [--max-depth N] [--root-is-project]
                       [--no-project <path>]... [--ignore <path>]...
```

## What gets reported

Nothing already covered by a `+`/`-` anywhere up to `path` is reported
at all — that's the baseline that keeps the output meaningful instead
of permanently re-suggesting things that are already fully decided.

**Three kinds of genuinely undecided match**, in descending order of confidence:

| Kind | Meaning | Suggests |
|---|---|---|
| Approved candidate | A watched name that's already a subvolume | `ghostvolumes decide <dir> --add <name>` |
| Unwatched subvolume | Already a subvolume, unwatched name | Both `--add <name>` and `--deny <name>` — discover can't ask interactively to default to yes the way `convert`/`decide` can |
| Not yet converted | A watched name that's still a plain directory | Nothing — report-only. `-` would misrepresent "nobody decided" as "a human declined" |

**Two drift kinds**, for a recorded decision that disagrees with the filesystem:

| Kind | Meaning | Suggests |
|---|---|---|
| `DRIFT` (denied but exists) | Recorded `-`, but it's a subvolume anyway | The override command to record `+` instead |
| Approved, not converted | Recorded `+`, but still plain | `ghostvolumes convert <path>` to materialize it |

Only on-disk mismatches are covered — a `+`/`?` decision recorded for a
path that doesn't exist on disk at all isn't detected.

## Nested suggestions get merged

Two suggested groups in the same lineage would, if both were
registered as separate projects, violate ["projects can't
nest"](how-it-works.md#projects-cant-nest). Instead of proposing both,
`discover` folds the deeper one into its shallowest ancestor — each
folded-in name becomes an anchored pattern relative to that ancestor
(e.g. `/bb/cc/build`), so the single resulting command still targets
the exact original location:

```
ghostvolumes decide /projects/app --add build --add /packages/foo/node_modules
```

## Controlling the merge and the walk

- **`--root-is-project`** — `path` itself is never used as a merge
  target unless you pass this. `path` is normally an arbitrary, broad
  directory being surveyed (`$HOME`, a workspace folder), not itself a
  project — an unrelated finding directly inside it (a stray
  subvolume, say) must not silently absorb every other suggestion
  found anywhere underneath. Pass this when you're deliberately
  running `discover` on a directory you already consider one project.
- **`--no-project <path>`** (repeatable) — the same exclusion, but for
  a known-not-a-project container found *below* `path`, not `path`
  itself (e.g. a workspace folder holding many unrelated repos),
  regardless of `--root-is-project`. It still gets reported on its own
  if it has a direct finding — it just never absorbs anything nested
  under it.
- **`--ignore <path>`** (repeatable) — stronger still: an exact
  absolute path never scanned at all, no report and no descent, for a
  known-noisy directory (a huge dependency cache, an editor's
  extension folder) you don't want walked in the first place. Unlike
  `--no-project`, its own contents are never even looked at.
- **`--max-depth`** defaults to `3` (unlike `convert`/`decide`, which
  default to unlimited), since `discover` walks an arbitrary,
  unregistered path rather than one already-scoped project. Depth
  counts directory levels recursed *into* to reach the current
  directory — pass a larger value explicitly for a deeper tree.

## Example

```
$ ghostvolumes discover ~/ --max-depth 9 --ignore ~/.cache --no-project ~/workspace
/root/
  watched names present but not yet converted (informational only): .cache

/root/workspace/some-repo
  already a subvolume, needs a decision:
    ghostvolumes decide /root/workspace/some-repo --add /nested/build --add build
```

`~/` reports its own direct finding (informational only, no command —
nothing has actually been decided there); `~/workspace` was named via
`--no-project` so it never absorbs `some-repo`'s finding, and
`some-repo` correctly stands on its own with its own nested `build`
folded in.
