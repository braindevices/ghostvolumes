# `ghostvolumes convert` — guide

Registers `<path>` as a project, then walks it asking about each
undecided candidate — converting and recording a `+`/`-` decision as
it goes. `<path>` itself is never converted.

```bash
ghostvolumes convert <path> [--create <relative-path>]... [--max-depth N] [--dry-run]
```

## Example

```
$ cd ~/projects/my-app && npm install        # node_modules created as a plain directory
$ ghostvolumes convert .
Register /home/user1/projects/my-app as a project? [Y/n] y
/home/user1/projects/my-app/node_modules: convert to a subvolume? [Y/n] y
$ cat .ghostvolumes-decisions
+ node_modules
$ git add .ghostvolumes-decisions && git commit -m "Record subvolume decisions"

$ rm -rf node_modules && ghostvolumes intercept -- npm install   # from now on: automatic, no prompt
```

```
$ ghostvolumes convert . --dry-run
would create/convert: node_modules
undecided: build (skipped — dry run)
```

## Notes

- **`--create <relative-path>`** (repeatable) names a specific target directly, bypassing the watched-name check — for a one-off you don't want on the global watch list. Recording an anchored `+`/`?` decision for it is the *persisted* equivalent — it keeps being honored on future runs without needing `--create` again.
- **An existing subvolume with no decision** is asked about too, but defaults to **yes** on an empty answer (unlike every other prompt) — a hand-made subvolume is overwhelmingly likely to have been made on purpose.
- **Non-interactively** (no TTY), converts nothing and leaves a `?` marker instead, same as [`intercept`](intercept.md).
- **`--dry-run`** never prompts or touches anything — the filesystem, the decision file, and the project-roots list are all left alone.
- Skips anything matching a configured ignore pattern — see [Ignoring directories entirely](#ignoring-directories-entirely) below.
- If `<path>` isn't already a registered project, asks once upfront before touching anything — see [project-roots.md](project-roots.md) for the full registration decision tree.
- Set `GHOSTVOLUMES_DEBUG=debug` to see why each candidate resolved the way it did.
- See [decision-files.md](decision-files.md) for the `.ghostvolumes-decisions` pattern syntax `convert` reads and writes.

## Ignoring directories entirely

`convert`'s walk never even checks an ignored directory for a
watched-name match, let alone descends into it — same pattern grammar
a decision file uses (bare `name`, anchored `/name`, `/a/b/**/name`),
but no `+`/`-`/`?` prefix, since there's nothing to decide, only
whether to walk in at all.

```toml
# roots.d config
default-ignore = [".git", ".hg", ".svn", ".snapshots"]
```

```gitignore
# .ghostvolumes-ignore at a volume root or project root
node_modules/some-vendored-thing
```

| Tier | Where |
|---|---|
| Global | `default-ignore` in `roots.d` |
| Volume root | `.ghostvolumes-ignore` at a `roots.d`-configured root's own path |
| Project root | `.ghostvolumes-ignore` at a registered project's own path |

All three are unioned (matching *any* skips). Unlike decision files, a
`.ghostvolumes-ignore` file exists *only* at that one boundary
location — never walked up through every intermediate directory —
though a `**` pattern inside it can still reach arbitrary depth from
there. [`discover`](discover.md) (not tied to any one registered
project) only honors the global tier.
