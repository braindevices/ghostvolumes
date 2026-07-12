//! `ghostvolumes intercept -- <cmd>` (ai-work/tasks/decision-model.plan.md
//! §5): the one entry point that sets `LD_PRELOAD` for a child process
//! only — no shell-rc setup required, works standalone. Sets the env
//! var on the *child's* environment only (`std::process::Command::env`
//! never touches this process's own environment), execs with stdio
//! fully inherited (`Command`'s default), and waits — completely normal
//! passthrough while `<cmd>` runs, no redirection, no flags, no
//! prompting of any kind (§5's "the shim can never be the one
//! prompting" applies here too: `intercept` itself doesn't prompt
//! either, only reports after the fact).
//!
//! After `<cmd>` exits (full foreground control back, no longer racing
//! with anything), checks whether any decision file at a *possible*
//! project-root boundary (every distinct `compiled.tsv` row prefix and
//! registered project-roots entry — the exhaustive set of locations
//! `walkup_boundary` could ever resolve to) changed during the run, and
//! if so, prints one notice per changed root naming the single
//! covering `ghostvolumes convert <project-root>` command, rather than
//! one line per individual pending path.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::decision;

/// Every location a decision file could possibly live at the *root* of
/// a project (§3's `walkup_boundary`): the union of `compiled.tsv`'s
/// row prefixes and the registered project-roots list. Deduplicated
/// and sorted for a deterministic snapshot/diff order.
fn candidate_boundaries(rows: &[(String, String)], project_roots: &[String]) -> Vec<PathBuf> {
    let mut set: BTreeSet<String> = BTreeSet::new();
    for (prefix, _) in rows {
        set.insert(prefix.clone());
    }
    for root in project_roots {
        set.insert(root.clone());
    }
    set.into_iter().map(PathBuf::from).collect()
}

/// Each boundary's decision file text right now (`None` if it doesn't
/// exist) - compared before/after `<cmd>` runs. Full-text comparison
/// rather than mtime: these are small files and mtime resolution on
/// some filesystems is coarse enough to miss a sub-second run.
fn snapshot(boundaries: &[PathBuf]) -> Vec<Option<String>> {
    boundaries
        .iter()
        .map(|b| std::fs::read_to_string(b.join(decision::DECISION_FILE_NAME)).ok())
        .collect()
}

/// The boundaries whose decision file's text differs between `before`
/// and `after` - i.e., something (almost certainly a pending-comment
/// append, §4) changed it during the run.
fn touched_boundaries<'a>(
    boundaries: &'a [PathBuf],
    before: &[Option<String>],
    after: &[Option<String>],
) -> Vec<&'a Path> {
    boundaries
        .iter()
        .zip(before.iter().zip(after.iter()))
        .filter(|(_, (b, a))| b != a)
        .map(|(p, _)| p.as_path())
        .collect()
}

pub fn intercept(
    cmd: &[String],
    preload_so_path: &Path,
    cache_path: &Path,
    project_roots_path: &Path,
) -> anyhow::Result<i32> {
    intercept_with_notifier(
        cmd,
        preload_so_path,
        cache_path,
        project_roots_path,
        print_notice,
    )
}

fn print_notice(root: &Path) {
    eprintln!(
        "ghostvolumes: new undecided path(s) found under {} — run `ghostvolumes convert {}` to review them",
        root.display(),
        root.display()
    );
}

/// `notify` is injectable so the notice logic is unit-testable without
/// capturing real stderr output.
fn intercept_with_notifier(
    cmd: &[String],
    preload_so_path: &Path,
    cache_path: &Path,
    project_roots_path: &Path,
    mut notify: impl FnMut(&Path),
) -> anyhow::Result<i32> {
    let Some((program, args)) = cmd.split_first() else {
        anyhow::bail!("no command given (usage: ghostvolumes intercept -- <cmd> [args...])");
    };

    let rows = crate::cache::parse(&std::fs::read_to_string(cache_path).unwrap_or_default());
    let project_roots = crate::project_roots::parse(
        &std::fs::read_to_string(project_roots_path).unwrap_or_default(),
    );
    let boundaries = candidate_boundaries(&rows, &project_roots);
    let before = snapshot(&boundaries);

    let status = std::process::Command::new(program)
        .args(args)
        .env("LD_PRELOAD", preload_so_path)
        .status()?;

    let after = snapshot(&boundaries);
    for root in touched_boundaries(&boundaries, &before, &after) {
        notify(root);
    }

    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn candidate_boundaries_dedups_and_unions_both_sources() {
        let rows = vec![
            ("/a".to_string(), "node_modules".to_string()),
            ("/a".to_string(), "target".to_string()),
            ("/b".to_string(), "node_modules".to_string()),
        ];
        let project_roots = vec!["/a".to_string(), "/c".to_string()];
        let boundaries = candidate_boundaries(&rows, &project_roots);
        assert_eq!(
            boundaries,
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c")
            ]
        );
    }

    #[test]
    fn touched_boundaries_detects_a_newly_created_file() {
        let boundaries = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let before = vec![None, None];
        let after = vec![Some("# /node_modules\n".to_string()), None];
        assert_eq!(
            touched_boundaries(&boundaries, &before, &after),
            vec![Path::new("/a")]
        );
    }

    #[test]
    fn touched_boundaries_detects_a_changed_file() {
        let boundaries = vec![PathBuf::from("/a")];
        let before = vec![Some("+ target\n".to_string())];
        let after = vec![Some("+ target\n# /node_modules\n".to_string())];
        assert_eq!(
            touched_boundaries(&boundaries, &before, &after),
            vec![Path::new("/a")]
        );
    }

    #[test]
    fn touched_boundaries_empty_when_nothing_changed() {
        let boundaries = vec![PathBuf::from("/a"), PathBuf::from("/b")];
        let before = vec![Some("+ target\n".to_string()), None];
        let after = before.clone();
        assert!(touched_boundaries(&boundaries, &before, &after).is_empty());
    }

    #[test]
    fn runs_the_command_with_ld_preload_set_and_propagates_its_exit_code() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("compiled.tsv");
        let project_roots_path = dir.path().join("project-roots.txt");
        let preload_so = dir.path().join("preload.so");

        let code = intercept(
            &["sh".to_string(), "-c".to_string(), "exit 7".to_string()],
            &preload_so,
            &cache_path,
            &project_roots_path,
        )
        .unwrap();
        assert_eq!(code, 7);
    }

    #[test]
    fn reports_a_touched_project_root_after_the_command_exits() {
        let dir = tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let cache_path = dir.path().join("compiled.tsv");
        let project_roots_path = dir.path().join("project-roots.txt");
        std::fs::write(&project_roots_path, format!("{}\n", project.display())).unwrap();
        let preload_so = dir.path().join("preload.so");

        let decision_file = project.join(".ghostvolumes-decisions");
        let cmd = format!("echo '# /node_modules' >> {}", decision_file.display());
        let mut reported = Vec::new();
        intercept_with_notifier(
            &["sh".to_string(), "-c".to_string(), cmd],
            &preload_so,
            &cache_path,
            &project_roots_path,
            |p| reported.push(p.to_path_buf()),
        )
        .unwrap();

        assert_eq!(reported, vec![project]);
    }

    #[test]
    fn errors_on_an_empty_command() {
        let dir = tempdir().unwrap();
        let err = intercept(
            &[],
            &dir.path().join("preload.so"),
            &dir.path().join("compiled.tsv"),
            &dir.path().join("project-roots.txt"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no command given"));
    }
}
