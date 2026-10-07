//! `ghostvolumes intercept -- <cmd>`: sets `LD_PRELOAD` on the child
//! process only, execs with stdio fully inherited, and waits - a plain
//! passthrough, no prompting.
//!
//! After `<cmd>` exits, checks whether any decision file at a possible
//! project-root boundary changed during the run, and if so prints one
//! notice per changed root naming the covering `convert` command.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use crate::filenames;

/// Every possible project-root boundary: the union of `compiled.tsv`'s
/// row prefixes and the registered project-roots list, deduplicated and
/// sorted for a deterministic snapshot/diff order.
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

/// Each boundary's decision file text right now (`None` if absent),
/// compared before/after `<cmd>` runs. Full-text, not mtime, since
/// mtime resolution can be too coarse to catch a sub-second run.
fn snapshot(boundaries: &[PathBuf]) -> Vec<Option<String>> {
    boundaries
        .iter()
        .map(|b| crate::decision::read_regular_file(&b.join(filenames::DECISION_FILE_NAME)))
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
        .env(
            "LD_PRELOAD",
            preload_value(std::env::var_os("LD_PRELOAD").as_deref(), preload_so_path),
        )
        .status()?;

    let after = snapshot(&boundaries);
    for root in touched_boundaries(&boundaries, &before, &after) {
        notify(root);
    }

    // Killed by a signal: the shell convention 128+N, not a bare 1.
    use std::os::unix::process::ExitStatusExt;
    Ok(status
        .code()
        .unwrap_or_else(|| 128 + status.signal().unwrap_or(0)))
}

/// `LD_PRELOAD` for the child: the shim appended to whatever is already
/// preloaded (e.g. jemalloc), the same as `shell-init` does, and not
/// added twice if it's already there.
fn preload_value(existing: Option<&std::ffi::OsStr>, so: &Path) -> std::ffi::OsString {
    let Some(existing) = existing.filter(|e| !e.is_empty()) else {
        return so.as_os_str().to_owned();
    };
    let already = existing
        .to_string_lossy()
        .split([':', ' '])
        .any(|entry| Path::new(entry) == so);
    if already {
        return existing.to_owned();
    }
    let mut value = existing.to_owned();
    value.push(":");
    value.push(so);
    value
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
        let cache_path = dir.path().join(filenames::COMPILED_CACHE_FILE_NAME);
        let project_roots_path = dir.path().join(filenames::PROJECT_ROOTS_FILE_NAME);
        let preload_so = dir.path().join(filenames::SHIM_FILE_NAME);

        // 7 only if the child really sees the shim in LD_PRELOAD.
        let script = format!(
            "case \"$LD_PRELOAD\" in *{}) exit 7;; esac; exit 1",
            preload_so.display()
        );
        let code = intercept(
            &["sh".to_string(), "-c".to_string(), script],
            &preload_so,
            &cache_path,
            &project_roots_path,
        )
        .unwrap();
        assert_eq!(code, 7);
    }

    #[test]
    fn a_signal_death_maps_to_128_plus_the_signal() {
        let dir = tempdir().unwrap();
        let code = intercept(
            &[
                "sh".to_string(),
                "-c".to_string(),
                "kill -TERM $$".to_string(),
            ],
            &dir.path().join(filenames::SHIM_FILE_NAME),
            &dir.path().join(filenames::COMPILED_CACHE_FILE_NAME),
            &dir.path().join(filenames::PROJECT_ROOTS_FILE_NAME),
        )
        .unwrap();
        assert_eq!(code, 128 + 15);
    }

    #[test]
    fn preload_value_appends_once_and_keeps_existing_entries() {
        let so = Path::new("/d/libghostvolumes_shim.so");
        let value = |existing: Option<&str>| {
            preload_value(existing.map(std::ffi::OsStr::new), so)
                .into_string()
                .unwrap()
        };
        assert_eq!(value(None), "/d/libghostvolumes_shim.so");
        assert_eq!(value(Some("")), "/d/libghostvolumes_shim.so");
        assert_eq!(value(Some("/j.so")), "/j.so:/d/libghostvolumes_shim.so");
        assert_eq!(
            value(Some("/j.so:/d/libghostvolumes_shim.so")),
            "/j.so:/d/libghostvolumes_shim.so"
        );
    }

    #[test]
    fn reports_a_touched_project_root_after_the_command_exits() {
        let dir = tempdir().unwrap();
        let project = dir.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        let cache_path = dir.path().join(filenames::COMPILED_CACHE_FILE_NAME);
        let project_roots_path = dir.path().join(filenames::PROJECT_ROOTS_FILE_NAME);
        std::fs::write(&project_roots_path, format!("{}\n", project.display())).unwrap();
        let preload_so = dir.path().join(filenames::SHIM_FILE_NAME);

        let decision_file = project.join(filenames::DECISION_FILE_NAME);
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
            &dir.path().join(filenames::SHIM_FILE_NAME),
            &dir.path().join(filenames::COMPILED_CACHE_FILE_NAME),
            &dir.path().join(filenames::PROJECT_ROOTS_FILE_NAME),
        )
        .unwrap_err();
        assert!(err.to_string().contains("no command given"));
    }
}
