//! `ghostvolumes convert <path> [--create <relative-path>]...` recursively
//! walks `<path>` and resolves each candidate directory to either convert
//! it to a BTRFS subvolume or record a decision about it. `<path>` itself
//! is the decision-file/project-roots boundary and is never a candidate;
//! at most one registered project can ever cover a given path. Candidates
//! come from the recursive walk, `--create`, and anchored decision-file
//! patterns, resolved shallowest-first. Per candidate: already a
//! subvolume -> skip; `+` -> convert; `-` -> skip (confirm if named via
//! `--create`); undecided at a TTY -> ask; undecided, no TTY -> leave a
//! pending `?` marker. `ensure_project_registered` runs once upfront.

use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::debug::{Verbosity, trace};
use crate::{btrfs, cache, decision, filenames, merge, project_roots, projects};

/// What resolving a candidate's decision should actually *do*, decomposed
/// into two independent capabilities. `convert` and `decide` are two of
/// the four combinations of these named fields (not bare `bool`s, so a
/// call site can't silently swap them).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Mode {
    /// Consider undecided candidates at all — ask (if a TTY) or leave a
    /// pending `?` marker (if not). `false` skips silently, no marker.
    /// Distinct from `is_tty`: this is "do we want an answer at all",
    /// not "can we get one right now".
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

/// Asks the "convert (and remember)?"/"decide (and remember)?" question
/// and parses the answer. Always asks — whether to ask at all (`is_tty`)
/// is the caller's call. The verb is `mode`-aware so `decide` never
/// claims it will "Convert" when it won't.
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

/// Asks about an already-a-subvolume candidate with no recorded decision
/// — only a decision to record, nothing left to convert. Defaults to
/// **yes** on an empty answer (unlike every other ask here), since an
/// existing subvolume was most likely made that way on purpose.
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

/// `pub(crate)` since `projects::unregister`'s auto-scan-and-prune mode
/// reuses this same injectable-stdin-reader shape.
pub(crate) fn read_stdin_line() -> Option<String> {
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok()?;
    Some(line)
}

fn read_decision_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// `true` iff `a` and `b` fall under the same configured `roots.d` volume
/// (same `cache::longest_matching_prefix` result, including both `None`).
/// Path containment alone doesn't imply same volume — a narrower row can
/// nest inside a broader one — so both must be checked.
fn same_volume(rows: &[(String, String)], a: &Path, b: &Path) -> bool {
    cache::longest_matching_prefix(rows, a) == cache::longest_matching_prefix(rows, b)
}

/// The decision-file/ignore-file walk-up boundary for the whole
/// `convert`/`decide` run, computed once from `project_path` (nested
/// project registration is disallowed, so there's at most one covering
/// project to find). Finds the registered project that is an
/// ancestor-or-self of `project_path` *and* on the same volume
/// (`same_volume`); falls back to `project_path` itself if none
/// qualifies. Never falls back to a bare volume-root prefix — that
/// would let resolution wander outside a real registered project.
fn decision_boundary(
    rows: &[(String, String)],
    project_roots: &[String],
    project_path: &Path,
) -> PathBuf {
    project_roots
        .iter()
        .map(Path::new)
        .filter(|root| project_path.starts_with(root) && same_volume(rows, root, project_path))
        // Shallowest of any qualifying matches, so the walk-up checks
        // every level's decision file instead of stopping at the deepest.
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

/// Idempotently registers `boundary` into the project-roots list, both
/// on disk (`projects::register`) and in the in-memory list (so later
/// candidates in this same run see it without a second disk read).
fn register_project_root(
    boundary: &Path,
    project_roots: &mut Vec<String>,
    project_roots_path: &Path,
) -> anyhow::Result<()> {
    // Normalize here too so this in-memory list stays consistent with
    // whatever `projects::register` writes to disk.
    let boundary_str = crate::project_roots::normalize_root_path(&boundary.display().to_string());
    if !project_roots.iter().any(|r| r == &boundary_str) {
        project_roots.push(boundary_str.clone());
    }
    projects::register(project_roots_path, &boundary_str)
}

/// Blocking-locks `boundary`'s decisions lock file (distinct from
/// `materialize`'s subvolume-creation lock) around any read-then-write
/// of the decision file, since toggling a pending marker is a
/// read-modify-write, unlike a plain append. Blocking is fine: `convert`
/// is an explicit, occasional, human-run command.
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
/// still undecided — the same mechanism the shim uses, so a candidate
/// `convert` can't ask about (no TTY) leaves a trail a human can later
/// turn into a real `+`/`-` line by hand.
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

/// Records a real `+`/`-` decision (`prefix` is `"+"` or `"-"`) for
/// `boundary`'s decision file, replacing any pending marker matching
/// `anchored_pattern` in place — even when `decision_pattern` is a
/// broader pattern than the marker it supersedes. Falls back to a plain
/// append if there was no pending marker. Rewrites the whole file
/// atomically under `lock_decisions`, so a reader never sees a
/// half-written file and this can't race a concurrent shim append.
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

/// Asks "convert this?" *before* touching the filesystem. No TTY ->
/// leave a pending `?` marker, convert nothing. "No"/empty -> record a
/// `-`, convert nothing. "Yes"/"all" -> convert, then record `+` (only
/// after `materialize` succeeds, so a failed conversion never leaves a
/// `+` for something not actually converted).
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
        // Always printed: the one signal a human has that a decision is
        // waiting to be made.
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

/// Creates `target` directly as a fresh, empty subvolume, creating any
/// missing parent directories first.
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

    // AlreadyExists is tolerated: the shim could have won a race and
    // created it just before this call took the lock, but the desired
    // end state (target is a subvolume) still holds either way.
    match btrfs::create_subvolume(parent, &name) {
        Ok(()) => {
            println!("create: {} (new empty subvolume)", target.display());
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// Creates a subvolume at a temp sibling path, `cp -a --reflink=always`s
/// the existing plain directory's contents in, then atomically swaps it
/// into place and removes the old directory.
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
/// around the create/copy/rename sequence — coordinates with the shim's
/// own (non-blocking) lock on the same boundary. Blocking is fine here
/// since `convert` is a human-run command. Held only around this
/// operation, not the "remember this?" prompt before it.
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

/// Should `candidate` be skipped entirely by the walk, never even
/// checked for a watched-name match? Unions three tiers: the global
/// `default-ignore` list (anchored to `candidate`'s parent), the nearest
/// configured volume root's own `.ghostvolumes-ignore`, and `boundary`'s
/// own `.ghostvolumes-ignore`. Ignore files are read fresh per directory
/// rather than cached, unlike decision files.
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

/// Every candidate implied by an anchored, wildcard-free `+`/`?` pattern
/// in `boundary`'s decision file — surfaces targets the filesystem walk
/// can't discover on its own (doesn't exist yet, or isn't a watched
/// name). Shared by `convert` and `decide`.
fn decision_file_anchored_candidates(boundary: &Path) -> Vec<PathBuf> {
    let text =
        std::fs::read_to_string(boundary.join(filenames::DECISION_FILE_NAME)).unwrap_or_default();
    decision::parse_anchored_exact_patterns(&text)
        .into_iter()
        .map(|pattern| boundary.join(pattern.trim_start_matches('/')))
        .collect()
}

/// Walks `project_path`'s subtree (skipping `is_ignored`, never
/// descending into a match), collecting every directory that's either a
/// watched name under a configured root, or already a real BTRFS
/// subvolume regardless of name. `project_path` itself is never
/// included. `boundary` is threaded through separately for
/// `is_ignored`'s project-root ignore-file tier.
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
        // An existing subvolume is a candidate regardless of whether its
        // name is on the watch list; never descended into either way.
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

/// Prints what `resolve_candidate`'s `Some(true)` branch would do,
/// without doing it — mirrors `materialize`'s create-vs-migrate split so
/// a dry run reports which of the two a real run would perform.
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

/// `boundary` is the whole run's precomputed `decision_boundary`.
/// `dry_run` short-circuits every branch that would otherwise mutate the
/// filesystem, the decision file, or prompt, printing what would happen
/// instead.
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
        // Already a subvolume: only a decision to record, so the "yes"
        // path here must never call `materialize` — `copy_and_swap`'s
        // `remove_dir_all` cannot remove a real subvolume.
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

/// `true` only for an explicit "y"/"yes"; an empty answer is a decline.
/// Used for the default-no confirmations in `ensure_project_registered`.
fn read_yes_no(read_line: &mut impl FnMut() -> Option<String>) -> bool {
    match read_line() {
        Some(line) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        None => false,
    }
}

/// Walks up from `path`'s parent looking for a decision file authored
/// for a broader ancestor project — a sign the real project boundary
/// may have been forgotten. Bounded by `limit` (`path`'s own volume, or
/// `None` for the filesystem root).
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

/// Ensures `path` is covered by exactly one registered project (nested
/// registration is disallowed). In order: (1) already covered -> no-op.
/// (2) would nest over a registered descendant -> ask to unregister it
/// first (default no). (3) an ancestor has an orphaned decision file ->
/// ask to register anyway (default no). (4) otherwise, plain register
/// ask (default yes). No TTY aborts; `dry_run` short-circuits after (1).
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
/// testable core.
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

    // Computed once for the whole run, not per-candidate.
    let boundary = decision_boundary(&rows, &project_roots, path);

    // Dedupes across all three candidate sources before sorting below.
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
/// <pattern>`: same walk-and-resolve engine as `convert`, but `Mode::DECIDE`
/// means an existing `+` is a no-op and "yes" only records a decision,
/// never touches the filesystem. No `--create`.
///
/// First, each `--add`/`--deny` pattern is recorded verbatim (also
/// doubling as the search key for any existing pending `?` marker with
/// that exact pattern, so hand-typing it toggles that line in place).
/// Then candidates are gathered and resolved exactly like `convert`.
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

    // 1. Hand-authored patterns, verbatim, first.
    for pattern in add {
        record_decision(data_dir, &boundary, pattern, pattern, "+")?;
        println!("recorded: + {pattern} (at {})", boundary.display());
    }
    for pattern in deny {
        record_decision(data_dir, &boundary, pattern, pattern, "-")?;
        println!("recorded: - {pattern} (at {})", boundary.display());
    }

    // 2. Walk the filesystem exactly like convert, plus anything implied
    // by an anchored decision-file pattern the walk couldn't discover.
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

    /// A `roots.d`-less config dir, for tests that don't care about
    /// `default-ignore`.
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
        // Boundary must be the shallowest registered project (/a/b), not
        // the deepest, in a same-volume nested chain.
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
        // Would panic if it asked at all - already covered, no-op expected.
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
        // A pre-recorded `+` decision converts directly without asking.
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
        // An anchored `+` decision for an unwatched name must still get
        // created, with no --create needed.
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
        // first; create_empty must tolerate this, not error.
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

        // Non-TTY: verifies conversion happened via the existing `+`,
        // not by falling through to asking.
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
        // Asking happens before any filesystem change; "yes" both
        // converts and records.
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
        // Declining leaves existing content untouched but still records
        // a `-`, so it isn't asked again next time.
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
        // Non-interactive: converts nothing but leaves a pending
        // "? <pattern>" marker.
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
        // A pending marker becomes the real decision in place, not a
        // second, separate line.
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
        // "a" records a broader pattern than the marker, still swapped
        // in place rather than appended at the end.
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

        // vendor is found via the recursive walk, so its `-` decision
        // is respected with no override prompt.
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
        // `project` itself is never added to `candidates`, unlike the
        // nested "node_modules" match.
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
        // Only the outer (/a/b) of a same-volume nested chain has a
        // decision; it must still govern the inner (/a/b/c) argument.
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
        // A single row at `outer` keeps both projects on the same volume.
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
        // A path-ancestor project on a different volume must not count
        // as covering; the inner path gets its own registration.
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

        // First answer: register as a project (empty = yes). Second:
        // "convert (and remember)?" for the node_modules candidate.
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
        // The volume root is the broader `scratch` dir, not `project`,
        // so its `.ghostvolumes-ignore` lives there instead.
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
        // Same nested-project reasoning as convert's own equivalent
        // test: the decision must land in outer's file, not inner's.
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
        // Unlike the shim's non-blocking try_lock, materialize blocks.
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
