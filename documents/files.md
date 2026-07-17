# Files — guide

Every path GhostVolumes reads or writes, in one place.

## `~/.config/ghostvolumes/` (XDG config dir)

```
~/.config/ghostvolumes/
├── roots.d/
│   ├── 00-auto.toml         # written by `roots scan --save` — regenerated every run, never hand-edit
│   ├── 00-defaults.toml     # written once by `init` if missing, never overwritten after — see below
│   ├── 10-disable.toml      # written by `roots disable`/`enable` — only ever lists disabled roots
│   ├── roots-disable.lock   # guards that read-modify-write, never deleted (same as the locks below)
│   └── 10-local.toml        # yours: any *.toml file works, this is just the documented convention name
```

Every `*.toml` directly under `roots.d/` is merged, sorted by
filename, last file wins per field — the loader doesn't care what a
file is named, `10-local.toml` is convention, not enforced. The real
shipped `00-defaults.toml`:

```toml
default-watches = ["node_modules", "target", ".venv", "build", ".cache", ".uv-cache", ".ruff_cache", ".pytest_cache"]
default-ignore = [".git", ".hg", ".svn", ".snapshots"]
```

## `~/.local/share/ghostvolumes/` (XDG data dir)

```
~/.local/share/ghostvolumes/
├── compiled.tsv                              # derived cache: (root, watched-name) pairs. Written by
│                                              # `reload` (also runs at the end of `roots scan --save`);
│                                              # the shim's only source of truth — never hand-edit.
├── libghostvolumes_shim.so                   # the compiled LD_PRELOAD shim, extracted by `init`
├── locks/                                    # advisory flocks — files are markers, never deleted;
│   │                                         # seeing them persist after normal use is expected.
│   ├── %2Froot%2Ftest.lock                   # one per project/root boundary (path percent-encoded,
│   ├── %2Froot%2Ftest%2Fproject-tracked.lock # / -> %2F) — guards the subvolume create/copy/rename
│   └── decisions/                            # separate namespace: guards a project's own
│       ├── %2Froot%2Fdrift-test.lock         # .ghostvolumes-decisions read-modify-write instead —
│       └── %2Froot%2Ftest%2Fproject-tracked.lock  # doesn't block on, or get blocked by, the locks above
├── project-roots.list                        # the registered project list — see project-roots.md
├── project-roots.lock                        # guards register/unregister edits to the list above
├── reload.lock                                # guards the reload sequence that produces compiled.tsv
└── shim.log                                   # the shim's own log — the CLI never writes here
```

## Per-project files

Live inside your own repos, committed alongside your code:

```
~/projects/my-app/
├── .ghostvolumes-decisions   # +/-/? records — see decision-files.md
├── .ghostvolumes-ignore      # optional: never-walk-into patterns for this project root
├── node_modules/             # + decided -> a real BTRFS subvolume
└── package.json
```

```gitignore
# .ghostvolumes-ignore at a volume root or project root
node_modules/some-vendored-thing
```

Same pattern grammar as a decision file, no `+`/`-`/`?` prefix — see
[convert.md](convert.md#ignoring-directories-entirely) for the three
tiers (`default-ignore` global, volume root, project root) and how
they combine.
