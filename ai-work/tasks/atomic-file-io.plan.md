# Cross-process atomic file I/O, plus a proper project-roots lifecycle

Closes several cross-process races found by auditing every file
write/append site in `src/` and `shim/`, and — along the way — adds the
missing "remove a project root" and "list project roots" operations
that the previous design never had a place for. Not yet released, so
renaming `project-roots.txt` and restructuring the `register` command
are breaking changes with no migration concern, same posture as
`decision-model.plan.md`.

## Problem

Auditing every `std::fs::write`/`OpenOptions::append` call site turned
up three distinct risk classes:

1. **Concurrent-writer corruption in `atomic_write.rs`.** `reload()`
   (writing `compiled.tsv`) and `scan::save_roots()` (writing
   `roots.d/00-auto.toml`) both go through `write_atomically()`, which
   uses a *fixed* temp filename (`.{name}.tmp`) in the target
   directory. Two concurrent writers to the same destination share that
   temp path — the second writer's `std::fs::write()` (open + truncate)
   can land while the first is still writing, corrupting the temp
   file's content before either renames. This is not "last write wins,"
   it's a genuinely corrupted result.
2. **Interleaved-line corruption on every append-based writer.**
   `register::register()`, `convert::append_decision()`, and
   `main.rs`'s `Discover --save` loop all call `writeln!(file, "...")`
   on an `O_APPEND`-opened file. `writeln!` with a non-trivial format
   string is *multiple* `write()` syscalls (one per literal/argument
   piece), not one — `O_APPEND` only guarantees each individual `write()`
   call is atomically appended, not the whole logical line. A
   concurrent second appender's line can land between two pieces of the
   first, merging or splitting lines. (The shim's own
   `append_pending_comment()` already avoids this — it pre-formats the
   whole line into one `String` before a single `write_all()` call —
   this plan just applies that same pattern everywhere else.) The
   shim's own log line (`log_line()`) has the identical bug via a
   multi-argument `writeln!`.
3. **A real directory-swap race between the shim and `convert`.**
   `convert::copy_and_swap()` does `create tmp subvolume` → `cp -a
   --reflink=always` → `rename(path, backup)` → `rename(tmp, path)` →
   `remove_dir_all(backup)`. If a build is running under `intercept`
   and writing into that same directory while this runs, anything with
   an open fd/cwd inside the *old* directory keeps writing to that
   inode after the first `rename` — those writes silently end up in
   `backup`, which gets `remove_dir_all`'d at the end. Real, silent
   data loss, not just a cosmetic race. The shim's own
   `try_create_subvolume()` (called from `intercept`) needs to
   coordinate with this, not just `convert` internally.

Separately: `project-roots.txt` never had a supported way to *remove*
an entry — the only way to shrink it today is hand-editing the file
directly, which reintroduces exactly the half-baked-read risk this
plan closes for `compiled.tsv`. And there's no way to see what's
registered at all short of `cat`-ing the file yourself.

## Non-goals (explicit)

- **Decision-file hand-editing.** Still completely unprotected, and
  deliberately so — that's the whole point of the format (§ design.md).
  This plan only closes races between *ghostvolumes' own* writers
  (the shim vs. `convert`), never claims to protect against a human
  editing a decision file at the wrong moment.
- **A `project-roots.d`/TOML-fragment compile pipeline.**
  Considered and rejected: `project-roots.list` is fundamentally a flat
  list (built almost entirely by `register`'s own side effects), not a
  merge of independently-authored config fragments the way
  `roots.d`/`watched.d` genuinely are. Reusing `write_atomically` for
  every mutation gets the same atomicity guarantee without inventing a
  second compiled-artifact layer.

## Design

### 1. `atomic_write.rs` — unique temp names + a `reload.lock`

Temp filename becomes `.{name}.{pid}.{counter}.tmp` — `std::process::id()`
plus a small per-process `AtomicU64` counter (covers the rare case of
one process calling `write_atomically` on the same destination twice
before the first rename lands). Eliminates the corruption case
outright: worst case with unique names is an ordinary "last valid
rename wins," never a torn temp file.

On top of that, `reload()` (and therefore `scan --save`, which calls it
internally) takes a blocking exclusive lock on a fixed
`<data_dir>/reload.lock` for its whole read-merge-validate-write
sequence, fully serializing concurrent `reload`/`scan --save` runs
rather than just avoiding byte-level corruption.

### 2. A new shared `lock_core.rs` — no new dependency

`std::fs::File::lock()` / `try_lock()` / `unlock()` are stable as of
Rust 1.89 (confirmed on this toolchain, 1.96.1) — pure `std`, no
`extern "C"` declarations needed, so (unlike almost everything else
locking-related) this is usable directly from the dependency-free shim
too. Dropping the `File` releases the lock automatically; no explicit
`unlock()` needed in the common path.

New file `shim/lock_core.rs` (`mod`-included into the shim,
`include!()`-included into `src/lock.rs` for the CLI, same convention
as every other shared file):

```rust
/// Opens (creating if needed) the lock file at `path`, creating its
/// parent directory too. Callers then call `.lock()` (blocking) or
/// `.try_lock()` (non-blocking) on the returned handle themselves —
/// dropping it releases the lock.
pub fn open_lock_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new().create(true).write(true).open(path)
}

/// Percent-encodes `/` and `%` so a boundary's absolute path becomes a
/// single flat, human-inspectable filename (e.g.
/// /home/user1/app -> %2Fhome%2Fuser1%2Fapp.lock) — deliberately not a
/// hash, so a lock file on disk is still legible during debugging, and
/// so there's no risk of the shim and CLI ever computing different
/// hashes if their toolchains' hash algorithm ever drifted.
pub fn boundary_lock_path(locks_dir: &std::path::Path, boundary: &std::path::Path) -> std::path::PathBuf {
    let mut name = String::new();
    for ch in boundary.to_string_lossy().chars() {
        match ch {
            '/' => name.push_str("%2F"),
            '%' => name.push_str("%25"),
            other => name.push(other),
        }
    }
    name.push_str(".lock");
    locks_dir.join(name)
}
```

New shim-shared constant in `shim/filenames_core.rs`: `LOCKS_DIR:
&str = "locks"`. New CLI-only constant in `src/filenames.rs`:
`RELOAD_LOCK_FILE_NAME: &str = "reload.lock"` and
`PROJECT_ROOTS_LOCK_FILE_NAME: &str = "project-roots.lock"`.

### 3. Single-`write_all()` fix for every append-based writer

`register()`'s append, `convert::append_decision()`, `main.rs`'s
`Discover --save` loop, and the shim's `log_line()` all switch from
multi-piece `writeln!(file, "...")` to building the full line into one
`String` first, then one `write_all(line.as_bytes())` call — the exact
pattern `append_pending_comment()` already uses correctly. No lock
needed for this: one `write()` syscall in `O_APPEND` mode is
kernel-atomic per line on Linux, which is all a pure append needs.

### 4. `project-roots.txt` → `project-roots.list`, not `compiled.tsv`-disposable

Renamed (constant value change in `shim/filenames_core.rs`'s
`PROJECT_ROOTS_FILE_NAME`) to signal a `.txt`-invites-editing name is
wrong for it — but the reasoning is *not* "this is a disposable
compiled artifact like `compiled.tsv`." It's the opposite: unlike
`compiled.tsv` (purely derived from `roots.d`/`watched.d`, trivially
regenerated by `reload`, safe to delete), `project-roots.list` is
genuine, persistent user data with no other source of truth — losing
it means re-registering every project by hand. It already lives in the
semantically correct place for that: `XDG_DATA_HOME` is specifically
for persistent user data that's neither disposable cache nor
config-file-format config, which is exactly what this is.

The actual risk this rename+atomicity work closes is a *live* editor
save racing a concurrently-running `ghostvolumes`/shim process (a
non-atomic in-place truncate+rewrite mid-read) — not "this file has no
standalone value." A **whole-file deploy when nothing is running** —
restoring it from a backup, a disk migration, or a dotfile manager like
chezmoi managing it as a tracked, synced file across machines — is
fine and expected; that's not a live edit, it's the same as replacing
any other config file at rest. `design.md`/`README.md` should say this
plainly: mutate it live via `ghostvolumes projects register`/
`unregister`, but persisting/syncing the file itself as a whole is a
legitimate, supported way to carry registrations across machines.
Entries that arrive already-stale this way (a path that existed on the
old machine/disk but not this one) are exactly what `unregister`'s
auto-scan-and-prune mode (§5) is for — the same mechanism that handles
locally-deleted projects handles migrated-in-stale ones for free.

With this plan's changes, every *live* write to it (append or removal)
goes through an already-atomic mechanism, so no reader (the shim's
one-time-per-process read, or `convert`'s own dedup read) can ever
observe a half-baked file from `ghostvolumes`' own writes — a
non-atomic write from some *other* tool while `ghostvolumes` is
actively running is the one residual risk, same as it would be for any
externally-managed file, and out of scope for the same reason
hand-edited decision files are.

### 5. `ghostvolumes projects` — nested subcommand group

Restructures the CLI (breaking, no migration — not yet released):

```rust
enum Command {
    // ... unchanged ...
    /// Manage the registered project-roots list
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },
    // Register { path: String } removed from the top level.
}

enum ProjectsAction {
    /// List every registered project root, flagging any that no longer exist
    List,
    /// Register a project-root path for a narrower decision-file walk-up boundary
    Register { path: String },
    /// Remove a project root. With no path: scan every entry and interactively
    /// offer to prune ones that no longer exist on disk.
    Unregister { path: Option<String> },
}
```

Backing module: rename `src/register.rs` → `src/projects.rs` (matches
the new `Command::Projects` naming, same convention as
`discover.rs`/`Command::Discover`). Hosts three functions:

- `register(list_path, path)` — unchanged logic, just relocated;
  append path gets the Tier-3 single-`write_all()` fix and now takes
  `project-roots.lock` (see below) before appending.
- `unregister(list_path, path: Option<&str>, is_tty, read_line) -> anyhow::Result<()>`:
  - `Some(path)`: read the list, filter out the exact match, write the
    result back via `write_atomically` (not append) — a clean,
    idempotent removal.
  - `None` (auto mode): read the list; for each entry where
    `!Path::new(entry).is_dir()`, prompt (mirroring
    `convert.rs`'s existing `confirm_override`/`ask_remember`
    shape exactly — `eprint!` to stderr, injectable `is_tty`/`read_line`
    for testability, defaults to **not removing** on a non-TTY or empty
    answer, same safe-by-default posture as the rest of the codebase):
    `"<path> no longer exists — remove it from the project-roots list? [y/N]: "`.
    Collects every confirmed removal, then one `write_atomically` call
    with all of them filtered out.
  - Both branches acquire `project-roots.lock` (blocking) for their
    whole read-filter-write sequence — this is the one place a lock is
    needed even though the underlying writes are individually atomic:
    without it, a concurrent `register` append landing between
    `unregister`'s read and its `write_atomically` call would be
    silently dropped by `unregister`'s stale-snapshot rewrite (a
    lost-update race, not a corruption one, but just as real).
    `register()`'s own append also takes this lock, even though its
    single-`write_all()` append is already safe against *other
    appends* — it isn't safe against being invisibly overwritten by a
    concurrent `unregister` rewrite that started before it landed.
- `list_projects(list_path) -> Vec<(String, bool)>` (path, still-exists) —
  read-only, no lock needed. `main.rs`'s `ProjectsAction::List` arm
  prints one line per entry, appending `" (missing)"` for any where
  `is_dir()` is false, nothing extra for the rest.

`main.rs` updates: `mod register;` → `mod projects;`; the
`Command::Register` arm is replaced by a `Command::Projects` arm
dispatching on `action`. `convert.rs`'s internal auto-registration
(`register_project_root` → `register::register`) becomes
`projects::register` — same call, just the module rename, since that's
an internal function call, not a CLI dispatch path.

### 6. The shim-vs-`convert` directory-swap lock (per-project-boundary)

- **`Decision` enum change** (`shim/preload.rs`): `Decision::Accept`
  becomes `Decision::Accept(PathBuf)`, carrying the same `boundary`
  that's already computed once in `decide()` right before the match
  (today only `Decision::Undecided(boundary)` carries it) — needed so
  `try_create_subvolume` knows which lock file to use.
- **Shim side** (`try_create_subvolume(target: &Path, boundary: &Path) -> CreateResult`):
  resolves `data_dir` (already has `resolved_data_dir()` for this),
  computes `lock_core::boundary_lock_path(&data_dir.join(LOCKS_DIR), boundary)`,
  opens it, and calls **non-blocking** `try_lock()`. On contention,
  returns a new `CreateResult::LockContended` variant (rather than
  reusing `bool`, so `handle_intercept`'s logging can tell "skipped —
  another process is converting this project right now" apart from
  "the ioctl itself failed") and falls through to the real syscall —
  the shim must never block inside an intercepted call; a hang there
  freezes the host build, a far worse failure than missing one
  conversion opportunity that a later build or an explicit `convert`
  will pick up.
- **CLI side** (`convert::materialize(target: &Path, boundary: &Path)`):
  takes the same lock, **blocking** (`convert` is an explicit,
  occasional, human-run command — waiting briefly is fine), held only
  around the create/copy/rename sequence itself (`create_empty`/
  `copy_and_swap`), not the interactive "remember this?" prompt that
  runs before it. `materialize`'s three call sites in
  `resolve_candidate` gain the already-in-scope `&boundary` argument.
  `convert()`'s own signature gains a `data_dir: &Path` parameter
  (threaded through from `main.rs`, which already resolves it for
  `cache_path`/`project_roots_path`) so `materialize` can compute the
  lock path without re-deriving `data_dir` from another path's parent.

### 7. `convert::create_empty` — tolerate a race the shim just won

Small, related fix: `create_empty` currently propagates any error from
`btrfs::create_subvolume` with `?`, including `AlreadyExists` — if the
shim wins a race and creates the subvolume first, `convert` would
report an error for what is actually the desired end state. Match the
shim's own `try_create_subvolume` tolerance: treat
`ErrorKind::AlreadyExists` as success.

## Build order

Each step independently compiles, passes its own tests, and gets its
own commit before the next starts (per the standing workflow) — `cargo
fmt`/`clippy --all-targets -- -D warnings` clean, full test suite green,
and a standalone shim recompile check for anything touching `shim/`.

1. `atomic_write.rs`: unique temp filenames (§1's first half). Tests:
   two sequential `write_atomically` calls to the same destination
   never collide on temp-file identity even if the first hasn't
   renamed yet (simulate via a held file handle on the old temp path).
2. `shim/lock_core.rs` + `src/lock.rs`: the shared lock-path/open-file
   helpers (§2). Tests: `boundary_lock_path` escaping round-trips for
   paths containing `/` and `%`; two `File` handles opened on the same
   path can't both hold an exclusive lock (`try_lock` on the second
   fails while the first holds it).
3. Wire `reload.lock` into `reload()`/`scan::save_roots()` (§1's second
   half). Test: a held lock on `reload.lock` causes a concurrent
   `reload()` call to block (spawn a thread holding the lock briefly,
   confirm the second call only proceeds after release).
4. Single-`write_all()` fix across `register()`, `convert::append_decision()`,
   `main.rs`'s `Discover --save` loop, and the shim's `log_line()` (§3).
   Tests: existing append tests still pass; add one asserting a single
   line is written as exactly one line even when built from multiple
   format-string pieces (regression guard for the old multi-`write()`
   shape).
5. Rename `project-roots.txt` → `project-roots.list` (§4). Update every
   reference (constants, tests, design.md, README.md).
6. `ghostvolumes projects` subcommand restructuring: `src/register.rs`
   → `src/projects.rs`, `unregister`/`list_projects` functions,
   `project-roots.lock` wiring, `main.rs`/`Command` changes (§5). Tests:
   `unregister` with an exact path; auto-mode with a mix of
   still-existing and missing entries, TTY and non-TTY; `list_projects`'s
   missing-marker output; the lost-update race is closed (a `register`
   call between `unregister`'s read and write doesn't get dropped —
   testable by holding `project-roots.lock` open in the test to force
   the ordering).
7. Per-boundary shim-vs-convert lock (§6): `Decision::Accept(PathBuf)`,
   `try_create_subvolume`'s `CreateResult` + non-blocking `try_lock`,
   `convert::materialize`'s blocking `lock()` + boundary parameter,
   `convert()`'s `data_dir` parameter threaded through. Tests: shim-side
   contention falls through to a real `mkdir` rather than blocking
   (hold the boundary lock in the test, confirm the intercepted call
   still succeeds as a plain directory); CLI-side blocks and eventually
   succeeds once a held lock releases.
8. `convert::create_empty`'s `AlreadyExists` tolerance (§7). Test:
   pre-create the target as a subvolume, confirm `create_empty` no
   longer errors.
9. Docs pass: `design.md` (new "Key decisions" entries for the lock
   design, and the `project-roots.list` rename with the persistent
   -user-data-not-a-disposable-cache framing from §4 — explicitly
   noting that syncing it via a dotfile manager like chezmoi is fine,
   only live hand-editing while `ghostvolumes` might be running isn't),
   `README.md` (the `projects` command replaces `register` in the
   Commands table), `FAQ.md` if any workflow guidance references
   `register`, `CHANGELOG.md` (new entry).
