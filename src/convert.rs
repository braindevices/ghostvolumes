//! `ghostvolumes convert <path> [--create <relative-path>]...`
//! (ai-work/tasks/decision-model.plan.md §6, extended by
//! ai-work/tasks/convert-project-model.plan.md Phase 1): a recursive
//! walk-and-resolve, not just a single-leaf migration.
//!
//! `<path>` is *only ever* the project: the decision-file/project-roots
//! boundary. It is never itself added to the candidate list and never
//! itself converted — conflating "the project" with "a thing that
//! might get converted" under one argument was the source of real
//! confusion (naming an arbitrary directory would silently make it
//! *become* the registered project-roots boundary the moment any
//! nested decision got recorded, regardless of whether that directory
//! was ever meant to be treated as one project). If `<path>` isn't
//! already covered by a registered project, `convert`/`decide` ask
//! *once, upfront*, before touching any candidate — see
//! `ensure_project_registered`'s own doc comment for the full
//! decision tree (plain registration ask, a same-volume nesting
//! conflict, or an orphaned ancestor decision file all get handled
//! differently). Declining (in any branch), or no TTY at all, aborts
//! the whole command — registration is a hard prerequisite now, not a
//! side effect to guess at. **Nested project registration is
//! disallowed**: at most one registered project can ever cover a given
//! path (`ai-work/tasks/nested-project-boundaries.plan.md`) — decision
//! and ignore files already self-distribute via their own closest-
//! file-wins walk-up, so a hierarchy of registered projects was never
//! providing anything a single, correct stopping boundary doesn't
//! already give.
//!
//! Three ways a path becomes a candidate:
//! - The walk: every directory under `<path>` that's either a watched
//!   name under a configured root, or already a real BTRFS subvolume
//!   regardless of its name (`find_nested_candidates`, reusing
//!   `discover`'s tree-walking conventions — skip anything matching an
//!   ignore pattern, optional `--max-depth`, never descend into a
//!   match). Ignore patterns (Phase 2,
//!   `ai-work/tasks/convert-project-model.plan.md`) union three tiers:
//!   the global `default-ignore` list, the nearest configured *volume*
//!   root's own `.ghostvolumes-ignore` file, and `<path>`'s own
//!   `.ghostvolumes-ignore` file — see `is_ignored`.
//! - `--create <relative-path>` (repeatable): explicitly names a
//!   specific target (relative to `<path>`) to resolve directly,
//!   bypassing the watched-name-match requirement entirely — the
//!   direct replacement for what naming `<path>` itself used to do,
//!   now unambiguous since the project and its candidates are
//!   structurally different things (positional argument vs. flag).
//! - `decision_file_anchored_candidates`: any anchored, wildcard-free
//!   `+`/`?` pattern already in `boundary`'s own decision file —
//!   surfaces something the walk could never discover on its own,
//!   because its target doesn't exist on disk yet or doesn't match any
//!   watched name at all. An anchored `+` decision is the *persisted*
//!   equivalent of `--create`: recording it once keeps being honored
//!   on every future run, not just the one where `--create` was
//!   passed — without this, a decision for an unwatched, not-yet-
//!   created name would silently never actually get materialized.
//!
//! Each candidate, resolved shallowest-first (so an "every match of
//! this name" answer for a shallow candidate is already reflected by
//! the time a same-named, `**`-covered deeper one is resolved, instead
//! of asking twice):
//! - Already a subvolume → skip silently.
//! - A `+` decision already exists → convert directly, no asking.
//! - A `-` decision already exists → skip silently, unless this exact
//!   candidate was explicitly named via `--create` (a deliberate
//!   override attempt), in which case confirm before proceeding.
//! - Undecided (or doesn't exist yet), at a real TTY → ask "convert
//!   (and remember) this?" *before* touching the filesystem at all.
//!   "yes"/"all" converts (create empty, or copy-and-swap if already a
//!   plain directory) and records a `+` decision. "no" (or an
//!   empty/garbage answer) converts nothing and records a `-` decision
//!   instead — a real, deliberate decline, not silently forgotten.
//! - Undecided, no TTY at all (a script, cron job, or CI run) → can't
//!   ask, so don't: converts nothing, and appends a pending `?` marker
//!   noting the candidate, the same mechanism the shim itself already
//!   uses for the identical situation — a later real decision for the
//!   same pattern toggles that marker in place; a human can also turn
//!   it into a real `+`/`-` line by hand.
//!
//! Every actual filesystem-mutating step (`create_empty`/
//! `copy_and_swap`'s create/cp/rename/remove calls) prints a line to
//! stdout as it happens — unlike the shim, which can't touch stdout at
//! all (it's injected into arbitrary processes with no such
//! guarantee), `convert` is a deliberate, foreground, human-run
//! command, so there's no reason for it to be silent about what it did.
//! Separately, with `GHOSTVOLUMES_DEBUG=debug` (or more verbose) set —
//! see `ai-work/tasks/leveled-verbosity.plan.md` for the full
//! `error`/`warn`/`info`/`debug`/`trace` scale, shared with the shim's
//! own debug logging — `resolve_candidate` also traces *why* each
//! candidate resolved the way it did (already a subvolume, an existing
//! decision found at which boundary, or undecided) to stderr (or
//! `GHOSTVOLUMES_LOG_FILE`, if set) — this is what a raw, unconditional
//! `println!` sprinkled in ad hoc while debugging a specific issue
//! should have been from the start.
//!
//! Unlike Phase 1, project registration is no longer a side effect of
//! recording a decision — `ensure_project_registered` runs once,
//! upfront, before any candidate is touched, and every candidate in a
//! run resolves against the same, already-registered
//! `decision_boundary`.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::debug::{Verbosity, trace};
use crate::{btrfs, cache, decision, filenames, merge, project_roots, projects};

/// What resolving a candidate's decision should actually *do*, decomposed
/// into two independent capabilities
/// (`ai-work/tasks/decide-walk-and-markers.plan.md`) — `convert` and
/// `decide` are just two of the four combinations; a hypothetical
/// future "apply only what's already decided, ignore anything
/// undecided" mode (`Mode { decide: false, convert: true }`) needs no
/// further plumbing changes when it's ever wanted. Named fields, not
/// two bare positional `bool`s, so a call site can't silently swap
/// them with nothing catching it at compile time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mode {
    /// Consider undecided candidates at all — ask (if a TTY) or leave
    /// a pending `?` marker (if not). `false` means anything without
    /// an existing decision is silently skipped, no marker either —
    /// this is a genuinely different outcome than "asked, but no TTY
    /// available" (which still leaves a marker), not the same thing;
    /// `is_tty` stays its own, separate parameter for exactly this
    /// reason — it answers "can we get a real answer right now", not
    /// "do we want one at all".
    decide: bool,
    /// Actually materialize an approved candidate (an existing `+`, or
    /// a freshly-answered "yes"). `false` means only ever record or
    /// toggle the decision — never touch the filesystem.
    convert: bool,
}

impl Mode {
    /// `convert`'s own behavior: ask about (or apply) a decision, then
    /// actually materialize it.
    const CONVERT: Mode = Mode {
        decide: true,
        convert: true,
    };
    /// `decide`'s own behavior: ask about (or apply) a decision, but
    /// never touch the filesystem either way.
    const DECIDE: Mode = Mode {
        decide: true,
        convert: false,
    };
}

enum RememberChoice {
    Deny,
    JustThisPath,
    EveryMatchOfThisName,
}

fn parse_remember_answer(line: &str) -> RememberChoice {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => RememberChoice::JustThisPath,
        "a" | "all" => RememberChoice::EveryMatchOfThisName,
        _ => RememberChoice::Deny,
    }
}

/// Asks the "convert (and remember)?"/"decide (and remember)?"
/// question and parses the answer - always actually asks, unlike
/// `confirm_override`. Whether to ask at all based on `is_tty` is the
/// caller's call (`ask_and_maybe_convert`), since a non-interactive run
/// needs a different fallback (a pending marker, not a recorded deny)
/// than an interactive "no" does - a distinction this function doesn't
/// need to know about. The verb in the prompt is `mode`-aware -
/// asking "Convert...?" when `decide` will never actually convert,
/// even on "yes", would be misleading.
fn ask_remember(
    candidate: &Path,
    mode: Mode,
    read_line: impl FnOnce() -> Option<String>,
) -> RememberChoice {
    let verb = if mode.convert { "Convert" } else { "Decide" };
    eprint!(
        "{verb} (and remember this decision for) {}? [y]es, just this path / [a]ll matches of this name / [N]o: ",
        candidate.display()
    );
    let _ = std::io::stderr().flush();
    match read_line() {
        Some(line) => parse_remember_answer(&line),
        None => RememberChoice::Deny,
    }
}

/// Confirms a deliberate override of an existing `-` decision on the
/// literal `<path>` argument (§6). Same TTY/injectable posture as
/// `ask_remember`.
fn confirm_override(
    candidate: &Path,
    is_tty: bool,
    read_line: impl FnOnce() -> Option<String>,
) -> bool {
    if !is_tty {
        return false;
    }
    eprint!(
        "{} is marked to never be converted — continue anyway? [y/N]: ",
        candidate.display()
    );
    let _ = std::io::stderr().flush();
    match read_line() {
        Some(line) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        None => false,
    }
}

/// Asks about an already-a-subvolume candidate that has no recorded
/// decision at all — states the reason outright (unlike `ask_remember`'s
/// generic "convert (and remember)?", there's nothing left to convert
/// here, only a decision to record). Defaults to **yes** on an empty
/// answer, unlike every other ask in this file (which defaults to
/// declining, since it gates an actual filesystem mutation) — in the
/// overwhelming common case a directory that's already a real
/// subvolume was made that way specifically to hold volatile build
/// output, not by accident.
fn ask_about_existing_subvolume(
    candidate: &Path,
    read_line: impl FnOnce() -> Option<String>,
) -> bool {
    eprint!(
        "{} is already a subvolume with no recorded decision — record + for it? [Y/n]: ",
        candidate.display()
    );
    let _ = std::io::stderr().flush();
    match read_line() {
        Some(line) => {
            let trimmed = line.trim().to_ascii_lowercase();
            trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
        }
        None => false,
    }
}

/// `pub(crate)` rather than private - `projects::unregister`'s
/// auto-scan-and-prune mode reuses this exact injectable-stdin-reader
/// shape (see `ask_remember`/`confirm_override` above) rather than
/// duplicating it.
pub(crate) fn read_stdin_line() -> Option<String> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    Some(line)
}

fn read_decision_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// `true` iff `a` and `b` fall under the same configured `roots.d`
/// volume (the same `cache::longest_matching_prefix` result, including
/// both being `None` — neither covered by any configured root at all).
/// Two paths can be ancestor/descendant of each other in plain path
/// terms while sitting on *different* BTRFS roots (a narrower row
/// nested inside a broader one) — `ai-work/tasks/nested-project-boundaries.plan.md`
/// bug #2. Path containment alone is never enough to decide whether one
/// registered project's scope legitimately extends to cover another
/// path; it must also be the same volume.
fn same_volume(rows: &[(String, String)], a: &Path, b: &Path) -> bool {
    cache::longest_matching_prefix(rows, a) == cache::longest_matching_prefix(rows, b)
}

/// The decision-file (and ignore-file project-root tier's) walk-up
/// boundary for the whole `convert`/`decide` run — computed *once* from
/// `project_path` alone, not per-candidate, since nested project
/// registration is disallowed (enforced at `ensure_project_registered`
/// time, see its own doc comment): there is at most one registered
/// project that can ever cover `project_path`, so there's nothing to
/// pick "the more specific of several" from the way the old
/// `walkup_boundary` tried to.
///
/// Finds the registered project that is both an ancestor-or-self of
/// `project_path` *and* on the same volume (`same_volume`) — a project
/// that's a path-ancestor but on a different, more specific volume
/// doesn't count (bug #2); falls back to `project_path` itself if
/// nothing qualifies (by the time this runs, `ensure_project_registered`
/// has already guaranteed `project_path` itself is covered, one way or
/// another). Deliberately never falls back further to a bare
/// `rows`/volume prefix — that's `project_roots` existing precisely to
/// avoid: letting decision resolution wander into broad, incidental
/// root territory rather than staying scoped to a real, registered
/// project.
fn decision_boundary(
    rows: &[(String, String)],
    project_roots: &[String],
    project_path: &Path,
) -> PathBuf {
    project_roots
        .iter()
        .map(Path::new)
        .filter(|root| project_path.starts_with(root) && same_volume(rows, root, project_path))
        // Shortest (shallowest) of any qualifying matches - the top of
        // a nested chain of same-volume registered projects, so
        // resolve()'s own walk-up checks every level's decision file
        // down to project_path instead of stopping at the deepest one.
        .min_by_key(|root| root.as_os_str().len())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| project_path.to_path_buf())
}

/// The `+ <pattern>` line for "every match of this name" (§6),
/// anchored to `candidate`'s own containing directory so it never
/// silently covers an unrelated same-named directory elsewhere that
/// was never actually looked at.
fn containing_dir_pattern(boundary: &Path, candidate: &Path, name: &str) -> String {
    let containing = candidate.parent().unwrap_or(boundary);
    match decision::anchored_pattern(boundary, containing) {
        Some(prefix) if prefix != "/" => format!("{prefix}/**/{name}"),
        _ => format!("/**/{name}"),
    }
}

/// Idempotently registers `boundary` into the project-roots list (§3),
/// both on disk (via `projects::register`, so later `convert`/shim
/// invocations see it too) and in the in-memory `project_roots` list
/// (so later candidates *within this same run* see it without a second
/// disk read).
fn register_project_root(
    boundary: &Path,
    project_roots: &mut Vec<String>,
    project_roots_path: &Path,
) -> anyhow::Result<()> {
    // `Path::display()` essentially never produces a trailing slash on
    // its own, but normalizing here too keeps this in-memory list (used
    // for this same run's own dedup checks) consistent with whatever
    // `projects::register` writes to disk, rather than relying on that
    // being incidentally true.
    let boundary_str = crate::project_roots::normalize_root_path(&boundary.display().to_string());
    if !project_roots.iter().any(|r| r == &boundary_str) {
        project_roots.push(boundary_str.clone());
    }
    projects::register(project_roots_path, &boundary_str)
}

/// Blocking-locks `boundary`'s decisions lock file
/// (`locks/decisions/<boundary>.lock`, distinct from `materialize`'s
/// own `locks/<boundary>.lock` for subvolume creation) around any
/// read-then-write of the decision file - needed because toggling a
/// pending marker into a real decision is a read-modify-write, unlike
/// every other decision-file write, which was ever only a pure append.
/// Blocking is fine here (unlike the shim's non-blocking equivalent):
/// `convert` is an explicit, occasional, human-run command, not
/// something injected into an arbitrary intercepted call.
fn lock_decisions(data_dir: &Path, boundary: &Path) -> anyhow::Result<std::fs::File> {
    let lock_path = crate::lock::boundary_lock_path(
        &data_dir.join(filenames::LOCKS_DIR).join("decisions"),
        boundary,
    );
    let lock_file = crate::lock::open_lock_file(&lock_path)?;
    lock_file.lock()?;
    Ok(lock_file)
}

/// Appends a `? <pattern>` pending-marker line noting `candidate` as
/// still undecided — the exact mechanism the shim itself already uses
/// for the same situation (`shim/preload.rs`'s `append_pending_marker`,
/// via the same `decision::anchored_pattern`/`pending_marker_line`/
/// `needs_pending_marker` trio), reused here rather than duplicated so
/// a candidate `convert` can't ask about (no TTY - a script, cron job,
/// or CI run) leaves the same trail a human can later turn into a real
/// `+`/`-` line by hand.
fn append_pending_marker(data_dir: &Path, boundary: &Path, candidate: &Path) -> anyhow::Result<()> {
    let Some(pattern) = decision::anchored_pattern(boundary, candidate) else {
        return Ok(());
    };
    let _lock = lock_decisions(data_dir, boundary)?;
    let file_path = boundary.join(filenames::DECISION_FILE_NAME);
    let existing = std::fs::read_to_string(&file_path).unwrap_or_default();
    if !decision::needs_pending_marker(&existing, &pattern) {
        return Ok(());
    }
    std::fs::create_dir_all(boundary)?;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)?;
    file.write_all(format!("{}\n", decision::pending_marker_line(&pattern)).as_bytes())?;
    Ok(())
}

/// Records a real `+`/`-` decision (`prefix` is `"+"` or `"-"`,
/// `decision_pattern` its own pattern) for `boundary`'s decision file,
/// replacing whatever pending marker `anchored_pattern` (the pattern a
/// prior undecided-candidate note for this exact candidate would have
/// used) refers to — `toggle_or_replace_pending` only matches against
/// `anchored_pattern` to find the line; it doesn't care whether the
/// replacement's own pattern matches too. So this is a true in-place
/// same-line swap even for "a"/every-match-of-this-name, whose
/// `decision_pattern` is a broader pattern than the anchored marker it
/// supersedes: no need to remove the marker and append the broader
/// line separately at the end, just put the new content where the
/// marker already was. Falls back to a plain append (unchanged file
/// position) only when there was no pending marker to begin with.
///
/// Rewrites the whole file (`atomic_write::write_atomically`, not a
/// plain append) under `lock_decisions` - the first read-modify-write
/// on a decision file in this whole design, needing both the lock (so
/// it can't race a concurrent shim append) and an atomic replace (so a
/// reader never observes a half-written file).
fn record_decision(
    data_dir: &Path,
    boundary: &Path,
    anchored_pattern: &str,
    decision_pattern: &str,
    prefix: &str,
) -> anyhow::Result<()> {
    let _lock = lock_decisions(data_dir, boundary)?;
    std::fs::create_dir_all(boundary)?;
    let file_path = boundary.join(filenames::DECISION_FILE_NAME);
    let existing = std::fs::read_to_string(&file_path).unwrap_or_default();
    let decision_line = format!("{prefix} {decision_pattern}");
    let updated = decision::toggle_or_replace_pending(&existing, anchored_pattern, &decision_line);
    crate::atomic_write::write_atomically(&file_path, &updated)
}

/// Asks "convert this?" *before* touching the filesystem at all. Three
/// outcomes, none of which leave a candidate silently forgotten:
/// - No TTY at all (a script, cron job, or CI run) - can't ask, so
///   don't: append a pending `?` marker instead (same as the shim's
///   own undecided-candidate handling) and convert nothing.
/// - Answered "no" (or empty/garbage) at an actual prompt - a real,
///   deliberate decision, not just "couldn't ask": record a `-` for
///   this exact path, same as if it had been hand-authored, and
///   convert nothing.
/// - Answered "yes"/"all" - convert, then record the `+` decision. Only
///   after a successful `materialize` - a failed conversion must never
///   leave a `+` line for something that was never actually converted.
///
/// Ask-before-acting throughout (matches `confirm_override`'s existing
/// posture for an existing `-` decision) rather than converting
/// unconditionally and only asking afterward whether to persist it -
/// the original, surprising order, where a candidate would already be
/// a subvolume by the time a human saw a prompt they could still say
/// "no" to.
fn ask_and_maybe_convert(
    candidate: &Path,
    boundary: &Path,
    data_dir: &Path,
    is_tty: bool,
    mode: Mode,
    read_line: &mut impl FnMut() -> Option<String>,
) -> anyhow::Result<()> {
    let anchored = decision::anchored_pattern(boundary, candidate)
        .unwrap_or_else(|| candidate.display().to_string());
    if !is_tty {
        // Always printed, not silent - the shim's own handling of the
        // identical situation logs this unconditionally too ("this is
        // the one signal a human has that a decision is waiting to be
        // made, so it can't be silent-by-default"), and convert already
        // reports every other real step it takes.
        println!(
            "skip: {} (undecided — run with a TTY to decide, or edit the decision file by hand)",
            candidate.display()
        );
        return append_pending_marker(data_dir, boundary, candidate);
    }
    let choice = ask_remember(candidate, mode, read_line);
    let pattern = match choice {
        RememberChoice::Deny => {
            return record_decision(data_dir, boundary, &anchored, &anchored, "-");
        }
        RememberChoice::JustThisPath => anchored.clone(),
        RememberChoice::EveryMatchOfThisName => {
            let name = candidate
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            containing_dir_pattern(boundary, candidate, &name)
        }
    };
    if mode.convert {
        materialize(candidate, boundary, data_dir)?;
    }
    record_decision(data_dir, boundary, &anchored, &pattern, "+")
}

/// Creates `target` directly as a fresh, empty subvolume — replaces
/// cd-hook's old proactive pre-creation (§6). Creates any missing
/// parent directories first (the common case is a parent that already
/// exists; only the literal `<path>` argument could plausibly need
/// this).
fn create_empty(target: &Path) -> anyhow::Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", target.display()))?;
    let name = target
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", target.display()))?
        .to_string_lossy()
        .into_owned();
    std::fs::create_dir_all(parent)?;

    // AlreadyExists is tolerated, not propagated - materialize()'s own
    // lock (ai-work/tasks/atomic-file-io.plan.md §6/§7) makes this rare
    // (the shim can't be mid-creation while this lock is held), but the
    // shim could still have won a race and created it just before this
    // call took the lock. Either way the desired end state - target is
    // a subvolume - already holds, matching the shim's own
    // try_create_subvolume tolerance for the identical race.
    match btrfs::create_subvolume(parent, &name) {
        Ok(()) => {
            println!("create: {} (new empty subvolume)", target.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Creates a new subvolume at a temp sibling path, `cp -a
/// --reflink=always`s the existing plain directory's contents in
/// (cheap on BTRFS: extent-sharing metadata, not a real copy, though
/// still a full tree walk so cost scales with file count not size),
/// then atomically swaps it into place and removes the old plain
/// directory.
fn copy_and_swap(path: &Path) -> anyhow::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", path.display()))?
        .to_string_lossy()
        .into_owned();

    let tmp_name = format!(".{name}.ghostvolumes-convert-tmp");
    let tmp_path = parent.join(&tmp_name);
    if tmp_path.exists() {
        anyhow::bail!(
            "temp path {} already exists; a previous convert may have failed partway — \
             remove it manually and retry",
            tmp_path.display()
        );
    }
    btrfs::create_subvolume(parent, &tmp_name)?;
    println!("create: {} (temporary subvolume)", tmp_path.display());
    let status = Command::new("cp")
        .arg("-a")
        .arg("--reflink=always")
        .arg("--")
        .arg(format!("{}/.", path.display()))
        .arg(&tmp_path)
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "cp -a --reflink=always into {} failed: {status}",
            tmp_path.display()
        );
    }
    println!(
        "cp -a --reflink=always: {} -> {}",
        path.display(),
        tmp_path.display()
    );

    // Atomic swap: move the old plain dir out of the way, move the new
    // subvolume into place, then clean up the old dir. `path` is never
    // missing or half-written in between the two renames.
    let backup_name = format!(".{name}.ghostvolumes-convert-old");
    let backup_path = parent.join(&backup_name);
    std::fs::rename(path, &backup_path)?;
    println!("rename: {} -> {}", path.display(), backup_path.display());
    std::fs::rename(&tmp_path, path)?;
    println!(
        "rename: {} -> {} (subvolume now in place)",
        tmp_path.display(),
        path.display()
    );
    std::fs::remove_dir_all(&backup_path)?;
    println!("remove: {} (old backup)", backup_path.display());

    Ok(())
}

/// Blocking-locks `boundary`'s per-project lock file
/// (ai-work/tasks/atomic-file-io.plan.md §6) around the create/copy
/// /rename sequence below - coordinates with the shim's own
/// `try_create_subvolume`, which takes the same lock (non-blocking)
/// before creating a subvolume for any candidate under this same
/// boundary. Blocking is fine here (unlike the shim): `convert` is an
/// explicit, occasional, human-run command, not something injected
/// into an arbitrary intercepted call. Held only around this
/// operation, not the interactive "remember this?" prompt that runs
/// before it - that could take arbitrarily long, and there's no need
/// to hold the lock while waiting on a human.
fn materialize(target: &Path, boundary: &Path, data_dir: &Path) -> anyhow::Result<()> {
    let lock_path = crate::lock::boundary_lock_path(&data_dir.join(filenames::LOCKS_DIR), boundary);
    let lock_file = crate::lock::open_lock_file(&lock_path)?;
    lock_file.lock()?;
    if target.exists() {
        copy_and_swap(target)
    } else {
        create_empty(target)
    }
}

fn read_ignore_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Should `candidate` (a directory the walk is about to check/recurse
/// into) be skipped entirely — never even checked for a watched-name
/// match? Three tiers, unioned (Phase 2,
/// `ai-work/tasks/convert-project-model.plan.md`):
/// - `global_ignore` (the merged `default-ignore` list), anchored to
///   `candidate`'s own parent directory — the natural anchor for the
///   common bare-name case (e.g. `.git`), since an unanchored pattern
///   matches by leaf name alone regardless of what anchor is used; an
///   anchored global pattern is a degenerate, rare case this anchor
///   choice doesn't handle meaningfully, but there's no single
///   "correct" anchor for a pattern with no natural directory of its
///   own.
/// - The nearest configured *volume* root's own `.ghostvolumes-ignore`
///   file, found via `cache::longest_matching_prefix` — the same
///   computation `decision_boundary` uses (for a different purpose) for
///   decisions.
/// - `boundary`'s own `.ghostvolumes-ignore` file — `boundary` is the
///   resolved `decision_boundary` for this run (either `project_path`
///   itself, or a shallower already-registered covering project), so
///   this tier stays consistent with decision resolution rather than
///   always reading `project_path`'s own file even when a shallower
///   project is the one actually in effect.
///
/// Unlike decision files, an ignore file is read fresh at each
/// directory visited rather than cached across the walk — simpler, and
/// this isn't a hot path the way per-syscall shim logic is.
fn is_ignored(
    rows: &[(String, String)],
    boundary: &Path,
    global_ignore: &[String],
    candidate: &Path,
) -> bool {
    let anchor = candidate.parent().unwrap_or(candidate);
    if decision::ignore_matches(global_ignore, anchor, candidate) {
        return true;
    }
    if let Some(volume_root) = cache::longest_matching_prefix(rows, anchor) {
        let volume_root = PathBuf::from(volume_root);
        if let Some(text) = read_ignore_file(&volume_root.join(filenames::IGNORE_FILE_NAME)) {
            let patterns = decision::parse_ignore_patterns(&text);
            if decision::ignore_matches(&patterns, &volume_root, candidate) {
                return true;
            }
        }
    }
    if let Some(text) = read_ignore_file(&boundary.join(filenames::IGNORE_FILE_NAME)) {
        let patterns = decision::parse_ignore_patterns(&text);
        if decision::ignore_matches(&patterns, boundary, candidate) {
            return true;
        }
    }
    false
}

/// Every candidate implied by an anchored, wildcard-free `+`/`?`
/// pattern in `boundary`'s own decision file — surfaces something the
/// filesystem walk could never discover on its own, because its target
/// doesn't exist on disk yet, or doesn't match any watched name at all
/// (an anchored `+` decision is the *persisted* equivalent of
/// `--create`: recording it once should keep being honored on every
/// future run, not just the one where `--create` was passed). Shared
/// by `convert` and `decide` — the only difference between them is
/// `Mode`, same as everywhere else in the per-candidate resolution.
fn decision_file_anchored_candidates(boundary: &Path) -> Vec<PathBuf> {
    let text =
        std::fs::read_to_string(boundary.join(filenames::DECISION_FILE_NAME)).unwrap_or_default();
    decision::parse_anchored_exact_patterns(&text)
        .into_iter()
        .map(|pattern| boundary.join(pattern.trim_start_matches('/')))
        .collect()
}

/// Walks `project_path`'s subtree (skipping anything `is_ignored`,
/// never descending into a match — same conventions as
/// `discover::walk`), collecting every directory that's either a
/// watched name under a configured root at its own location
/// (`cache::names_for`, which is root-scoped, so this naturally
/// excludes anything outside every configured root) *or* already a
/// real BTRFS subvolume regardless of its name — an existing subvolume
/// is itself evidence someone already decided to convert it, whether
/// or not its name is (or ever was) on the watch list.
/// `project_path` itself is not included — the caller already knows
/// never to treat it as a candidate. `boundary` is the resolved
/// `decision_boundary` for this run, threaded through separately from
/// the walk's own recursion cursor purely for `is_ignored`'s
/// project-root ignore-file tier.
fn find_nested_candidates(
    project_path: &Path,
    boundary: &Path,
    max_depth: Option<u32>,
    rows: &[(String, String)],
    global_ignore: &[String],
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    find_nested_candidates_inner(
        project_path,
        boundary,
        max_depth,
        rows,
        global_ignore,
        0,
        &mut out,
    );
    out
}

#[allow(clippy::too_many_arguments)]
fn find_nested_candidates_inner(
    dir: &Path,
    boundary: &Path,
    max_depth: Option<u32>,
    rows: &[(String, String)],
    global_ignore: &[String],
    depth: u32,
    out: &mut Vec<PathBuf>,
) {
    if let Some(max) = max_depth
        && depth > max
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let names = cache::names_for(rows, dir);
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let path = entry.path();
        if is_ignored(rows, boundary, global_ignore, &path) {
            continue;
        }
        // A real, already-existing subvolume is itself direct evidence
        // someone already decided to convert it - a candidate
        // regardless of whether its name happens to be on the watch
        // list right now (a name could've been watched when it was
        // created and removed from the list since, or converted by
        // hand for a name nobody thought to add at all).
        // `resolve_candidate` asks about it (defaulting to yes) if it
        // has no decision yet - never descended into either way, same
        // as a watched-name match.
        if names.contains(name_str.as_ref()) || btrfs::is_subvolume(&path).unwrap_or(false) {
            out.push(path);
            continue; // never descend into a match
        }
        find_nested_candidates_inner(
            &path,
            boundary,
            max_depth,
            rows,
            global_ignore,
            depth + 1,
            out,
        );
    }
}

/// `boundary` is the whole run's precomputed `decision_boundary` — the
/// same value for every candidate, no longer recomputed per-candidate
/// (see `decision_boundary`'s own doc comment for why that's now
/// correct rather than a loss of precision).
/// Prints what `resolve_candidate`'s `Some(true)` branch would do
/// without doing it — mirrors `materialize`'s own real
/// create-vs-migrate split (`ai-work/tasks/convert-project-model.plan.md`
/// Phase 3), since a dry run should say which of the two a real run
/// would actually perform.
fn report_would_materialize(candidate: &Path) {
    if candidate.exists() {
        println!(
            "would convert: {} (existing + decision)",
            candidate.display()
        );
    } else {
        println!(
            "would create: {} (existing + decision)",
            candidate.display()
        );
    }
}

/// `boundary` is the whole run's precomputed `decision_boundary`, `dry_run`
/// short-circuits every branch that would otherwise mutate the
/// filesystem, the decision file, or prompt — printing what *would*
/// happen instead (Phase 3, `ai-work/tasks/convert-project-model.plan.md`).
/// Every dry-run message is a plain, unconditional `println!` (not
/// gated by verbosity) since it's the direct, primary output of the
/// command, the same way a real run's `create:`/`cp -a:`/`rename:`
/// lines already are.
#[allow(clippy::too_many_arguments)]
fn resolve_candidate(
    candidate: &Path,
    boundary: &Path,
    create: &[PathBuf],
    data_dir: &Path,
    is_tty: bool,
    dry_run: bool,
    mode: Mode,
    mut read_line: &mut impl FnMut() -> Option<String>,
) -> anyhow::Result<()> {
    let existing_decision = decision::resolve(
        candidate,
        boundary,
        filenames::DECISION_FILE_NAME,
        read_decision_file,
    );

    if btrfs::is_subvolume(candidate).unwrap_or(false) {
        if existing_decision.is_some() {
            trace(Verbosity::Debug, || {
                format!(
                    "{}: already a subvolume, decision already recorded -> skip",
                    candidate.display()
                )
            });
            return Ok(());
        }
        // Manually converted (or converted by a prior run before this
        // candidate had a decision) - there's nothing left to
        // *convert*, only a decision to record, so this is treated as
        // its own undecided candidate: same TTY/no-TTY split as any
        // other one, but the "yes" path never calls `materialize` -
        // the desired end state already holds (and would be actively
        // wrong to try: `copy_and_swap`'s final `remove_dir_all` can't
        // remove a real subvolume, that needs `BTRFS_IOC_SNAP_DESTROY`,
        // not a plain `rmdir()`).
        if !mode.decide {
            trace(Verbosity::Debug, || {
                format!(
                    "{}: already a subvolume, undecided, decide disabled -> skip",
                    candidate.display()
                )
            });
            return Ok(());
        }
        trace(Verbosity::Debug, || {
            format!(
                "{}: already a subvolume, undecided -> ask (default yes) or pending marker",
                candidate.display()
            )
        });
        if dry_run {
            println!(
                "already a subvolume, undecided: {} (skipped — dry run)",
                candidate.display()
            );
            return Ok(());
        }
        let anchored = decision::anchored_pattern(boundary, candidate)
            .unwrap_or_else(|| candidate.display().to_string());
        if !is_tty {
            println!(
                "skip: {} (already a subvolume, undecided — run with a TTY to decide, or edit the decision file by hand)",
                candidate.display()
            );
            return append_pending_marker(data_dir, boundary, candidate);
        }
        if ask_about_existing_subvolume(candidate, read_line) {
            record_decision(data_dir, boundary, &anchored, &anchored, "+")?;
            println!(
                "recorded: + {anchored} (at {}, already a subvolume)",
                boundary.display()
            );
        } else {
            record_decision(data_dir, boundary, &anchored, &anchored, "-")?;
            println!("recorded: - {anchored} (at {})", boundary.display());
        }
        return Ok(());
    }

    // Every candidate is either an explicit `--create` target or
    // discovered by the walk (which only ever finds watched-name
    // matches) - `project_path` itself is never a candidate at all
    // (see the module doc comment), so there's no more "is this the
    // literal <path> argument" check needed the way there used to be.
    let is_explicit = create.iter().any(|c| c == candidate);

    match existing_decision {
        Some(true) => {
            if !mode.convert {
                trace(Verbosity::Debug, || {
                    format!(
                        "{}: existing + decision at boundary {} -> convert disabled, no-op",
                        candidate.display(),
                        boundary.display()
                    )
                });
                return Ok(());
            }
            trace(Verbosity::Debug, || {
                format!(
                    "{}: existing + decision at boundary {} -> materialize",
                    candidate.display(),
                    boundary.display()
                )
            });
            if dry_run {
                report_would_materialize(candidate);
                return Ok(());
            }
            materialize(candidate, boundary, data_dir)
        }
        Some(false) => {
            if !is_explicit {
                trace(Verbosity::Debug, || {
                    format!(
                        "{}: existing - decision at boundary {}, found via the walk -> skip silently",
                        candidate.display(),
                        boundary.display()
                    )
                });
                return Ok(()); // found via the walk, not named explicitly - skip silently
            }
            trace(Verbosity::Debug, || {
                format!(
                    "{}: existing - decision at boundary {}, explicitly created -> confirm override",
                    candidate.display(),
                    boundary.display()
                )
            });
            if dry_run {
                println!(
                    "would ask to override the '-' decision for {} (skipped — dry run)",
                    candidate.display()
                );
                return Ok(());
            }
            if !confirm_override(candidate, is_tty, &mut read_line) {
                return Ok(());
            }
            ask_and_maybe_convert(candidate, boundary, data_dir, is_tty, mode, read_line)
        }
        None => {
            if !mode.decide {
                trace(Verbosity::Debug, || {
                    format!(
                        "{}: undecided at boundary {}, decide disabled -> skip silently, no marker",
                        candidate.display(),
                        boundary.display()
                    )
                });
                return Ok(());
            }
            trace(Verbosity::Debug, || {
                format!(
                    "{}: undecided at boundary {} -> ask",
                    candidate.display(),
                    boundary.display()
                )
            });
            if dry_run {
                println!("undecided: {} (skipped — dry run)", candidate.display());
                return Ok(());
            }
            ask_and_maybe_convert(candidate, boundary, data_dir, is_tty, mode, read_line)
        }
    }
}

/// `true` only for an explicit "y"/"yes" — every other answer,
/// including an empty one, is a decline. The shared shape for every
/// *default-no* confirmation in `ensure_project_registered` below
/// (nesting, orphaned ancestor) — unlike the plain registration ask,
/// where an empty answer means yes.
fn read_yes_no(read_line: &mut impl FnMut() -> Option<String>) -> bool {
    match read_line() {
        Some(line) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        None => false,
    }
}

/// Walks up from `path`'s own parent looking for a decision file some
/// human may have already authored for a broader ancestor project — a
/// signal, when `path` isn't covered by any registered project, that
/// its *actual* intended project boundary might be that ancestor
/// (forgotten, or not yet registered), not `path` itself.
/// `ensure_project_registered` uses this to warn rather than silently
/// registering `path` as its own, narrower project. Bounded by `limit`
/// (`path`'s own volume, or `None` to walk all the way to the
/// filesystem root) — nothing past the volume boundary is even
/// BTRFS-managed territory, so a decision file there wouldn't be
/// actionable anyway.
fn nearest_ancestor_decision_file(path: &Path, limit: Option<&Path>) -> Option<PathBuf> {
    for ancestor in path.ancestors().skip(1) {
        if ancestor.join(filenames::DECISION_FILE_NAME).is_file() {
            return Some(ancestor.to_path_buf());
        }
        if Some(ancestor) == limit {
            break;
        }
    }
    None
}

/// Ensures `path` (a project argument to `convert`/`decide`) is covered
/// by exactly one registered project before anything else happens —
/// explicit and upfront, never a side effect of some candidate's
/// decision getting recorded. **Nested project registration is never
/// allowed** (`ai-work/tasks/nested-project-boundaries.plan.md`): at
/// most one registered project can ever cover a given path, since
/// decision/ignore files already self-distribute via their own
/// closest-file-wins walk-up — a hierarchy of registered projects was
/// never providing anything beyond a single, correct stopping boundary.
///
/// Four outcomes, checked in order:
/// 1. `path` already covered (ancestor-or-self, and on the *same
///    volume* — see `same_volume`; a path-ancestor on a different,
///    more specific BTRFS root doesn't count) by a registered project
///    → no-op, that project's decisions already apply to `path`.
/// 2. Not covered, but registering `path` would nest *over* an
///    already-registered, same-volume descendant project → the one
///    case that can't just be asked "register?", since doing so would
///    silently orphan the descendant's own decisions the moment a
///    shallower project takes precedence (`decision_boundary` always
///    prefers whichever covering project is more specific). Warns,
///    lists the conflicting project(s), and asks whether to unregister
///    them and register `path` as the new parent instead (default
///    **no** — a real structural change, not a reversible-by-default
///    action like plain registration below).
/// 3. Not covered, no nesting conflict, but a decision file exists at
///    some ancestor of `path` (up to `path`'s own volume boundary, see
///    `nearest_ancestor_decision_file`) with nothing registered
///    covering it — a parent registration may have been forgotten.
///    Warns and asks whether to continue and register `path` as its
///    own project anyway (default **no**).
/// 4. Otherwise: today's plain "Register `path` as a project? [Y/n]"
///    (default *yes* on an empty interactive answer — registering here
///    is low-stakes and easily reversed via `projects unregister`,
///    unlike every default-no ask above, which gates a real structural
///    decision).
///
/// A missing TTY at any of the above still can't be presumed into a
/// "yes": there's no human there at all, so every branch aborts rather
/// than guessing. Declining (explicitly, or via no TTY) aborts the
/// whole command in every branch — registration is a hard prerequisite
/// now, not a soft preference.
///
/// `dry_run` (Phase 3, `ai-work/tasks/convert-project-model.plan.md`)
/// short-circuits right after the coverage check (branch 1) — a dry
/// run never asks, never mutates `project_roots`/disk, and never
/// aborts for an uncovered path; it just reports that a real run would
/// need to register `path` first. This also means `decision_boundary`
/// (computed by the caller right after this returns) correctly falls
/// back to `path` itself for the rest of the preview — the exact
/// boundary a real, confirmed registration would produce in every one
/// of branches 2-4 above, since a confirmed registration always ends
/// with `path` itself as the sole covering project.
fn ensure_project_registered(
    path: &Path,
    project_roots: &mut Vec<String>,
    project_roots_path: &Path,
    rows: &[(String, String)],
    is_tty: bool,
    dry_run: bool,
    read_line: &mut impl FnMut() -> Option<String>,
) -> anyhow::Result<()> {
    let is_ancestor_or_self_same_volume =
        |root: &str| path.starts_with(Path::new(root)) && same_volume(rows, Path::new(root), path);
    if let Some(covering) = project_roots
        .iter()
        .find(|r| is_ancestor_or_self_same_volume(r))
    {
        trace(Verbosity::Debug, || {
            format!(
                "{}: already covered by registered project {covering} (same volume) -> no-op",
                path.display()
            )
        });
        return Ok(()); // covered by an existing, same-volume project already
    }

    if dry_run {
        trace(Verbosity::Debug, || {
            format!(
                "{}: not covered by any registered project -> would register (dry run)",
                path.display()
            )
        });
        println!(
            "would register: {} as a project (skipped — dry run)",
            path.display()
        );
        return Ok(());
    }

    let conflicting_children: Vec<String> = project_roots
        .iter()
        .filter(|root| {
            let root_path = Path::new(root.as_str());
            root_path.starts_with(path) && root_path != path && same_volume(rows, root_path, path)
        })
        .cloned()
        .collect();

    if !conflicting_children.is_empty() {
        let list = conflicting_children.join(", ");
        trace(Verbosity::Debug, || {
            format!(
                "{}: not covered, but registering would nest over already-registered project(s) \
                 {list} (same volume) -> ask to unregister them",
                path.display()
            )
        });
        if !is_tty {
            anyhow::bail!(
                "{} is not covered by any registered project, and registering it would nest over \
                 already-registered project(s) on the same volume ({list}) — nested projects \
                 aren't supported, and there's no TTY to ask; unregister the conflicting \
                 project(s) first with `ghostvolumes projects unregister <path>`, or run \
                 convert/decide interactively",
                path.display(),
            );
        }
        eprintln!(
            "Registering {} would nest over already-registered project(s) on the same volume: {list}",
            path.display(),
        );
        eprint!(
            "Unregister them and register {} as the new parent instead? [y/N]: ",
            path.display()
        );
        let _ = std::io::stderr().flush();
        if !read_yes_no(read_line) {
            anyhow::bail!(
                "stopped — nested projects aren't supported; unregister {list} first if {} \
                 should become the new parent, or point convert/decide at one of them directly \
                 instead",
                path.display(),
            );
        }
        for child in &conflicting_children {
            projects::unregister(project_roots_path, Some(child.as_str()))?;
        }
        project_roots.retain(|r| !conflicting_children.contains(r));
        return register_project_root(path, project_roots, project_roots_path);
    }

    let volume = cache::longest_matching_prefix(rows, path).map(PathBuf::from);
    let orphan = nearest_ancestor_decision_file(path, volume.as_deref());

    if let Some(ancestor) = &orphan {
        trace(Verbosity::Debug, || {
            format!(
                "{}: not covered, no nesting conflict, but an orphaned decision file exists at \
                 {} -> ask before registering",
                path.display(),
                ancestor.display()
            )
        });
        if !is_tty {
            anyhow::bail!(
                "{} is not covered by any registered project, and there's no TTY to ask — a \
                 decision file exists at {} with nothing registered covering it; register that \
                 (or another) ancestor first with `ghostvolumes projects register <path>`, or \
                 run convert/decide interactively",
                path.display(),
                ancestor.display(),
            );
        }
        eprintln!(
            "A decision file exists at {} (an ancestor of {}), but no registered project covers \
             {} yet.",
            ancestor.display(),
            path.display(),
            path.display(),
        );
        eprint!(
            "Continue and register {} as its own project anyway? [y/N]: ",
            path.display()
        );
        let _ = std::io::stderr().flush();
        if !read_yes_no(read_line) {
            anyhow::bail!(
                "stopped — register {} (or another ancestor) as the parent project first if it \
                 should cover {}, or re-run and confirm to register {} directly",
                ancestor.display(),
                path.display(),
                path.display(),
            );
        }
        return register_project_root(path, project_roots, project_roots_path);
    }

    trace(Verbosity::Debug, || {
        format!(
            "{}: not covered, no nesting conflict, no orphaned decision file -> plain register ask",
            path.display()
        )
    });
    if !is_tty {
        anyhow::bail!(
            "{} is not a registered project, and there's no TTY to ask — \
             run `ghostvolumes projects register {}` first, or run convert interactively",
            path.display(),
            path.display()
        );
    }
    eprint!("Register {} as a project? [Y/n]: ", path.display());
    let _ = std::io::stderr().flush();
    let confirmed = match read_line() {
        Some(line) => {
            let trimmed = line.trim().to_ascii_lowercase();
            trimmed.is_empty() || trimmed == "y" || trimmed == "yes"
        }
        None => false,
    };
    if !confirmed {
        anyhow::bail!(
            "{} was not registered as a project — aborting",
            path.display()
        );
    }
    register_project_root(path, project_roots, project_roots_path)
}

/// Real entry point: real TTY/stdin. See `convert_with_io` for the
/// testable core — now that the ask-then-convert gate decides whether
/// a candidate is touched at all (not just whether the decision gets
/// persisted), it needs the same injectable-stdin treatment as
/// `reload_with_validator`/`unregister_with_io` elsewhere in this crate.
#[allow(clippy::too_many_arguments)]
pub fn convert(
    path: &Path,
    create: &[PathBuf],
    max_depth: Option<u32>,
    config_dir: &Path,
    cache_path: &Path,
    project_roots_path: &Path,
    data_dir: &Path,
    dry_run: bool,
) -> anyhow::Result<()> {
    let mut read_line = read_stdin_line;
    convert_with_io(
        path,
        create,
        max_depth,
        config_dir,
        cache_path,
        project_roots_path,
        data_dir,
        dry_run,
        std::io::stdin().is_terminal(),
        &mut read_line,
    )
}

#[allow(clippy::too_many_arguments)]
fn convert_with_io(
    path: &Path,
    create: &[PathBuf],
    max_depth: Option<u32>,
    config_dir: &Path,
    cache_path: &Path,
    project_roots_path: &Path,
    data_dir: &Path,
    dry_run: bool,
    is_tty: bool,
    read_line: &mut impl FnMut() -> Option<String>,
) -> anyhow::Result<()> {
    if path.exists() && !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }
    let rows = cache::parse(&std::fs::read_to_string(cache_path).unwrap_or_default());
    let mut project_roots =
        project_roots::parse(&std::fs::read_to_string(project_roots_path).unwrap_or_default());
    let global_ignore = merge::load_all(config_dir)?.ignore;

    ensure_project_registered(
        path,
        &mut project_roots,
        project_roots_path,
        &rows,
        is_tty,
        dry_run,
        read_line,
    )?;

    // Computed once for the whole run, not per-candidate (see
    // `decision_boundary`'s own doc comment) - `ensure_project_registered`
    // has already guaranteed `path` is covered by exactly one registered
    // project by this point (or, in a dry run, would be by a real run -
    // see its own doc comment for why the fallback below still matches).
    let boundary = decision_boundary(&rows, &project_roots, path);

    // A `BTreeSet` dedupes across all three sources (`--create`, the
    // walk, and any anchored decision-file pattern with no matching
    // watched-name/existing-target) before the final shallowest-first
    // ordering below.
    let mut candidates: std::collections::BTreeSet<PathBuf> = create.iter().cloned().collect();
    if path.is_dir() {
        candidates.extend(find_nested_candidates(
            path,
            &boundary,
            max_depth,
            &rows,
            &global_ignore,
        ));
    }
    candidates.extend(decision_file_anchored_candidates(&boundary));
    let mut candidates: Vec<PathBuf> = candidates.into_iter().collect();
    // Shallowest first (§6): an "every match of this name" answer for a
    // shallow candidate must already be reflected by the time a
    // same-named, `**`-covered deeper one is resolved.
    candidates.sort_by_key(|p| p.components().count());

    for candidate in &candidates {
        resolve_candidate(
            candidate,
            &boundary,
            create,
            data_dir,
            is_tty,
            dry_run,
            Mode::CONVERT,
            read_line,
        )?;
    }
    Ok(())
}

/// `ghostvolumes decide <path> [--max-depth N] --add <pattern> --deny
/// <pattern>` (Phase 4, `ai-work/tasks/convert-project-model.plan.md`,
/// revised per `ai-work/tasks/decide-walk-and-markers.plan.md`): same
/// walk-and-resolve engine as `convert` (same upfront registration,
/// same boundary resolution, same `find_nested_candidates` walk, same
/// per-candidate decision resolution and prompting) — the only
/// difference is the mode (`Mode::DECIDE`): an existing `+` is a no-op
/// instead of a re-materialize, and a freshly-answered "yes" only
/// records the decision, never touches the filesystem. No `--create`,
/// unlike `convert` — naming something explicitly to *materialize*
/// conflicts with `decide`'s whole contract.
///
/// Two things happen, in order:
/// 1. Each `--add`/`--deny` pattern is recorded verbatim (no anchoring
///    or broadening computed, since the human is directly specifying
///    the pattern) — the original Phase 4 "hand-author ahead of time"
///    behavior. Each pattern also doubles as `record_decision`'s own
///    search key for an existing pending `?` marker with that *exact*
///    pattern — an exact-string coincidence, not a re-derivation, but
///    it means hand-typing the same pattern a prior undecided
///    candidate left behind toggles that line in place.
/// 2. Candidates are gathered exactly like `convert`'s own (the
///    filesystem walk, plus `decision_file_anchored_candidates` —
///    anything an anchored `+`/`?` pattern implies that the walk alone
///    could never discover, most commonly because its target doesn't
///    exist on disk yet) and resolved the same way: anything step 1
///    already covers resolves via ordinary `decision::resolve`, no
///    separate filter needed; anything still undecided gets asked
///    about (or left pending, non-interactively).
#[allow(clippy::too_many_arguments)]
pub fn decide(
    path: &Path,
    add: &[String],
    deny: &[String],
    max_depth: Option<u32>,
    config_dir: &Path,
    cache_path: &Path,
    project_roots_path: &Path,
    data_dir: &Path,
) -> anyhow::Result<()> {
    let mut read_line = read_stdin_line;
    decide_with_io(
        path,
        add,
        deny,
        max_depth,
        config_dir,
        cache_path,
        project_roots_path,
        data_dir,
        std::io::stdin().is_terminal(),
        &mut read_line,
    )
}

#[allow(clippy::too_many_arguments)]
fn decide_with_io(
    path: &Path,
    add: &[String],
    deny: &[String],
    max_depth: Option<u32>,
    config_dir: &Path,
    cache_path: &Path,
    project_roots_path: &Path,
    data_dir: &Path,
    is_tty: bool,
    read_line: &mut impl FnMut() -> Option<String>,
) -> anyhow::Result<()> {
    if path.exists() && !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }
    let rows = cache::parse(&std::fs::read_to_string(cache_path).unwrap_or_default());
    let mut project_roots =
        project_roots::parse(&std::fs::read_to_string(project_roots_path).unwrap_or_default());
    let global_ignore = merge::load_all(config_dir)?.ignore;

    ensure_project_registered(
        path,
        &mut project_roots,
        project_roots_path,
        &rows,
        is_tty,
        false, // no --dry-run for decide yet - every call here is for real
        read_line,
    )?;

    let boundary = decision_boundary(&rows, &project_roots, path);
    trace(Verbosity::Debug, || {
        format!(
            "{}: resolved boundary -> {}",
            path.display(),
            boundary.display()
        )
    });

    if add.is_empty() && deny.is_empty() {
        trace(Verbosity::Debug, || {
            format!(
                "{}: no --add/--deny patterns given -> nothing to hand-author upfront",
                path.display()
            )
        });
    }

    // 1. Hand-authored patterns, verbatim, first - anything the walk
    // or marker-scan below encounters that matches resolves via
    // ordinary decision resolution with no separate logic needed.
    for pattern in add {
        record_decision(data_dir, &boundary, pattern, pattern, "+")?;
        println!("recorded: + {pattern} (at {})", boundary.display());
    }
    for pattern in deny {
        record_decision(data_dir, &boundary, pattern, pattern, "-")?;
        println!("recorded: - {pattern} (at {})", boundary.display());
    }

    // 2. Walk the filesystem exactly like convert, plus anything
    // implied by an anchored decision-file pattern the walk couldn't
    // discover on its own (its target doesn't exist on disk yet, or
    // it doesn't match any watched name at all) - same source, same
    // dedup, as `convert`'s own candidate gathering.
    let mut candidates: std::collections::BTreeSet<PathBuf> = std::collections::BTreeSet::new();
    if path.is_dir() {
        candidates.extend(find_nested_candidates(
            path,
            &boundary,
            max_depth,
            &rows,
            &global_ignore,
        ));
    }
    candidates.extend(decision_file_anchored_candidates(&boundary));
    let mut candidates: Vec<PathBuf> = candidates.into_iter().collect();
    candidates.sort_by_key(|p| p.components().count());

    for candidate in &candidates {
        resolve_candidate(
            candidate,
            &boundary,
            &[],
            data_dir,
            is_tty,
            false,
            Mode::DECIDE,
            read_line,
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::btrfs_scratch_dir;
    use std::os::unix::fs::MetadataExt;
    use tempfile::tempdir;

    fn empty_cache() -> tempfile::TempDir {
        tempdir().unwrap()
    }

    /// Writes `project` into the project-roots list directly, bypassing
    /// the interactive "register as a project?" ask - most tests below
    /// aren't testing that ask itself (see the dedicated
    /// `ensure_project_registered`/upfront-registration tests for that),
    /// just what happens once a project is already registered.
    fn register_project(dir: &tempfile::TempDir, project: &Path) {
        std::fs::write(roots_path(dir), format!("{}\n", project.display())).unwrap();
    }

    /// A `roots.d`-less config dir - `merge::load_all` treats a missing
    /// `roots.d` subdirectory as an empty config, so tests that don't
    /// care about `default-ignore` can point at this without creating
    /// anything on disk.
    fn config_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join("config")
    }

    fn cache_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(filenames::COMPILED_CACHE_FILE_NAME)
    }

    fn roots_path(dir: &tempfile::TempDir) -> PathBuf {
        dir.path().join(filenames::PROJECT_ROOTS_FILE_NAME)
    }

    #[test]
    fn parse_remember_answer_recognizes_yes_and_all() {
        assert!(matches!(
            parse_remember_answer("y"),
            RememberChoice::JustThisPath
        ));
        assert!(matches!(
            parse_remember_answer("Yes"),
            RememberChoice::JustThisPath
        ));
        assert!(matches!(
            parse_remember_answer("a"),
            RememberChoice::EveryMatchOfThisName
        ));
        assert!(matches!(
            parse_remember_answer("ALL"),
            RememberChoice::EveryMatchOfThisName
        ));
        assert!(matches!(parse_remember_answer(""), RememberChoice::Deny));
        assert!(matches!(parse_remember_answer("n"), RememberChoice::Deny));
        assert!(matches!(
            parse_remember_answer("garbage"),
            RememberChoice::Deny
        ));
    }

    #[test]
    fn ask_remember_denies_when_read_line_returns_nothing() {
        // Whether to ask at all (is_tty) is the caller's decision now -
        // ask_remember itself always asks; an absent answer (stdin
        // closed mid-prompt) still degrades to Deny, not a hang or panic.
        assert!(matches!(
            ask_remember(Path::new("/x"), Mode::CONVERT, || None),
            RememberChoice::Deny
        ));
    }

    #[test]
    fn confirm_override_defaults_to_false_when_not_a_tty() {
        assert!(!confirm_override(Path::new("/x"), false, || Some(
            "y".to_string()
        )));
    }

    #[test]
    fn confirm_override_true_only_for_yes() {
        assert!(confirm_override(Path::new("/x"), true, || Some(
            "y".to_string()
        )));
        assert!(!confirm_override(Path::new("/x"), true, || Some(
            "n".to_string()
        )));
        assert!(!confirm_override(Path::new("/x"), true, || None));
    }

    #[test]
    fn containing_dir_pattern_anchors_to_the_containing_directory() {
        assert_eq!(
            containing_dir_pattern(
                Path::new("/proj"),
                Path::new("/proj/packages/foo/node_modules"),
                "node_modules"
            ),
            "/packages/foo/**/node_modules"
        );
    }

    #[test]
    fn containing_dir_pattern_degrades_to_bare_double_star_at_the_boundary_itself() {
        assert_eq!(
            containing_dir_pattern(
                Path::new("/proj"),
                Path::new("/proj/node_modules"),
                "node_modules"
            ),
            "/**/node_modules"
        );
    }

    #[test]
    fn same_volume_true_for_paths_under_the_same_row() {
        let rows = [("/".to_string(), "node_modules".to_string())];
        assert!(same_volume(&rows, Path::new("/a"), Path::new("/a/b2")));
    }

    #[test]
    fn same_volume_false_across_a_more_specific_nested_row() {
        // The exact bug case: /a is on volume "/", but /a/b3/c1/d1 is on
        // the more specific, nested volume "/a/b3/c1" - even though one
        // path is textually an ancestor of the other.
        let rows = [
            ("/".to_string(), String::new()),
            ("/a/b1/c1".to_string(), String::new()),
            ("/a/b3/c1".to_string(), String::new()),
        ];
        assert!(!same_volume(
            &rows,
            Path::new("/a"),
            Path::new("/a/b3/c1/d1")
        ));
        assert!(!same_volume(
            &rows,
            Path::new("/a/b1"),
            Path::new("/a/b1/c1/d1")
        ));
    }

    #[test]
    fn same_volume_true_when_neither_path_matches_any_row() {
        assert!(same_volume(&[], Path::new("/a"), Path::new("/a/b")));
    }

    #[test]
    fn decision_boundary_falls_back_to_the_project_path_when_nothing_registered() {
        let boundary = decision_boundary(&[], &[], Path::new("/proj"));
        assert_eq!(boundary, PathBuf::from("/proj"));
    }

    #[test]
    fn decision_boundary_uses_a_covering_registered_project() {
        let boundary = decision_boundary(
            &[],
            &["/proj/packages/foo".to_string()],
            Path::new("/proj/packages/foo"),
        );
        assert_eq!(boundary, PathBuf::from("/proj/packages/foo"));
    }

    #[test]
    fn decision_boundary_merges_all_the_way_to_the_shallowest_of_a_nested_chain() {
        // The core nested-chain fix: every level from /a/b down is a
        // registered project, on the same (empty-rows) volume - the
        // boundary must be the *shallowest* (/a/b), not the deepest,
        // so resolve()'s own walk-up checks every level's decision file
        // instead of stopping at the first (deepest) one it finds.
        let project_roots = [
            "/a/b/c/d/e/f/g".to_string(),
            "/a/b/c/d/e".to_string(),
            "/a/b/c".to_string(),
            "/a/b".to_string(),
        ];
        let boundary = decision_boundary(&[], &project_roots, Path::new("/a/b/c/d/e/f/g"));
        assert_eq!(boundary, PathBuf::from("/a/b"));
    }

    #[test]
    fn decision_boundary_ignores_a_path_ancestor_project_on_a_different_volume() {
        let rows = [
            ("/".to_string(), String::new()),
            ("/a/b3/c1".to_string(), String::new()),
        ];
        // "/a" is registered but sits on volume "/", while
        // "/a/b3/c1/d1" sits on the more specific volume "/a/b3/c1" -
        // "/a" must not be treated as covering it.
        let boundary = decision_boundary(&rows, &["/a".to_string()], Path::new("/a/b3/c1/d1"));
        assert_eq!(boundary, PathBuf::from("/a/b3/c1/d1"));
    }

    #[test]
    fn nearest_ancestor_decision_file_finds_the_closest_one_within_the_limit() {
        let scratch = btrfs_scratch_dir();
        let parent = scratch.path().join("parent");
        let child = parent.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::write(parent.join(filenames::DECISION_FILE_NAME), "+ x\n").unwrap();

        let found = nearest_ancestor_decision_file(&child, Some(scratch.path()));
        assert_eq!(found, Some(parent));
    }

    #[test]
    fn nearest_ancestor_decision_file_none_when_nothing_exists_above() {
        let scratch = btrfs_scratch_dir();
        let child = scratch.path().join("child");
        std::fs::create_dir_all(&child).unwrap();

        assert_eq!(
            nearest_ancestor_decision_file(&child, Some(scratch.path())),
            None
        );
    }

    #[test]
    fn nearest_ancestor_decision_file_stops_at_the_limit_even_if_one_exists_further_up() {
        let scratch = btrfs_scratch_dir();
        let outside = scratch.path().join("outside");
        let inside = scratch.path().join("inside");
        let child = inside.join("child");
        std::fs::create_dir_all(&child).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        // A decision file that exists, but *above* the limit - must
        // never be found, same as the volume boundary being a hard cap
        // on how far up it's meaningful to look.
        std::fs::write(scratch.path().join(filenames::DECISION_FILE_NAME), "+ x\n").unwrap();

        assert_eq!(
            nearest_ancestor_decision_file(&child, Some(inside.as_path())),
            None
        );
    }

    #[test]
    fn ensure_project_registered_registers_on_an_empty_or_yes_answer() {
        let cache_dir = empty_cache();
        let mut project_roots = Vec::new();
        let mut answers = vec![String::new()].into_iter();
        ensure_project_registered(
            Path::new("/some/project"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            true,
            false,
            &mut move || answers.next(),
        )
        .unwrap();

        assert_eq!(project_roots, vec!["/some/project".to_string()]);
        assert_eq!(
            std::fs::read_to_string(roots_path(&cache_dir)).unwrap(),
            "/some/project\n"
        );
    }

    #[test]
    fn ensure_project_registered_aborts_on_an_explicit_no() {
        let cache_dir = empty_cache();
        let mut project_roots = Vec::new();
        let mut answers = vec!["n".to_string()].into_iter();
        let err = ensure_project_registered(
            Path::new("/some/project"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            true,
            false,
            &mut move || answers.next(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("not registered"));
        assert!(project_roots.is_empty());
    }

    #[test]
    fn ensure_project_registered_aborts_without_a_tty() {
        let cache_dir = empty_cache();
        let mut project_roots = Vec::new();
        let err = ensure_project_registered(
            Path::new("/some/project"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            false,
            false,
            &mut || panic!("must not ask at all without a TTY"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("not a registered project"));
    }

    #[test]
    fn ensure_project_registered_is_a_no_op_when_already_registered() {
        let cache_dir = empty_cache();
        let mut project_roots = vec!["/some/project".to_string()];
        // Would panic if it asked at all - already registered, nothing
        // to confirm.
        ensure_project_registered(
            Path::new("/some/project"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            false,
            false,
            &mut || panic!("must not ask when already registered"),
        )
        .unwrap();
    }

    #[test]
    fn ensure_project_registered_is_a_no_op_when_covered_by_a_shallower_same_volume_ancestor() {
        let cache_dir = empty_cache();
        let mut project_roots = vec!["/home/user".to_string()];
        // Would panic if it asked at all - a shallower registered
        // project already covers this exact nested path (the bug fixed
        // alongside `decision_boundary`: exact-match-only used to miss
        // this).
        ensure_project_registered(
            Path::new("/home/user/monorepo/packages/foo"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            false,
            false,
            &mut || panic!("must not ask when already covered by an ancestor project"),
        )
        .unwrap();
        assert_eq!(project_roots, vec!["/home/user".to_string()]);
    }

    #[test]
    fn ensure_project_registered_ignores_a_path_ancestor_project_on_a_different_volume() {
        let cache_dir = empty_cache();
        let rows = [
            ("/".to_string(), String::new()),
            ("/a/b3/c1".to_string(), String::new()),
        ];
        let mut project_roots = vec!["/a".to_string()];
        let mut answers = vec![String::new()].into_iter();
        // "/a" is a path-ancestor but a different volume - must not
        // count as coverage, so this proceeds to the plain register ask.
        ensure_project_registered(
            Path::new("/a/b3/c1/d1"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &rows,
            true,
            false,
            &mut move || answers.next(),
        )
        .unwrap();
        assert!(project_roots.contains(&"/a/b3/c1/d1".to_string()));
    }

    #[test]
    fn ensure_project_registered_warns_and_aborts_on_a_same_volume_nesting_conflict() {
        let cache_dir = empty_cache();
        let mut project_roots = vec!["/a/b1/c1/d1".to_string()];
        let mut answers = vec!["n".to_string()].into_iter();
        let err = ensure_project_registered(
            Path::new("/a/b1"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            true,
            false,
            &mut move || answers.next(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("nested projects"));
        // Declined - the conflicting child must survive untouched.
        assert_eq!(project_roots, vec!["/a/b1/c1/d1".to_string()]);
    }

    #[test]
    fn ensure_project_registered_confirmed_nesting_unregisters_the_child_and_registers_the_parent()
    {
        let cache_dir = empty_cache();
        std::fs::write(roots_path(&cache_dir), "/a/b1/c1/d1\n").unwrap();
        let mut project_roots = vec!["/a/b1/c1/d1".to_string()];
        let mut answers = vec!["y".to_string()].into_iter();
        ensure_project_registered(
            Path::new("/a/b1"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            true,
            false,
            &mut move || answers.next(),
        )
        .unwrap();

        assert_eq!(project_roots, vec!["/a/b1".to_string()]);
        assert_eq!(
            std::fs::read_to_string(roots_path(&cache_dir)).unwrap(),
            "/a/b1\n"
        );
    }

    #[test]
    fn ensure_project_registered_does_not_treat_a_different_volume_descendant_as_a_nesting_conflict()
     {
        let cache_dir = empty_cache();
        let rows = [
            ("/".to_string(), String::new()),
            ("/a/b1/c1".to_string(), String::new()),
        ];
        // /a/b1/c1/d1 is a path-descendant of /a/b1, but on a different,
        // more specific volume - registering /a/b1 must proceed as a
        // plain registration, not warn about nesting.
        let mut project_roots = vec!["/a/b1/c1/d1".to_string()];
        let mut answers = vec![String::new()].into_iter();
        ensure_project_registered(
            Path::new("/a/b1"),
            &mut project_roots,
            &roots_path(&cache_dir),
            &rows,
            true,
            false,
            &mut move || answers.next(),
        )
        .unwrap();

        assert!(project_roots.contains(&"/a/b1".to_string()));
        assert!(project_roots.contains(&"/a/b1/c1/d1".to_string()));
    }

    #[test]
    fn ensure_project_registered_warns_and_aborts_on_an_orphaned_ancestor_decision_file() {
        let scratch = btrfs_scratch_dir();
        let parent = scratch.path().join("parent");
        let project = parent.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(parent.join(filenames::DECISION_FILE_NAME), "+ x\n").unwrap();
        let cache_dir = empty_cache();
        let mut project_roots = Vec::new();
        let mut answers = vec!["n".to_string()].into_iter();

        let err = ensure_project_registered(
            &project,
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            true,
            false,
            &mut move || answers.next(),
        )
        .unwrap_err();

        assert!(err.to_string().contains(&parent.display().to_string()));
        assert!(project_roots.is_empty());
    }

    #[test]
    fn ensure_project_registered_confirmed_orphan_registers_anyway() {
        let scratch = btrfs_scratch_dir();
        let parent = scratch.path().join("parent");
        let project = parent.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(parent.join(filenames::DECISION_FILE_NAME), "+ x\n").unwrap();
        let cache_dir = empty_cache();
        let mut project_roots = Vec::new();
        let mut answers = vec!["y".to_string()].into_iter();

        ensure_project_registered(
            &project,
            &mut project_roots,
            &roots_path(&cache_dir),
            &[],
            true,
            false,
            &mut move || answers.next(),
        )
        .unwrap();

        assert_eq!(project_roots, vec![project.display().to_string()]);
    }

    #[test]
    fn convert_aborts_entirely_when_registration_is_declined() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();

        let mut answers = vec!["n".to_string()].into_iter();
        let err = convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("not registered"));
        assert!(!btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn create_resolves_a_target_with_no_cache_rows_at_all() {
        // --create bypasses the watched-name walk entirely - no
        // compiled.tsv row is needed for an explicitly-named target,
        // unlike anything discovered by the walk.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("totally-custom-name");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ totally-custom-name\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn converts_plain_directory_preserving_contents() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(target.join("pkg")).unwrap();
        std::fs::write(target.join("pkg/index.js"), b"module.exports = {}").unwrap();
        std::fs::write(target.join("top-level.txt"), b"hello").unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        // A pre-recorded `+` decision converts directly without asking
        // (see a_plus_decision_converts_directly_without_asking) - the
        // ask-then-convert gate for an *undecided* candidate would
        // otherwise default to "no" on this non-TTY test harness.
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read(target.join("pkg/index.js")).unwrap(),
            b"module.exports = {}"
        );
        assert_eq!(
            std::fs::read(target.join("top-level.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn no_leftover_backup_or_tmp_directories_after_success() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("f"), b"x").unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ target\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        let entries: Vec<_> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .filter(|name| name.to_string_lossy() != filenames::DECISION_FILE_NAME)
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("target")]);
    }

    #[test]
    fn empty_directory_converts_fine() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("build");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ build\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn creates_a_missing_path_directly_as_a_fresh_empty_subvolume() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("build");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ build\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn convert_proactively_creates_a_missing_anchored_plus_decision_for_an_unwatched_name() {
        // The bug this guards against: an anchored `+` decision for a
        // name that isn't even a watched name (so the walk could never
        // discover it on its own) must still get created, with no
        // --create needed - it's the persisted equivalent of one.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("venv2");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ /venv2\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn convert_ignores_a_wildcarded_anchored_pattern_for_proactive_creation() {
        // "/**/venv" has no single concrete location to create from -
        // must not be treated as an implied candidate at all.
        let scratch = btrfs_scratch_dir();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ /**/venv\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!scratch.path().join("venv").exists());
    }

    #[test]
    fn convert_dry_run_reports_would_create_for_a_missing_anchored_decision() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("venv2");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ /venv2\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true,
        )
        .unwrap();

        assert!(!target.exists());
    }

    #[test]
    fn decide_surfaces_a_missing_anchored_plus_decision_but_never_materializes_it() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("venv2");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ /venv2\n",
        )
        .unwrap();

        decide(
            scratch.path(),
            &[],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert!(!target.exists());
    }

    #[test]
    fn create_empty_tolerates_a_target_that_already_exists() {
        // Simulates the shim winning a race and creating the subvolume
        // just before convert's own materialize() call took the lock
        // (ai-work/tasks/atomic-file-io.plan.md §7) - create_empty
        // itself (unlike resolve_candidate's own upfront is_subvolume
        // check) must tolerate this rather than erroring, since the
        // desired end state already holds.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        btrfs::create_subvolume(scratch.path(), "node_modules").unwrap();

        create_empty(&target).unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn already_a_subvolume_with_an_existing_decision_is_a_silent_no_op() {
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "already").unwrap();
        let target = scratch.path().join("already");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ already\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ already\n"
        );
    }

    #[test]
    fn already_a_subvolume_without_a_tty_leaves_a_pending_marker_untouched_otherwise() {
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "already").unwrap();
        let target = scratch.path().join("already");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "? /already\n"
        );
    }

    #[test]
    fn already_a_subvolume_with_a_tty_defaults_to_yes_on_an_empty_answer() {
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "already").unwrap();
        let target = scratch.path().join("already");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        let mut answers = vec![String::new()].into_iter();
        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /already\n"
        );
    }

    #[test]
    fn already_a_subvolume_with_a_tty_records_a_decline_explicitly() {
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "already").unwrap();
        let target = scratch.path().join("already");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        let mut answers = vec!["n".to_string()].into_iter();
        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "- /already\n"
        );
    }

    #[test]
    fn already_a_subvolume_dry_run_reports_instead_of_asking() {
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "already").unwrap();
        let target = scratch.path().join("already");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true,
            true,
            &mut || panic!("dry run must never prompt"),
        )
        .unwrap();

        assert!(!scratch.path().join(filenames::DECISION_FILE_NAME).exists());
    }

    #[test]
    fn already_a_subvolume_is_a_candidate_even_when_its_name_is_not_watched() {
        // The walk itself, not just resolve_candidate: an unwatched
        // name that's already a real subvolume must still be
        // discovered, not silently skipped for lacking a watched name.
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "totally-unwatched-name").unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        let mut answers = vec!["y".to_string()].into_iter();
        convert_with_io(
            scratch.path(),
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /totally-unwatched-name\n"
        );
    }

    #[test]
    fn a_plain_directory_with_an_unwatched_name_is_still_never_a_candidate() {
        // Confirms the walk change is scoped to *subvolumes*
        // specifically - an ordinary plain directory with an unwatched
        // name must not suddenly become a candidate too.
        let scratch = btrfs_scratch_dir();
        std::fs::create_dir_all(scratch.path().join("totally-unwatched-plain-dir")).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        convert(
            scratch.path(),
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!scratch.path().join(filenames::DECISION_FILE_NAME).exists());
    }

    #[test]
    fn resolve_candidate_skips_an_undecided_existing_subvolume_when_decide_is_disabled() {
        // Direct unit test of the not-yet-wired-to-any-command
        // Mode { decide: false, convert: true } combination - see
        // Mode's own doc comment.
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "already").unwrap();
        let target = scratch.path().join("already");
        let data_dir = empty_cache();

        resolve_candidate(
            &target,
            scratch.path(),
            &[],
            data_dir.path(),
            true,
            false,
            Mode {
                decide: false,
                convert: true,
            },
            &mut || panic!("decide disabled - must never ask"),
        )
        .unwrap();

        assert!(!scratch.path().join(filenames::DECISION_FILE_NAME).exists());
    }

    #[test]
    fn refuses_plain_file_not_directory() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("not-a-dir");
        std::fs::write(&target, b"x").unwrap();
        let cache_dir = empty_cache();

        // The "not a directory" check on the *project* path itself
        // fires before project registration is even considered.
        let err = convert(
            &target,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }

    #[test]
    fn preserves_permissions_via_cp_a() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join(".venv");
        std::fs::create_dir_all(&target).unwrap();
        let script = target.join("run.sh");
        std::fs::write(&script, b"#!/bin/sh\necho hi").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ .venv\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        let mode = std::fs::metadata(target.join("run.sh")).unwrap().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn converted_subvolume_is_a_real_new_inode_not_the_old_directory() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("app");
        std::fs::create_dir_all(&target).unwrap();
        let original_ino = std::fs::metadata(&target).unwrap().ino();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ app\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        let new_ino = std::fs::metadata(&target).unwrap().ino();
        assert_ne!(original_ino, new_ino);
        assert_eq!(new_ino, 256);
    }

    #[test]
    fn a_plus_decision_converts_directly_without_asking() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        // Not a TTY in the test harness, so if this fell through to
        // asking it would answer "no" and record nothing - the
        // assertion below (subvolume created, decision file unchanged)
        // distinguishes "converted via the existing +" from "asked".
        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ node_modules\n"
        );
    }

    #[test]
    fn an_undecided_matching_candidate_converts_and_records_when_confirmed() {
        // The reordered ask-then-convert gate (§6 addendum): asking
        // happens *before* any filesystem change, and answering "yes"
        // both converts and records - the same one-answer-does-both
        // shape as before, just no longer already-converted by the
        // time the question is asked.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        let mut answers = vec!["y".to_string()].into_iter();
        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            // "y" ("just this path") anchors to the exact location,
            // unlike "a" ("all matches") which would record a bare
            // "node_modules" pattern instead - see anchored_pattern.
            "+ /node_modules\n"
        );
    }

    #[test]
    fn an_undecided_matching_candidate_is_left_alone_when_declined() {
        // Confirms the gate is a real approve/deny before any action,
        // not just a "remember for next time" afterthought: declining
        // must leave existing content completely untouched - and,
        // unlike the old "no" (silently forgotten), an explicit decline
        // at a real prompt is itself a deliberate decision worth
        // recording, so it isn't asked again next time.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("real-file.txt"), b"do not lose me").unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        let mut answers = vec!["n".to_string()].into_iter();
        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read(target.join("real-file.txt")).unwrap(),
            b"do not lose me"
        );
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "- /node_modules\n"
        );
    }

    #[test]
    fn a_matching_candidate_is_left_alone_without_a_tty_even_though_it_would_match() {
        // convert converts nothing for any undecided candidate when run
        // non-interactively (scripted, cron, CI) - naming it via
        // --create isn't enough on its own without someone there to
        // answer. It still leaves a trail, though: a pending "? <pattern>"
        // marker, the same mechanism the shim itself uses for an
        // undecided candidate it can't ask about either - so nothing
        // seen this run is silently forgotten.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "? /node_modules\n"
        );
    }

    #[test]
    fn a_repeated_non_tty_run_does_not_duplicate_the_pending_comment() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        for _ in 0..2 {
            convert(
                scratch.path(),
                std::slice::from_ref(&target),
                None,
                &config_path(&cache_dir),
                &cache_path(&cache_dir),
                &roots_path(&cache_dir),
                cache_dir.path(),
                false,
            )
            .unwrap();
        }

        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "? /node_modules\n"
        );
    }

    #[test]
    fn a_later_yes_toggles_an_existing_pending_marker_in_place() {
        // The bug this guards against: a candidate first seen
        // non-interactively (leaving "? /node_modules") must not end up
        // with *both* that pending marker and a real decision once one
        // is later recorded - the marker should become the decision,
        // same line, same position.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ /**/build\n? /node_modules\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        let mut answers = vec!["y".to_string()].into_iter();
        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /**/build\n+ /node_modules\n"
        );
    }

    #[test]
    fn a_later_no_toggles_an_existing_pending_marker_into_a_deny() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ /**/build\n? /node_modules\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        let mut answers = vec!["n".to_string()].into_iter();
        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /**/build\n- /node_modules\n"
        );
    }

    #[test]
    fn a_later_all_matches_answer_lands_the_broader_pattern_in_the_marker_s_own_spot() {
        // "a" records a *different*, broader pattern than the anchored
        // pending marker - still an in-place swap, not a remove-then-
        // append-at-the-end: surrounding lines (including human
        // comments) keep their exact position and order.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ /**/build\n# human comment\n? /node_modules\n# comment2\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        let mut answers = vec!["a".to_string()].into_iter();
        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /**/build\n# human comment\n+ /**/node_modules\n# comment2\n"
        );
    }

    #[test]
    fn a_minus_decision_found_via_the_walk_is_skipped_silently() {
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let target = project.join("vendor");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(project.join(filenames::DECISION_FILE_NAME), "- vendor\n").unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, &project);

        // Convert is pointed at the *project* directory, not `vendor`
        // itself directly - vendor is only found via the recursive
        // walk, so the `-` decision is respected with no override
        // prompt at all.
        write_cache_rows(&cache_path(&cache_dir), &[(&project, "vendor")]);
        convert(
            &project,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert!(target.is_dir());
    }

    #[test]
    fn a_minus_decision_on_an_explicit_create_target_is_not_overridden_without_a_tty() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("vendor");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "- vendor\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        // Named explicitly via --create - a deliberate override attempt
        // - but no TTY in the test harness, so it must stay declined.
        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn nested_candidate_under_a_directory_argument_is_found_and_converted() {
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let nested = project.join("packages/foo/node_modules");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, &project);
        write_cache_rows(&cache_path(&cache_dir), &[(&project, "node_modules")]);

        convert(
            &project,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&nested).unwrap());
    }

    #[test]
    fn a_populated_project_argument_is_never_a_candidate_but_nested_matches_still_convert() {
        // The bug this guards against: pointing `convert` at an
        // already-populated project (e.g. to bootstrap decisions for
        // what's inside it) must not fold the whole project itself into
        // a subvolume - `project` is never added to `candidates` at all
        // now (see the module doc comment), regardless of its own name
        // or content, unlike the nested "node_modules" match.
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let nested = project.join("packages/foo/node_modules");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(project.join("README.md"), b"real project content").unwrap();
        std::fs::write(
            project.join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, &project);
        write_cache_rows(&cache_path(&cache_dir), &[(&project, "node_modules")]);

        convert(
            &project,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&project).unwrap());
        assert_eq!(
            std::fs::read(project.join("README.md")).unwrap(),
            b"real project content"
        );
        assert!(btrfs::is_subvolume(&nested).unwrap());
    }

    #[test]
    fn an_empty_project_argument_is_never_converted_either() {
        let scratch = btrfs_scratch_dir();
        let empty_project = scratch.path().join("brand-new-project");
        std::fs::create_dir_all(&empty_project).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, &empty_project);

        convert(
            &empty_project,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&empty_project).unwrap());
        assert!(empty_project.is_dir());
    }

    #[test]
    fn a_not_yet_existing_project_argument_is_not_created_either() {
        let scratch = btrfs_scratch_dir();
        let missing_project = scratch.path().join("brand-new-project");
        let cache_dir = empty_cache();
        register_project(&cache_dir, &missing_project);

        convert(
            &missing_project,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!missing_project.exists());
    }

    #[test]
    fn does_not_descend_into_a_matched_nested_candidate() {
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let outer = project.join("node_modules");
        let inner = outer.join("target"); // nested match inside a match
        std::fs::create_dir_all(&inner).unwrap();
        std::fs::write(
            project.join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n+ target\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, &project);
        write_cache_rows(
            &cache_path(&cache_dir),
            &[(&project, "node_modules"), (&project, "target")],
        );

        convert(
            &project,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&outer).unwrap());
        // Never walked into `outer` looking for `inner` - still plain.
        assert!(!btrfs::is_subvolume(&inner).unwrap());
    }

    fn write_cache_rows(cache_path: &Path, rows: &[(&Path, &str)]) {
        let mut text = String::new();
        for (prefix, name) in rows {
            text.push_str(&format!("{}\t{name}\n", prefix.display()));
        }
        std::fs::write(cache_path, text).unwrap();
    }

    #[test]
    fn dry_run_leaves_an_undecided_candidate_untouched_and_never_prompts() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true, // dry_run
            true, // is_tty - would ask if this weren't a dry run
            &mut || panic!("dry run must never prompt"),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert!(!scratch.path().join(filenames::DECISION_FILE_NAME).exists());
    }

    #[test]
    fn dry_run_leaves_a_not_yet_existing_plus_decision_target_uncreated() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("build");
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ build\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true,
        )
        .unwrap();

        assert!(!target.exists());
    }

    #[test]
    fn dry_run_leaves_an_existing_plus_decision_target_unconverted() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("real-file.txt"), b"do not touch me").unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();

        convert(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read(target.join("real-file.txt")).unwrap(),
            b"do not touch me"
        );
    }

    #[test]
    fn dry_run_never_registers_an_uncovered_project() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = empty_cache();

        convert_with_io(
            scratch.path(),
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true, // dry_run
            true, // is_tty - would ask if this weren't a dry run
            &mut || panic!("dry run must never prompt, even to register the project"),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(roots_path(&cache_dir)).unwrap_or_default(),
            ""
        );
    }

    #[test]
    fn dry_run_never_prompts_to_override_an_explicit_minus_decision() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("vendor");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "- vendor\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        convert_with_io(
            scratch.path(),
            std::slice::from_ref(&target),
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true, // dry_run
            true, // is_tty - would ask to override if this weren't a dry run
            &mut || panic!("dry run must never prompt, even to confirm an override"),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "- vendor\n"
        );
    }

    #[test]
    fn convert_merges_decisions_from_a_shallower_registered_parent_project() {
        // End-to-end version of decision_boundary_merges_all_the_way_to_the_shallowest_of_a_nested_chain:
        // both /a/b (outer) and /a/b/c (inner, the actual convert
        // argument) are registered projects on the same volume - only
        // the outer one has a decision, and it must still govern.
        let scratch = btrfs_scratch_dir();
        let outer = scratch.path().join("a/b");
        let inner = outer.join("c");
        let target = inner.join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        std::fs::write(
            roots_path(&cache_dir),
            format!("{}\n{}\n", outer.display(), inner.display()),
        )
        .unwrap();
        // A single row at `outer` keeps both registered projects on the
        // same volume - a row at `inner` instead would make `outer`
        // volume-`None` and `inner` volume-`Some(inner)`, an unrelated
        // false mismatch this test isn't trying to exercise.
        write_cache_rows(&cache_path(&cache_dir), &[(&outer, "node_modules")]);
        std::fs::write(
            outer.join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();

        convert(
            &inner,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn convert_does_not_merge_decisions_across_a_volume_boundary_even_when_paths_nest() {
        // The complementary case: a path-ancestor project on a
        // *different* volume must not be treated as covering - the
        // inner path gets its own, separate registration instead of
        // silently inheriting (or being blocked by) the outer one.
        let scratch = btrfs_scratch_dir();
        let outer = scratch.path().join("a");
        let inner = outer.join("b3/c1/d1");
        let target = inner.join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        std::fs::write(roots_path(&cache_dir), format!("{}\n", outer.display())).unwrap();
        // Two distinct rows: `outer` is one volume, `outer/b3/c1` is a
        // separate, more specific one that `inner` actually falls under.
        write_cache_rows(
            &cache_path(&cache_dir),
            &[
                (&outer, "node_modules"),
                (&outer.join("b3/c1"), "node_modules"),
            ],
        );
        // A decision at `outer` that would (wrongly) approve this if
        // volume weren't taken into account.
        std::fs::write(
            outer.join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();

        // First answer: the plain "register as a project?" ask (empty
        // = yes) - `inner` isn't covered by `outer` (different
        // volume), so this is a fresh registration, not a no-op.
        // Second answer: "convert (and remember)?" for the discovered
        // node_modules candidate itself.
        let mut answers = vec![String::new(), "y".to_string()].into_iter();
        convert_with_io(
            &inner,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        // Registered as its own, separate project - not silently
        // resolved (or blocked) via the unrelated outer project.
        assert_eq!(
            std::fs::read_to_string(roots_path(&cache_dir)).unwrap(),
            format!("{}\n{}\n", outer.display(), inner.display())
        );
        assert!(btrfs::is_subvolume(&target).unwrap());
        // Anchored to `inner` (its own decision, "y" = just this path) -
        // not the unrelated "+ node_modules" sitting at `outer`.
        assert_eq!(
            std::fs::read_to_string(inner.join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /node_modules\n"
        );
    }

    #[test]
    fn a_global_default_ignore_pattern_skips_a_directory_even_though_it_matches_a_watched_name() {
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let target = project.join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, &project);
        write_cache_rows(&cache_path(&cache_dir), &[(&project, "node_modules")]);
        let config_dir = config_path(&cache_dir);
        std::fs::create_dir_all(config_dir.join(filenames::ROOTS_D_DIR)).unwrap();
        std::fs::write(
            config_dir.join(filenames::ROOTS_D_DIR).join("00-test.toml"),
            r#"default-ignore = ["node_modules"]"#,
        )
        .unwrap();

        convert(
            &project,
            &[],
            None,
            &config_dir,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert!(target.is_dir());
    }

    #[test]
    fn a_volume_root_s_ghostvolumes_ignore_file_skips_a_directory_even_though_it_matches_a_watched_name()
     {
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let target = project.join("vendor");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, &project);
        // The row's own prefix - the configured *volume* root - is the
        // broader `scratch` dir, not `project` itself, so its own
        // `.ghostvolumes-ignore` lives there too, distinct from the
        // project root.
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "vendor")]);
        std::fs::write(scratch.path().join(filenames::IGNORE_FILE_NAME), "vendor\n").unwrap();

        convert(
            &project,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert!(target.is_dir());
    }

    #[test]
    fn a_project_root_s_ghostvolumes_ignore_file_skips_a_directory_even_though_it_matches_a_watched_name()
     {
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let target = project.join("only_locally_ignored");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, &project);
        // Row prefix is the broader `scratch` dir, which has no
        // `.ghostvolumes-ignore` of its own - only the project root's
        // file (a distinct, separate tier) mentions this name.
        write_cache_rows(
            &cache_path(&cache_dir),
            &[(scratch.path(), "only_locally_ignored")],
        );
        std::fs::write(
            project.join(filenames::IGNORE_FILE_NAME),
            "only_locally_ignored\n",
        )
        .unwrap();

        convert(
            &project,
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert!(target.is_dir());
    }

    #[test]
    fn decide_records_a_plus_and_a_minus_decision_verbatim() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        decide(
            scratch.path(),
            &["node_modules".to_string()],
            &["vendor".to_string()],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ node_modules\n- vendor\n"
        );
    }

    #[test]
    fn decide_uses_patterns_verbatim_no_anchoring_or_broadening() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        decide(
            scratch.path(),
            &["/packages/*/**/node_modules".to_string()],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /packages/*/**/node_modules\n"
        );
    }

    #[test]
    fn decide_toggles_an_existing_pending_marker_with_the_exact_same_pattern_in_place() {
        let scratch = btrfs_scratch_dir();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "# a comment\n? node_modules\n# another comment\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        decide(
            scratch.path(),
            &["node_modules".to_string()],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "# a comment\n+ node_modules\n# another comment\n"
        );
    }

    #[test]
    fn decide_never_creates_a_subvolume_or_touches_anything_besides_the_decision_file() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("real-file.txt"), b"hello").unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        decide(
            scratch.path(),
            &["node_modules".to_string()],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read(target.join("real-file.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn decide_with_no_patterns_at_all_just_registers_the_project() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = empty_cache();

        let mut answers = vec![String::new()].into_iter();
        decide_with_io(
            scratch.path(),
            &[],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(roots_path(&cache_dir)).unwrap(),
            format!("{}\n", scratch.path().display())
        );
        assert!(!scratch.path().join(filenames::DECISION_FILE_NAME).exists());
    }

    #[test]
    fn decide_aborts_without_a_tty_when_the_project_is_not_yet_registered() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = empty_cache();

        let err = decide_with_io(
            scratch.path(),
            &["node_modules".to_string()],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            false,
            &mut || panic!("must not ask at all without a TTY"),
        )
        .unwrap_err();

        assert!(err.to_string().contains("not a registered project"));
        assert!(!scratch.path().join(filenames::DECISION_FILE_NAME).exists());
    }

    #[test]
    fn decide_refuses_a_plain_file_argument() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("not-a-dir");
        std::fs::write(&target, b"x").unwrap();
        let cache_dir = empty_cache();

        let err = decide(
            &target,
            &["x".to_string()],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap_err();

        assert!(err.to_string().contains("not a directory"));
    }

    #[test]
    fn decide_writes_to_a_shallower_already_registered_covering_project_not_the_argument_itself() {
        // Same-volume nested-project reasoning as
        // convert_merges_decisions_from_a_shallower_registered_parent_project:
        // both /a/b (outer) and /a/b/c (inner, the decide argument) are
        // registered on the same volume - the decision must land in the
        // outer project's own file, not create a new one at the inner path.
        let scratch = btrfs_scratch_dir();
        let outer = scratch.path().join("a/b");
        let inner = outer.join("c");
        std::fs::create_dir_all(&inner).unwrap();
        let cache_dir = empty_cache();
        std::fs::write(
            roots_path(&cache_dir),
            format!("{}\n{}\n", outer.display(), inner.display()),
        )
        .unwrap();

        decide(
            &inner,
            &["node_modules".to_string()],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(outer.join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ node_modules\n"
        );
        assert!(!inner.join(filenames::DECISION_FILE_NAME).exists());
    }

    #[test]
    fn decide_walk_discovers_an_undecided_candidate_and_records_without_materializing() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        let mut answers = vec!["y".to_string()].into_iter();
        decide_with_io(
            scratch.path(),
            &[],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /node_modules\n"
        );
    }

    #[test]
    fn decide_walk_leaves_a_pending_marker_without_a_tty() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        decide(
            scratch.path(),
            &[],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "? /node_modules\n"
        );
    }

    #[test]
    fn decide_walk_finding_an_existing_plus_decision_is_a_no_op_not_a_materialize() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("real.txt"), b"content").unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();

        decide(
            scratch.path(),
            &[],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert_eq!(std::fs::read(target.join("real.txt")).unwrap(), b"content");
    }

    #[test]
    fn decide_resolves_an_orphaned_pending_marker_whose_candidate_does_not_exist_on_disk() {
        let scratch = btrfs_scratch_dir();
        // "build" never actually exists on disk - the filesystem walk
        // could never discover it; only the marker scan can.
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "? /build\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        let mut answers = vec!["y".to_string()].into_iter();
        decide_with_io(
            scratch.path(),
            &[],
            &[],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true,
            &mut move || answers.next(),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "+ /build\n"
        );
        assert!(!scratch.path().join("build").exists());
    }

    #[test]
    fn decide_orphaned_marker_matching_a_deny_pattern_resolves_without_asking() {
        let scratch = btrfs_scratch_dir();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "? /build\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        register_project(&cache_dir, scratch.path());

        decide_with_io(
            scratch.path(),
            &[],
            &["/build".to_string()],
            None,
            &config_path(&cache_dir),
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
            true,
            &mut || panic!("must not ask - the --deny pattern already resolves this exact marker"),
        )
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(scratch.path().join(filenames::DECISION_FILE_NAME)).unwrap(),
            "- /build\n"
        );
    }

    #[test]
    fn materialize_blocks_while_the_boundary_lock_is_held_then_succeeds() {
        // The CLI-side half of the shim-vs-convert directory-swap lock
        // (ai-work/tasks/atomic-file-io.plan.md §6): unlike the shim's
        // own non-blocking try_lock, materialize blocks - fine here,
        // since convert is an explicit, occasional, human-run command.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        let cache_dir = empty_cache();
        let boundary = scratch.path().to_path_buf();

        let lock_path = crate::lock::boundary_lock_path(
            &cache_dir.path().join(filenames::LOCKS_DIR),
            &boundary,
        );
        let lock_file = crate::lock::open_lock_file(&lock_path).unwrap();
        lock_file.lock().unwrap();

        let target_thread = target.clone();
        let boundary_thread = boundary.clone();
        let data_dir = cache_dir.path().to_path_buf();
        let handle = std::thread::spawn(move || {
            materialize(&target_thread, &boundary_thread, &data_dir).unwrap();
        });

        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !handle.is_finished(),
            "materialize should still be blocked while the lock is held"
        );

        drop(lock_file);
        handle.join().unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
    }
}
