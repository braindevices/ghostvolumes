# `ghostvolumes decide` — guide

`convert`'s own walk-and-resolve engine, minus ever touching the
filesystem — it only ever records a decision, never converts.

```bash
ghostvolumes decide <path> [--max-depth N] [--add <pattern>]... [--deny <pattern>]...
```

## Example

```
$ ghostvolumes decide ~/projects/monorepo --add packages/foo/node_modules --deny vendor
$ cat ~/projects/monorepo/.ghostvolumes-decisions
+ packages/foo/node_modules
- vendor

/home/user1/projects/monorepo/build: convert to a subvolume? [y/N]
```

## Notes

- No `--create` — naming something to materialize conflicts with `decide`'s whole contract of never touching the filesystem.
- An existing `+` is a no-op instead of a re-materialize; a freshly-answered "yes" only records the decision.
- A pattern that exactly matches an existing pending `?` marker toggles that line in place instead of adding a second one.
- See [decision-files.md](decision-files.md) for the pattern syntax `--add`/`--deny` write, and [project-roots.md](project-roots.md) for the registration rules `decide` shares with `convert`.

## Order of operations

1. Each `--add`/`--deny` pattern is recorded verbatim — no anchoring or broadening computed, since you're specifying the pattern directly.
2. The filesystem is walked exactly like `convert`, resolving (not converting) anything undecided that step 1 didn't already cover.
3. Any `?` pending marker still left in the project's own decision file — one whose candidate doesn't exist on disk at all, so step 2's walk could never reach it — gets resolved the same way.
