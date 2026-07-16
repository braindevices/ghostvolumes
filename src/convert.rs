//! `ghostvolumes convert <path>` (ai-work/tasks/decision-model.plan.md
//! §6): a recursive walk-and-resolve, not just a single-leaf migration.
//! `<path>` is a starting point: every candidate under it matching a
//! watched name under a configured root gets resolved too (reusing
//! `discover`'s tree-walking conventions — skip `.git`, optional
//! `--max-depth`, never descend into a match). `<path>` itself is
//! always a candidate regardless of whether it matches (that's the
//! whole point of naming it explicitly), and is created directly as a
//! fresh, empty subvolume if it doesn't exist yet at all — this is
//! what replaces cd-hook's old proactive pre-creation entirely.
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
//! - Undecided (or doesn't exist yet) → convert (create empty, or
//!   copy-and-swap if already a plain directory), then ask "remember
//!   this?" (skipped, defaulting to no, when `stdin` isn't a TTY).
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
    No,
    JustThisPath,
    EveryMatchOfThisName,
}

fn parse_remember_answer(line: &str) -> RememberChoice {
    match line.trim().to_ascii_lowercase().as_str() {
        "y" | "yes" => RememberChoice::JustThisPath,
        "a" | "all" => RememberChoice::EveryMatchOfThisName,
        _ => RememberChoice::No,
    }
}

/// Asks the "remember this?" question (§6). `None` (no answer at all,
/// same as an explicit "no") when `is_tty` is false — the same
/// "couldn't ask isn't the human said no" posture used throughout this
/// design. `read_line` is injectable so this is unit-testable without a
/// real terminal.
fn ask_remember(
    candidate: &Path,
    is_tty: bool,
    read_line: impl FnOnce() -> Option<String>,
) -> RememberChoice {
    if !is_tty {
        return RememberChoice::No;
    }
    eprint!(
        "Remember this decision for {}? [y]es, just this path / [a]ll matches of this name / [N]o: ",
        candidate.display()
    );
    let _ = std::io::stderr().flush();
    match read_line() {
        Some(line) => parse_remember_answer(&line),
        None => RememberChoice::No,
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

fn append_decision(boundary: &Path, line: &str) -> anyhow::Result<()> {
    std::fs::create_dir_all(boundary)?;
    let file_path = boundary.join(filenames::DECISION_FILE_NAME);
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&file_path)?;
    // One write_all call for the whole line - see projects.rs's
    // identical fix for why writeln! isn't safe against a concurrent
    // appender (ai-work/tasks/atomic-file-io.plan.md §3).
    file.write_all(format!("{line}\n").as_bytes())?;
    Ok(())
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
    let boundary_str = boundary.display().to_string();
    if !project_roots.iter().any(|r| r == &boundary_str) {
        project_roots.push(boundary_str.clone());
    }
    projects::register(project_roots_path, &boundary_str)
}

fn maybe_ask_and_record(
    candidate: &Path,
    boundary: &Path,
    project_roots: &mut Vec<String>,
    project_roots_path: &Path,
) -> anyhow::Result<()> {
    let choice = ask_remember(candidate, std::io::stdin().is_terminal(), read_stdin_line);
    let pattern = match choice {
        RememberChoice::No => return Ok(()),
        RememberChoice::JustThisPath => decision::anchored_pattern(boundary, candidate)
            .unwrap_or_else(|| candidate.display().to_string()),
        RememberChoice::EveryMatchOfThisName => {
            let name = candidate
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned();
            containing_dir_pattern(boundary, candidate, &name)
        }
    };
    append_decision(boundary, &format!("+ {pattern}"))?;
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
        Ok(()) => Ok(()),
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

    // Atomic swap: move the old plain dir out of the way, move the new
    // subvolume into place, then clean up the old dir. `path` is never
    // missing or half-written in between the two renames.
    let backup_name = format!(".{name}.ghostvolumes-convert-old");
    let backup_path = parent.join(&backup_name);
    std::fs::rename(path, &backup_path)?;
    std::fs::rename(&tmp_path, path)?;
    std::fs::remove_dir_all(&backup_path)?;

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

fn resolve_candidate(
    candidate: &Path,
    top_level_path: &Path,
    rows: &[(String, String)],
    project_roots: &mut Vec<String>,
    project_roots_path: &Path,
    data_dir: &Path,
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
            if !confirm_override(candidate, std::io::stdin().is_terminal(), read_stdin_line) {
                return Ok(());
            }
            materialize(candidate, &boundary, data_dir)?;
            maybe_ask_and_record(candidate, &boundary, project_roots, project_roots_path)
        }
        None => {
            materialize(candidate, &boundary, data_dir)?;
            maybe_ask_and_record(candidate, &boundary, project_roots, project_roots_path)
        }
    }
}

pub fn convert(
    path: &Path,
    max_depth: Option<u32>,
    cache_path: &Path,
    project_roots_path: &Path,
    data_dir: &Path,
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
        assert!(matches!(parse_remember_answer(""), RememberChoice::No));
        assert!(matches!(parse_remember_answer("n"), RememberChoice::No));
        assert!(matches!(
            parse_remember_answer("garbage"),
            RememberChoice::No
        ));
    }

    #[test]
    fn ask_remember_defaults_to_no_when_not_a_tty() {
        assert!(matches!(
            ask_remember(Path::new("/x"), false, || Some("y".to_string())),
            RememberChoice::No
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
    fn does_not_descend_into_a_matched_nested_candidate() {
        let scratch = btrfs_scratch_dir();
        let project = scratch.path().join("project");
        let outer = project.join("node_modules");
        let inner = outer.join("target"); // nested match inside a match
        std::fs::create_dir_all(&inner).unwrap();
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
