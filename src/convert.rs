//! `ghostvolumes convert <path>` (ai-work/tasks/decision-model.plan.md
//! §6): a recursive walk-and-resolve, not just a single-leaf migration.
//! `<path>` is a starting point: every candidate under it matching a
//! watched name under a configured root gets resolved too (reusing
//! `discover`'s tree-walking conventions — skip `.git`, optional
//! `--max-depth`, never descend into a match).
//!
//! `<path>` itself still needs the *same* "is this actually a
//! recognized watched name" signal every nested candidate already
//! requires before it self-materializes — naming a path explicitly on
//! the command line isn't, by itself, enough justification to fold it
//! (e.g. a project root someone pointed `convert` at to bootstrap
//! decisions for what's *inside* it) into a subvolume. No exemption for
//! "but it's empty" either: pre-creation (created directly as a fresh,
//! empty subvolume, replacing cd-hook's old proactive pre-creation) was
//! only ever meaningful for names already configured as watched — a
//! not-yet-existing directory with an unrecognized name isn't a
//! build-artifact-in-waiting, it's just a name nobody told this tool to
//! care about, empty or not. `<path>` is still scanned as the walk's
//! starting point either way — only its own self-materialization is
//! gated, uniformly, on the exact same check nested candidates already
//! pass through.
//!
//! Each candidate, resolved shallowest-first (so an "every match of
//! this name" answer for a shallow candidate is already reflected by
//! the time a same-named, `**`-covered deeper one is resolved, instead
//! of asking twice):
//! - Already a subvolume → skip silently.
//! - A `+` decision already exists → convert directly, no asking.
//! - A `-` decision already exists → skip silently, unless this exact
//!   candidate is the literal `<path>` argument (a deliberate override
//!   attempt), in which case confirm before proceeding.
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
//!
//! Whenever a decision actually gets recorded above, the resolved
//! project root also gets silently, idempotently registered into the
//! project-roots list (§3) — the same effect `projects register` has,
//! but free.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{btrfs, cache, decision, filenames, project_roots, projects};

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

/// Asks the "convert (and remember)?" question and parses the answer -
/// always actually asks, unlike `confirm_override`. Whether to ask at
/// all based on `is_tty` is the caller's call (`ask_and_maybe_convert`),
/// since a non-interactive run needs a different fallback (a pending
/// marker, not a recorded deny) than an interactive "no" does - a
/// distinction this function doesn't need to know about.
fn ask_remember(candidate: &Path, read_line: impl FnOnce() -> Option<String>) -> RememberChoice {
    eprint!(
        "Convert (and remember this decision for) {}? [y]es, just this path / [a]ll matches of this name / [N]o: ",
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

/// The decision-file walk-up's stopping boundary for `candidate` (§3):
/// the longest ancestor-or-self prefix among `compiled.tsv`'s own rows
/// and the registered project-roots list, whichever is more specific —
/// same computation as the shim's `walkup_boundary`, using
/// `crate::cache::longest_matching_prefix` (the CLI-side path to the
/// same shared `cache_core` logic) instead of the shim's own module
/// path. Falls back to `top_level_path` (the literal `convert`
/// argument, always an ancestor-or-self of every candidate this run
/// resolves) rather than `candidate`'s own parent — `convert` was
/// explicitly pointed at that path as the project root, so that's the
/// more meaningful floor than an arbitrary nearer directory.
fn walkup_boundary(
    rows: &[(String, String)],
    project_roots: &[String],
    top_level_path: &Path,
    candidate: &Path,
) -> PathBuf {
    let combined: Vec<(String, String)> = rows
        .iter()
        .cloned()
        .chain(
            project_roots
                .iter()
                .map(|root| (root.clone(), String::new())),
        )
        .collect();
    if let Some(prefix) = cache::longest_matching_prefix(&combined, candidate) {
        return PathBuf::from(prefix);
    }
    // `top_level_path` is only a valid boundary for a candidate found
    // *under* it (always an ancestor-or-self of `candidate.parent()`
    // in that case). When `candidate` *is* `top_level_path` itself -
    // the totally-fresh, nothing-registered-anywhere-yet case - it
    // can't be its own boundary (`resolve()` requires the boundary be
    // at or above `candidate.parent()`), so fall back one level
    // further, to `candidate`'s own immediate parent.
    if candidate != top_level_path {
        return top_level_path.to_path_buf();
    }
    candidate.parent().unwrap_or(candidate).to_path_buf()
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
    project_roots: &mut Vec<String>,
    project_roots_path: &Path,
    data_dir: &Path,
    is_tty: bool,
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
    let choice = ask_remember(candidate, read_line);
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
    materialize(candidate, boundary, data_dir)?;
    record_decision(data_dir, boundary, &anchored, &pattern, "+")?;
    register_project_root(boundary, project_roots, project_roots_path)
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

/// Walks `start`'s subtree (skipping `.git`, never descending into a
/// match — same conventions as `discover::walk`), collecting every
/// directory whose name is watched under a configured root at its own
/// location (`cache::names_for`, which is root-scoped, so this
/// naturally excludes anything outside every configured root). `start`
/// itself is not included — the caller already knows to treat it as a
/// candidate unconditionally.
fn find_nested_candidates(
    start: &Path,
    max_depth: Option<u32>,
    rows: &[(String, String)],
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    find_nested_candidates_inner(start, max_depth, rows, 0, &mut out);
    out
}

fn find_nested_candidates_inner(
    dir: &Path,
    max_depth: Option<u32>,
    rows: &[(String, String)],
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
        if name_str == ".git" {
            continue;
        }
        let path = entry.path();
        if names.contains(name_str.as_ref()) {
            out.push(path);
            continue; // never descend into a match
        }
        find_nested_candidates_inner(&path, max_depth, rows, depth + 1, out);
    }
}

/// `true` if `candidate`'s own name is a watched name at its own
/// location — the same check `find_nested_candidates_inner` already
/// applies to every nested candidate, reused here so `<path>` itself
/// gets no special exemption once it has real content (see the module
/// doc comment).
fn matches_a_watched_name(candidate: &Path, rows: &[(String, String)]) -> bool {
    let Some(parent) = candidate.parent() else {
        return false;
    };
    let name = candidate
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    cache::names_for(rows, parent).contains(&name)
}

// Eight independently-necessary pieces of context (candidate identity,
// the shared config/state every candidate in this run reads or
// mutates, and the injectable TTY/stdin pair) - no natural subgroup
// that wouldn't just be a same-size struct moving the count elsewhere.
#[allow(clippy::too_many_arguments)]
fn resolve_candidate(
    candidate: &Path,
    top_level_path: &Path,
    rows: &[(String, String)],
    project_roots: &mut Vec<String>,
    project_roots_path: &Path,
    data_dir: &Path,
    is_tty: bool,
    mut read_line: &mut impl FnMut() -> Option<String>,
) -> anyhow::Result<()> {
    if btrfs::is_subvolume(candidate).unwrap_or(false) {
        return Ok(());
    }

    let boundary = walkup_boundary(rows, project_roots, top_level_path, candidate);
    let existing_decision = decision::resolve(
        candidate,
        &boundary,
        filenames::DECISION_FILE_NAME,
        read_decision_file,
    );

    match existing_decision {
        Some(true) => materialize(candidate, &boundary, data_dir),
        Some(false) => {
            if candidate != top_level_path {
                return Ok(()); // found via the walk, not named explicitly - skip silently
            }
            if !confirm_override(candidate, is_tty, &mut read_line) {
                return Ok(());
            }
            ask_and_maybe_convert(
                candidate,
                &boundary,
                project_roots,
                project_roots_path,
                data_dir,
                is_tty,
                read_line,
            )
        }
        None => {
            if candidate == top_level_path && !matches_a_watched_name(candidate, rows) {
                // Unrecognized name - not fair game just because it was
                // named explicitly (see module doc comment), regardless
                // of whether it exists yet. Still scanned as the walk's
                // starting point; only its own self-materialization is
                // skipped.
                return Ok(());
            }
            ask_and_maybe_convert(
                candidate,
                &boundary,
                project_roots,
                project_roots_path,
                data_dir,
                is_tty,
                read_line,
            )
        }
    }
}

/// Real entry point: real TTY/stdin. See `convert_with_io` for the
/// testable core — now that the ask-then-convert gate decides whether
/// a candidate is touched at all (not just whether the decision gets
/// persisted), it needs the same injectable-stdin treatment as
/// `reload_with_validator`/`unregister_with_io` elsewhere in this crate.
pub fn convert(
    path: &Path,
    max_depth: Option<u32>,
    cache_path: &Path,
    project_roots_path: &Path,
    data_dir: &Path,
) -> anyhow::Result<()> {
    let mut read_line = read_stdin_line;
    convert_with_io(
        path,
        max_depth,
        cache_path,
        project_roots_path,
        data_dir,
        std::io::stdin().is_terminal(),
        &mut read_line,
    )
}

fn convert_with_io(
    path: &Path,
    max_depth: Option<u32>,
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

    let mut candidates = vec![path.to_path_buf()];
    if path.is_dir() {
        candidates.extend(find_nested_candidates(path, max_depth, &rows));
    }
    // Shallowest first (§6): an "every match of this name" answer for a
    // shallow candidate must already be reflected by the time a
    // same-named, `**`-covered deeper one is resolved.
    candidates.sort_by_key(|p| p.components().count());

    for candidate in &candidates {
        resolve_candidate(
            candidate,
            path,
            &rows,
            &mut project_roots,
            project_roots_path,
            data_dir,
            is_tty,
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
            ask_remember(Path::new("/x"), || None),
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
    fn walkup_boundary_falls_back_to_the_top_level_path_when_nothing_registered() {
        let boundary = walkup_boundary(
            &[],
            &[],
            Path::new("/proj"),
            Path::new("/proj/packages/foo/node_modules"),
        );
        assert_eq!(boundary, PathBuf::from("/proj"));
    }

    #[test]
    fn walkup_boundary_prefers_a_registered_project_root_over_the_broader_top_level_path() {
        let boundary = walkup_boundary(
            &[("/".to_string(), "node_modules".to_string())],
            &["/proj/packages/foo".to_string()],
            Path::new("/proj"),
            Path::new("/proj/packages/foo/node_modules"),
        );
        assert_eq!(boundary, PathBuf::from("/proj/packages/foo"));
    }

    #[test]
    fn walkup_boundary_falls_back_one_level_further_when_the_candidate_is_the_top_level_path_itself()
     {
        // `top_level_path` can't be its own boundary here (`resolve()`
        // requires the boundary be at or above `candidate`'s *parent*)
        // - this is the exact scenario that used to make
        // `a_minus_decision_on_the_literal_argument_is_not_overridden_without_a_tty`
        // fail: an empty cache and no registered project roots at all,
        // with `<path>` named directly as the candidate.
        let boundary = walkup_boundary(
            &[],
            &[],
            Path::new("/proj/vendor"),
            Path::new("/proj/vendor"),
        );
        assert_eq!(boundary, PathBuf::from("/proj"));
    }

    #[test]
    fn converts_plain_directory_preserving_contents() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(target.join("pkg")).unwrap();
        std::fs::write(target.join("pkg/index.js"), b"module.exports = {}").unwrap();
        std::fs::write(target.join("top-level.txt"), b"hello").unwrap();
        let cache_dir = empty_cache();
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
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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

        convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        let entries: Vec<_> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("target")]);
    }

    #[test]
    fn empty_directory_converts_fine() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("build");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ build\n",
        )
        .unwrap();

        convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn creates_a_missing_path_directly_as_a_fresh_empty_subvolume() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("build");
        let cache_dir = empty_cache();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ build\n",
        )
        .unwrap();

        convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
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
    fn already_a_subvolume_is_a_silent_no_op() {
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "already").unwrap();
        let target = scratch.path().join("already");
        let cache_dir = empty_cache();

        convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn refuses_plain_file_not_directory() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("not-a-dir");
        std::fs::write(&target, b"x").unwrap();
        let cache_dir = empty_cache();

        let err = convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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

        convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "+ app\n",
        )
        .unwrap();

        convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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

        // Not a TTY in the test harness, so if this fell through to
        // asking it would answer "no" and record nothing - the
        // assertion below (subvolume created, decision file unchanged)
        // distinguishes "converted via the existing +" from "asked".
        convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        let mut answers = vec!["y".to_string()].into_iter();
        convert_with_io(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        let mut answers = vec!["n".to_string()].into_iter();
        convert_with_io(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
        // non-interactively (scripted, cron, CI) - matching a watched
        // name isn't enough on its own without someone there to answer.
        // It still leaves a trail, though: a pending "# <pattern>"
        // comment, the same mechanism the shim itself uses for an
        // undecided candidate it can't ask about either - so nothing
        // seen this run is silently forgotten.
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        convert(
            &target,
            None,
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
    fn a_repeated_non_tty_run_does_not_duplicate_the_pending_comment() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(&target).unwrap();
        let cache_dir = empty_cache();
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        for _ in 0..2 {
            convert(
                &target,
                None,
                &cache_path(&cache_dir),
                &roots_path(&cache_dir),
                cache_dir.path(),
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
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        let mut answers = vec!["y".to_string()].into_iter();
        convert_with_io(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        let mut answers = vec!["n".to_string()].into_iter();
        convert_with_io(
            &target,
            None,
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
        write_cache_rows(&cache_path(&cache_dir), &[(scratch.path(), "node_modules")]);

        let mut answers = vec!["a".to_string()].into_iter();
        convert_with_io(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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

        // Convert is pointed at the *project* directory, not `vendor`
        // itself directly - vendor is only found via the recursive
        // walk, so the `-` decision is respected with no override
        // prompt at all.
        write_cache_rows(&cache_path(&cache_dir), &[(&project, "vendor")]);
        convert(
            &project,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&target).unwrap());
        assert!(target.is_dir());
    }

    #[test]
    fn a_minus_decision_on_the_literal_argument_is_not_overridden_without_a_tty() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("vendor");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(
            scratch.path().join(filenames::DECISION_FILE_NAME),
            "- vendor\n",
        )
        .unwrap();
        let cache_dir = empty_cache();

        // Named explicitly as <path> - a deliberate override attempt -
        // but no TTY in the test harness, so it must stay declined.
        convert(
            &target,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
        write_cache_rows(&cache_path(&cache_dir), &[(&project, "node_modules")]);

        convert(
            &project,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&nested).unwrap());
    }

    #[test]
    fn a_populated_project_root_argument_is_left_alone_but_still_walked_for_nested_matches() {
        // The bug this guards against: pointing `convert` at an
        // already-populated project root (e.g. to bootstrap decisions
        // for what's inside it) must not fold the whole project itself
        // into a subvolume just because it was named explicitly -
        // "project" isn't a recognized build-artifact name anywhere,
        // unlike the nested "node_modules" match.
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let nested = project.join("packages/foo/node_modules");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(project.join("README.md"), b"real project content").unwrap();
        // A decision for "node_modules" specifically - doesn't match
        // "project"'s own name, so it has no bearing on whether the
        // root itself gets asked/converted (it shouldn't be).
        std::fs::write(
            project.join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();
        let cache_dir = empty_cache();
        write_cache_rows(&cache_path(&cache_dir), &[(&project, "node_modules")]);

        convert(
            &project,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
    fn an_empty_or_missing_top_level_path_with_an_unrecognized_name_is_left_alone() {
        // No exemption for "but it's empty" - pre-creation only ever
        // makes sense for a name already configured as watched. An
        // empty directory with an unrecognized name is just a name
        // nobody told this tool to care about, not a
        // build-artifact-in-waiting.
        let scratch = btrfs_scratch_dir();
        let empty_project = scratch.path().join("brand-new-project");
        std::fs::create_dir_all(&empty_project).unwrap();
        let cache_dir = empty_cache();

        convert(
            &empty_project,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
        )
        .unwrap();

        assert!(!btrfs::is_subvolume(&empty_project).unwrap());
        assert!(empty_project.is_dir());
    }

    #[test]
    fn a_not_yet_existing_top_level_path_with_an_unrecognized_name_is_not_created() {
        let scratch = btrfs_scratch_dir();
        let missing_project = scratch.path().join("brand-new-project");
        let cache_dir = empty_cache();

        convert(
            &missing_project,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
        write_cache_rows(
            &cache_path(&cache_dir),
            &[(&project, "node_modules"), (&project, "target")],
        );

        convert(
            &project,
            None,
            &cache_path(&cache_dir),
            &roots_path(&cache_dir),
            cache_dir.path(),
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
