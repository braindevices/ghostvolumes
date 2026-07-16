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

**Pre-authoring decisions before ever building:** hand-write `.ghostvolumes-decisions` yourself (see the README's [Decision files](README.md#decision-files) section for the pattern syntax) — `intercept` benefits immediately, same as the cloned-repo case.

## What happens if `intercept` finds something undecided?

It prints a notice after your command finishes, naming the one covering command to run:

```
ghostvolumes: new undecided path(s) found under /home/user1/projects/my-app — run `ghostvolumes convert /home/user1/projects/my-app` to review them
```

Running that `convert` resolves everything pending under that root in one pass, including anything nested (e.g. a `packages/foo/node_modules` inside a monorepo).

## How do I make sure a directory is never converted?

Write a `- name` (or `- /exact/path`) line to the decision file yourself, or answer accordingly when `convert` asks. `convert` won't silently override an existing `-` decision — pointing it directly at a denied path asks for confirmation first.

## Why not just export `LD_PRELOAD` globally?

`ghostvolumes shell-init <shell>` still prints a valid `export LD_PRELOAD=...` line, but it's a diagnostic/reference tool, not something to `eval` into your rc file. Sourcing it there means every process your shell spawns inherits `LD_PRELOAD` — including every `ghostvolumes` subcommand itself (`intercept`, `convert`, `register`, ...), not just the build you meant to wrap. That breaks `intercept`'s own invariant that the shim only ever loads into the child, never the parent, and makes `intercept` mostly redundant besides its post-run notice. See [design.md](design.md#key-decisions-and-why) for the full mechanism.

If you want whole-session coverage instead of wrapping each command individually, open a deliberate wrapped subshell:

```bash
ghostvolumes intercept -- bash   # or zsh
```

Everything inside that subshell is the "child," so the invariant holds and your outer login shell stays unaffected.

## Can I run `ghostvolumes` management commands from inside `intercept -- bash`?

No — `ghostvolumes` refuses to run at all if its own shim is already present in `LD_PRELOAD`, which is always true inside an `intercept -- bash` session. There's no legitimate workflow that needs this: `convert` is only ever meant to run before a project is wrapped, and `intercept`'s "undecided path" notice only prints after the wrapped session exits, by which point you're already back outside it. See [design.md](design.md#key-decisions-and-why) for why this has no carve-out.
