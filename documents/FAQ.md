# FAQ

## What's the recommended workflow?

**Starting a brand new project, nothing built yet:**

```bash
cd ~/projects/my-app
npm install                    # node_modules is created as a plain directory
ghostvolumes convert .         # finds it, asks whether to remember the decision
```

Answering yes writes a `+`/`-` line to `.ghostvolumes-decisions` at the project root. Commit it, the same way you'd commit `.gitignore`:

```bash
git add .ghostvolumes-decisions && git commit -m "Record subvolume decisions"
```

From the next build onward, wrap it with `intercept` and matching directories convert automatically, no prompting:

```bash
rm -rf node_modules && ghostvolumes intercept -- npm install
```

**Cloning a repo that already has decisions committed:** nothing to do — `ghostvolumes intercept -- <your build command>` works from the very first build.

**Pre-authoring decisions before ever building:** hand-write `.ghostvolumes-decisions` yourself (see [decision-files.md](decision-files.md) for the pattern syntax) — `intercept` benefits immediately, same as the cloned-repo case.

## What happens if `intercept` finds something undecided?

It prints a notice after your command finishes, naming the one covering command to run:

```
ghostvolumes: new undecided path(s) found under /home/user1/projects/my-app — run `ghostvolumes convert /home/user1/projects/my-app` to review them
```

Running that `convert` resolves everything pending under that root in one pass, including anything nested (e.g. a `packages/foo/node_modules` inside a monorepo).

## How do I make sure a directory is never converted?

Write a `- name` (or `- /exact/path`) line to the decision file yourself, or answer accordingly when `convert` asks. `convert` won't silently override an existing `-` decision — pointing it directly at a denied path asks for confirmation first.

## What does `--create` do that the watched-name walk doesn't?

`--create <relative-path>` (repeatable, `convert` only) names a specific target directly, bypassing the watched-name check — useful for a one-off you don't want to add to the global watch list. An anchored `+`/`?` decision already recorded for an unwatched name, or for a name that doesn't exist on disk yet, gets resolved too on every future run — recording an anchored decision is the *persisted* equivalent of `--create`, so you only need `--create` again if you don't want it remembered.

## What happens if `convert` finds an existing subvolume with no decision?

It still asks, but defaults to **yes** on an empty answer rather than declining, unlike every other prompt in the tool — there's nothing left to convert, only a decision to record, and a hand-made subvolume is overwhelmingly likely to have been made on purpose. Run non-interactively (no TTY), it converts nothing and leaves a pending `?` marker instead, same as `intercept`.

## What does `convert --dry-run` actually print?

Exactly what a real run would do, without prompting or touching anything — the filesystem, the decision file, and the project-roots list are all left alone:

- `would register: <path>` — if it isn't already a covered project
- `would create/convert: <name>` — for each candidate a real run would materialize
- `undecided: <name> (skipped — dry run)` — for anything that would otherwise prompt
- `would ask to override the '-' decision for <name>` — for a candidate already denied

Set `GHOSTVOLUMES_DEBUG=debug` (with or without `--dry-run`) to see *why* each candidate resolved the way it did.

## What order does `decide` do things in?

`decide` is `convert`'s own walk-and-resolve engine, minus ever touching the filesystem — an existing `+` is a no-op instead of a re-materialize, and a freshly-answered "yes" only records the decision. Three things happen, in order:

1. Each `--add`/`--deny` pattern is recorded verbatim — no anchoring or broadening computed, since you're specifying the pattern directly.
2. The filesystem is walked exactly like `convert`, resolving (not converting) anything undecided that step 1 didn't already cover.
3. Any `?` pending marker still left in the project's own decision file — one whose candidate doesn't exist on disk at all, so step 2's walk could never reach it — gets resolved the same way.

A pattern that exactly matches an existing pending `?` marker (from `--add`/`--deny`, or from asking about it in steps 2/3) toggles that line in place instead of adding a second one. There's no `--create` — naming something explicitly to materialize conflicts with `decide`'s whole contract of never touching the filesystem.

## Why not just export `LD_PRELOAD` globally?

`ghostvolumes shell-init <shell>` still prints a valid `export LD_PRELOAD=...` line, but it's a diagnostic/reference tool, not something to `eval` into your rc file. Sourcing it there means every process your shell spawns inherits `LD_PRELOAD` — including every `ghostvolumes` subcommand itself (`intercept`, `convert`, `projects`, ...), not just the build you meant to wrap. That breaks `intercept`'s own invariant that the shim only ever loads into the child, never the parent, and makes `intercept` mostly redundant besides its post-run notice. See [design.md](design.md#key-decisions-and-why) for the full mechanism.

If you want whole-session coverage instead of wrapping each command individually, open a deliberate wrapped subshell:

```bash
ghostvolumes intercept -- bash   # or zsh
```

Everything inside that subshell is the "child," so the invariant holds and your outer login shell stays unaffected.

## Can I run `ghostvolumes` management commands from inside `intercept -- bash`?

No — `ghostvolumes` refuses to run at all if its own shim is already present in `LD_PRELOAD`, which is always true inside an `intercept -- bash` session. There's no legitimate workflow that needs this: `convert` is only ever meant to run before a project is wrapped, and `intercept`'s "undecided path" notice only prints after the wrapped session exits, by which point you're already back outside it. See [design.md](design.md#key-decisions-and-why) for why this has no carve-out.
