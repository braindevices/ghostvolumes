# Fold `watched.d` into `roots.d`, add per-root watch-list overrides and root disable

Supersedes `decision-model.plan.md §7`'s "`roots.d` (broad, BTRFS-validated
mountpoints) and `watched.d` (global default names) are unrelated to any of
this and unchanged" — they turn out not to be unrelated once per-root
customization is needed. Not yet a public-facing tool with outside users
depending on the config format, so this is a breaking config-schema change
with no migration path — accepted explicitly, same posture
`decision-model.plan.md` itself took.

## Problem with the current design

`watched.d/*.toml` is a single global `names` list, unioned across files.
`cache::compile` cross-products every `roots.d` root with that one global
list into `compiled.tsv`. Two consequences, surfaced by trying to give one
specific project root (`subvolumize-home`) a different watch list than
everything else:

1. **No per-root watch list is possible at all.** A project's
   `.ghostvolumes-decisions` file can only narrow (`- name`, opt a
   globally-watched name out) or approve (`+`) — both the CLI
   (`convert.rs`'s `find_nested_candidates_inner`) and the shim
   (`preload.rs:297`'s `decide()`) gate candidate-detection on
   `cache::names_for`, which only ever contains globally-watched names.
   Decision files never get a chance to run on a name that isn't globally
   watched in the first place.
2. **No way to disable a root.** `roots.d/*.toml` merge is a pure set
   union — nothing can subtract a root `scan --save` keeps re-detecting
   into `00-auto.toml`.

## Design

Single `roots.d/` directory. Each `*.toml` file may set:

```toml
default-watches = ["node_modules", "target", ".venv", "build"]

["/home/dracula/git/braindevices/subvolumize-home"]
watches = ["node_modules", "dist"]

["/some/noisy/mount"]
enabled = false
```

- `default-watches`: the fallback watch list for any root with no
  `watches` override of its own.
- A root path key, table value: `enabled` (bool, default `true`) and
  `watches` (optional list, replaces — does not union with —
  `default-watches` when present).

**Merge rule across files, one rule for everything: last file wins per
field.** No unions anywhere in this schema (a deliberate simplification
over today's mixed union/cross-product rules) — files are processed in
existing lexical order (`00-*` before `10-*`), and for both `default-watches`
and each root's `enabled`/`watches`, a later file's explicitly-set field
value replaces the earlier one; fields a later file doesn't mention are left
as whatever an earlier file set (or the built-in default: `enabled = true`,
`watches` unset). This requires the *parsed* (pre-merge) struct shape to
distinguish "field absent in this file" from "field present with a default
value" (`Option<T>`, no `#[serde(default)]` coercion at parse time) —
defaults only get applied once, at final resolution after every file is
merged.

A root with `enabled = false` (from any file, including one loaded after
the file that first defined that root) is dropped entirely from
`MergedConfig` and never reaches `compiled.tsv`.

**No cascade between nested roots.** Each root path is an independent,
flatly-keyed entry — disabling `/` does not disable `/home` or
`/home/backups` even though both sit under it. `cache_core::names_for`
already unions every matching-ancestor row for a given path (existing
behavior, unchanged), so today a path under `/home/backups` gets `/`'s
watches, `/home`'s, and `/home/backups`'s, all unioned together;
disabling `/` just removes `/`'s own contribution from that union —
`/home`'s and `/home/backups`'s own entries keep contributing exactly as
before, for any path still under them. Confirmed as the desired behavior,
not just the path of least implementation resistance.

`MergedConfig` changes shape from `{ roots: Vec<String>, global_defaults:
Vec<String> }` to something like `{ roots: Vec<ResolvedRoot> }` where
`ResolvedRoot { path: String, watches: Vec<String> }` — already
enabled-filtered and default-vs-override-resolved, so `cache::compile`
just iterates resolved per-root lists instead of cross-producting two
separate flat lists. `compiled.tsv`'s own `(prefix, name)` row format is
unchanged — this only touches how those rows get produced, not how the
shim or `cache_core::names_for` reads them.

## Steps

1. `src/config.rs`: replace `RootsFile`/`WatchedFile` with the new
   flatten-based shape (`default_watches: Option<Vec<String>>`,
   `#[serde(flatten)] roots: BTreeMap<String, RawRootEntry>` where
   `RawRootEntry { enabled: Option<bool>, watches: Option<Vec<String>> }`).
   Remove `parse_watched`. Unit tests: round-trips the example above,
   rejects a malformed per-root table value, distinguishes "field absent"
   from "field explicitly set to the default."
2. `src/merge.rs`: rewrite `load_roots_dir`/`load_all` to layer files in
   sorted order per the last-file-wins rule above, then resolve into
   `Vec<ResolvedRoot>` (drop disabled, apply `watches.unwrap_or(default_watches)`).
   Remove `load_watched_dir`. Unit tests: per-root `watches` replaces (not
   unions with) `default-watches`; a later file's bare `default-watches`
   replaces an earlier file's entirely; a later file's `enabled = false`
   suppresses a root an earlier file defined; a root untouched by any
   later file keeps its earlier-file fields.
3. `src/cache.rs`: `compile()` iterates `config.roots` (now
   `Vec<ResolvedRoot>`) directly, one row per `(root.path, name)` for
   `name in root.watches` — no more explicit cross-product loop over two
   separate lists. Update existing tests to the new `MergedConfig` shape.
4. `src/scan.rs`: `save_roots()` writes bare root-path table entries (no
   `enabled`/`watches`) into `roots.d/00-auto.toml`, same
   full-overwrite-only-this-file behavior as today (verified by the
   existing "must not touch 10-local.toml" test, ported to the new shape).
5. `src/init.rs`: writes `roots.d/00-defaults.toml` with `default-watches =
   [...]` instead of `watched.d/00-defaults.toml` with `names = [...]`.
6. `src/filenames.rs`: remove `WATCHED_D_DIR`, `DEFAULT_WATCHED_FILE_NAME`;
   keep `ROOTS_D_DIR`/`AUTO_ROOTS_FILE_NAME`; rename
   `DEFAULT_WATCHED_FILE_NAME` → `DEFAULT_WATCHES_FILE_NAME` (still
   `"00-defaults.toml"`, now under `ROOTS_D_DIR`).
7. `README.md`: rewrite the Configuration section — one `roots.d/`
   directory, three example files (`00-auto.toml`, `00-defaults.toml`,
   `10-local.toml`), drop all `watched.d` mentions including in the
   `reload` command's one-line description.
8. Full-repo grep for any other `watched.d`/`WatchedFile`/`global_defaults`
   reference (`design.md`, doc comments) and update or note as superseded.
9. `cargo fmt` + `cargo clippy --all-targets` + full `cargo test` clean,
   `CHANGELOG.md` entry, version bump per the branch-suffix convention
   already in place.

## Explicitly out of scope

- The separate `convert <path>` issue (converting the literal `<path>`
  argument itself, even when it's a populated project root rather than a
  build-artifact name) raised in the same conversation — untouched here,
  a candidate for its own plan if pursued.
