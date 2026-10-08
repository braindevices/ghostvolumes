# FAQ

## What's the recommended workflow?

1. Set up the projects subvolume, the Snapper config and the timer once: [snapshot-prune.md](snapshot-prune.md#setup).
2. For each project, record what's volatile:

```bash
ghostvolumes discover ~/src/my-app                   # suggestions, read-only
ghostvolumes decide ~/src/my-app --add node_modules  # or plain `decide` to be asked
```

3. Commit `.ghostvolumes-decisions` like `.gitignore`:

```bash
git add .ghostvolumes-decisions && git commit -m "Record volatile directories"
```

From the next hourly run on, every snapshot of `~/src` leaves those directories out.

**Cloning a repo that already has decisions committed:** nothing to do; its committed `+` lines apply from the next run (reported once in `events.log` as new), but in git repos only to directories the repo also ignores.

**Pre-authoring decisions:** hand-write `.ghostvolumes-decisions` (see [decision-files.md](decision-files.md)); the next snapshot uses them.

## How do I see what a snapshot run did?

`snapper -c src list` shows `verify=…` per snapshot (see the table in [snapshot-prune.md](snapshot-prune.md#what-each-run-does)), and `journalctl --user -u ghostvolumes-snapshot@src` shows every pruned and refused path. To preview without changing anything: `ghostvolumes prune --dry-run ~/src/.snapshots/N/snapshot`; add `--config src --since 7d` to see what decision changes within a week newly prune.

## How do I make sure a directory is never converted?

Write a `- name` (or `- /exact/path`) line to the decision file yourself, or answer accordingly when `convert` asks. `convert` won't silently override an existing `-` decision — pointing it directly at a denied path asks for confirmation first.

## What does `--create` do that the watched-name walk doesn't?

`--create <relative-path>` (repeatable, `convert` only) names a specific target directly, bypassing the watched-name check — useful for a one-off you don't want to add to the global watch list. An anchored `+`/`?` decision already recorded for an unwatched name, or for a name that doesn't exist on disk yet, gets resolved too on every future run — recording an anchored decision is the *persisted* equivalent of `--create`, so you only need `--create` again if you don't want it remembered.

## What happens if `convert` finds an existing subvolume with no decision?

It still asks, but defaults to **yes** on an empty answer rather than declining, unlike every other prompt in the tool — there's nothing left to convert, only a decision to record, and a hand-made subvolume is overwhelmingly likely to have been made on purpose. Run non-interactively (no TTY), it converts nothing and leaves a pending `?` marker instead.

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

## What happened to `intercept` and the `LD_PRELOAD` shim?

They were retired (2026-10) in favour of pruning snapshots. Injecting a library into every build process missed static binaries, IDEs and anything started outside `intercept`, and carried most of the maintenance and security cost. Pruning inside Snapper snapshots needs nothing in your processes and covers every tool. `ghostvolumes init` removes the old shim file. Unset any `LD_PRELOAD` you exported from `shell-init`. The design history is in [design.md](design.md).
