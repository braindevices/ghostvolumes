//! `ghostvolumes projects list/register/unregister`: manages the
//! project-roots list, a flat text file giving the decision-file walk-up
//! a narrower stopping boundary. CLI-only; the shim never writes it.
//!
//! Every mutation holds `project-roots.lock` for its whole
//! read-modify-write sequence, so a concurrent `unregister` rewrite can
//! never silently drop a `register` append (or vice versa).

use std::io::{IsTerminal, Write};
use std::path::Path;

use crate::convert::read_stdin_line;

fn lock_project_roots(list_path: &Path) -> anyhow::Result<std::fs::File> {
    let data_dir = list_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "project-roots list path {} has no parent directory",
            list_path.display()
        )
    })?;
    let lock_path = data_dir.join(crate::filenames::PROJECT_ROOTS_LOCK_FILE_NAME);
    let lock_file = crate::lock::open_lock_file(&lock_path)?;
    lock_file.lock()?;
    Ok(lock_file)
}

pub fn register(list_path: &Path, path: &str) -> anyhow::Result<()> {
    // Normalized before writing so the file never gains a new
    // trailing-slash entry (shell tab-completion often appends one).
    let path = crate::project_roots::normalize_root_path(path);

    let _lock = lock_project_roots(list_path)?;

    let existing = std::fs::read_to_string(list_path).unwrap_or_default();
    if !crate::project_roots::needs_append(&existing, &path) {
        return Ok(());
    }
    if let Some(parent) = list_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(list_path)?;
    // Single write_all for the whole line (not writeln!): O_APPEND only
    // guarantees one write() call lands atomically, not multiple.
    file.write_all(format!("{path}\n").as_bytes())?;
    Ok(())
}

/// Real entry point for `ghostvolumes projects unregister [path]`. See
/// `unregister_with_io` for the testable core.
pub fn unregister(list_path: &Path, path: Option<&str>) -> anyhow::Result<()> {
    unregister_with_io(
        list_path,
        path,
        std::io::stdin().is_terminal(),
        read_stdin_line,
    )
}

/// `Some(path)` removes that exact entry, no prompt. `None` (auto mode)
/// scans every entry and asks before removing any that's no longer a
/// directory; defaults to *not* removing on a non-TTY or empty answer.
fn unregister_with_io(
    list_path: &Path,
    path: Option<&str>,
    is_tty: bool,
    mut read_line: impl FnMut() -> Option<String>,
) -> anyhow::Result<()> {
    let _lock = lock_project_roots(list_path)?;

    let existing = std::fs::read_to_string(list_path).unwrap_or_default();
    // `parse` slash-normalizes every entry, so an explicit `path` still
    // matches regardless of a trailing slash on either side.
    let entries = crate::project_roots::parse(&existing);

    let to_keep: Vec<String> = match path {
        Some(target) => {
            let target = crate::project_roots::normalize_root_path(target);
            entries
                .into_iter()
                .filter(|entry| *entry != target)
                .collect()
        }
        None => {
            let mut keep = Vec::new();
            for entry in entries {
                let still_exists = Path::new(&entry).is_dir();
                if still_exists || !confirm_unregister(&entry, is_tty, &mut read_line) {
                    keep.push(entry);
                }
            }
            keep
        }
    };

    let text: String = to_keep.iter().map(|entry| format!("{entry}\n")).collect();
    crate::atomic_write::write_atomically(list_path, &text)
}

fn confirm_unregister(
    entry: &str,
    is_tty: bool,
    read_line: &mut impl FnMut() -> Option<String>,
) -> bool {
    if !is_tty {
        return false;
    }
    eprint!("{entry} no longer exists — remove it from the project-roots list? [y/N]: ");
    let _ = std::io::stderr().flush();
    match read_line() {
        Some(line) => matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes"),
        None => false,
    }
}

/// `(path, still_exists)` for every registered entry, in file order -
/// read-only, no lock needed. Backs `ghostvolumes projects list`.
pub fn list_projects(list_path: &Path) -> Vec<(String, bool)> {
    let existing = std::fs::read_to_string(list_path).unwrap_or_default();
    crate::project_roots::parse(&existing)
        .into_iter()
        .map(|entry| {
            let exists = Path::new(&entry).is_dir();
            (entry, exists)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filenames;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// A fresh project-roots list path under a new tempdir, bundled with
    /// its `TempDir` guard (must stay alive for `list_path` to be valid).
    fn temp_list_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let list_path = dir.path().join(filenames::PROJECT_ROOTS_FILE_NAME);
        (dir, list_path)
    }

    #[test]
    fn appends_a_new_path() {
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/home/user1/projects/app").unwrap();
        assert_eq!(
            std::fs::read_to_string(&list_path).unwrap(),
            "/home/user1/projects/app\n"
        );
    }

    #[test]
    fn is_idempotent_for_an_already_registered_path() {
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/a").unwrap();
        register(&list_path, "/a").unwrap();
        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/a\n");
    }

    #[test]
    fn appends_alongside_existing_entries() {
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/a").unwrap();
        register(&list_path, "/b").unwrap();
        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/a\n/b\n");
    }

    #[test]
    fn register_strips_a_trailing_slash_before_writing() {
        // Shell tab-completion routinely appends one for a directory
        // argument - the raw CLI arg must not land in the file as-is.
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/home/user1/projects/app/").unwrap();
        assert_eq!(
            std::fs::read_to_string(&list_path).unwrap(),
            "/home/user1/projects/app\n"
        );
    }

    #[test]
    fn register_is_idempotent_regardless_of_trailing_slash_on_either_side() {
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/a").unwrap();
        register(&list_path, "/a/").unwrap();
        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/a\n");
    }

    #[test]
    fn unregister_removes_an_entry_regardless_of_trailing_slash_on_either_side() {
        let (_dir, list_path) = temp_list_path();
        std::fs::write(&list_path, "/a/\n/b\n").unwrap();

        unregister_with_io(&list_path, Some("/a"), false, || {
            panic!("must not prompt for an explicit path")
        })
        .unwrap();

        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/b\n");
    }

    #[test]
    fn list_projects_normalizes_a_trailing_slash() {
        let (_dir, list_path) = temp_list_path();
        std::fs::write(&list_path, "/a/\n/b\n").unwrap();

        let listed = list_projects(&list_path);

        assert_eq!(
            listed,
            vec![("/a".to_string(), false), ("/b".to_string(), false)]
        );
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempdir().unwrap();
        let list_path = dir
            .path()
            .join("nested/deep")
            .join(filenames::PROJECT_ROOTS_FILE_NAME);
        register(&list_path, "/a").unwrap();
        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/a\n");
    }

    #[test]
    fn concurrent_registers_never_interleave_or_split_a_line() {
        // Every concurrent append must land as a complete, untouched
        // line - never merged or split with another.
        let (_dir, list_path) = temp_list_path();
        let paths: Vec<String> = (0..8).map(|i| format!("/project-{i}")).collect();

        let handles: Vec<_> = paths
            .iter()
            .map(|path| {
                let list_path = list_path.clone();
                let path = path.clone();
                std::thread::spawn(move || register(&list_path, &path).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let text = std::fs::read_to_string(&list_path).unwrap();
        let mut lines: Vec<&str> = text.lines().collect();
        lines.sort();
        let mut expected: Vec<&str> = paths.iter().map(String::as_str).collect();
        expected.sort();
        assert_eq!(lines, expected);
    }

    #[test]
    fn unregister_removes_the_exact_path_with_no_prompt() {
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/a").unwrap();
        register(&list_path, "/b").unwrap();

        unregister_with_io(&list_path, Some("/a"), false, || {
            panic!("must not prompt for an explicit path")
        })
        .unwrap();

        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/b\n");
    }

    #[test]
    fn unregister_exact_path_is_idempotent_when_absent() {
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/a").unwrap();

        unregister_with_io(&list_path, Some("/nonexistent"), false, || None).unwrap();

        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/a\n");
    }

    #[test]
    fn auto_mode_prunes_only_missing_entries_when_confirmed() {
        let (dir, list_path) = temp_list_path();
        let still_here = dir.path().join("still-here");
        std::fs::create_dir_all(&still_here).unwrap();
        register(&list_path, still_here.to_str().unwrap()).unwrap();
        register(&list_path, "/definitely/does/not/exist").unwrap();

        let mut answers = vec!["y".to_string()].into_iter();
        unregister_with_io(&list_path, None, true, move || answers.next()).unwrap();

        assert_eq!(
            std::fs::read_to_string(&list_path).unwrap(),
            format!("{}\n", still_here.display())
        );
    }

    #[test]
    fn auto_mode_keeps_a_missing_entry_when_declined() {
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/definitely/does/not/exist").unwrap();

        let mut answers = vec!["n".to_string()].into_iter();
        unregister_with_io(&list_path, None, true, move || answers.next()).unwrap();

        assert_eq!(
            std::fs::read_to_string(&list_path).unwrap(),
            "/definitely/does/not/exist\n"
        );
    }

    #[test]
    fn auto_mode_defaults_to_keeping_everything_on_a_non_tty() {
        let (_dir, list_path) = temp_list_path();
        register(&list_path, "/definitely/does/not/exist").unwrap();

        unregister_with_io(&list_path, None, false, || {
            panic!("must not read a line when not a tty")
        })
        .unwrap();

        assert_eq!(
            std::fs::read_to_string(&list_path).unwrap(),
            "/definitely/does/not/exist\n"
        );
    }

    #[test]
    fn a_register_between_unregisters_read_and_write_is_not_lost() {
        // Hold the lock, spawn unregister (must block on it), then
        // append and release - unregister's read must see this append.
        let (dir, list_path) = temp_list_path();
        register(&list_path, "/existing").unwrap();

        let lock_path = dir.path().join(filenames::PROJECT_ROOTS_LOCK_FILE_NAME);
        let lock_file = crate::lock::open_lock_file(&lock_path).unwrap();
        lock_file.lock().unwrap();

        let unregister_list_path = list_path.clone();
        let handle = std::thread::spawn(move || {
            unregister_with_io(&unregister_list_path, Some("/existing"), false, || None).unwrap();
        });

        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !handle.is_finished(),
            "unregister should still be blocked while register's lock is held"
        );

        // The in-flight "register" appends a new path before releasing.
        std::fs::OpenOptions::new()
            .append(true)
            .open(&list_path)
            .unwrap()
            .write_all(b"/new-during-the-race\n")
            .unwrap();
        drop(lock_file);

        handle.join().unwrap();
        assert_eq!(
            std::fs::read_to_string(&list_path).unwrap(),
            "/new-during-the-race\n"
        );
    }

    #[test]
    fn list_projects_flags_missing_entries() {
        let (dir, list_path) = temp_list_path();
        let still_here = dir.path().join("still-here");
        std::fs::create_dir_all(&still_here).unwrap();
        register(&list_path, still_here.to_str().unwrap()).unwrap();
        register(&list_path, "/definitely/does/not/exist").unwrap();

        let listed = list_projects(&list_path);
        assert_eq!(
            listed,
            vec![
                (still_here.display().to_string(), true),
                ("/definitely/does/not/exist".to_string(), false),
            ]
        );
    }
}
