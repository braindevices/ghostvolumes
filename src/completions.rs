//! Dynamic shell-completion candidates (`clap_complete`'s
//! `unstable-dynamic` engine) - both read live on-disk state, so a
//! completer never runs a decision or registers anything itself.

use std::path::Path;

use clap_complete::engine::CompletionCandidate;

fn matches_prefix(value: &str, current: &std::ffi::OsStr) -> bool {
    current.to_str().is_some_and(|c| value.starts_with(c))
}

/// Every path in the registered project-roots list, for `<path>`
/// arguments (`convert`/`decide`/`projects unregister`).
pub fn registered_projects(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Ok(data_dir) = crate::xdg::data_dir() else {
        return Vec::new();
    };
    registered_projects_in(&data_dir, current)
}

fn registered_projects_in(data_dir: &Path, current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let list_path = data_dir.join(crate::filenames::PROJECT_ROOTS_FILE_NAME);
    crate::projects::list_projects(&list_path)
        .into_iter()
        .filter(|(path, _)| matches_prefix(path, current))
        .map(|(path, _)| CompletionCandidate::new(path))
        .collect()
}

/// Every currently-enabled, configured root, for `roots disable`.
pub fn enabled_roots(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Ok(config_dir) = crate::xdg::config_dir() else {
        return Vec::new();
    };
    let Ok(merged) = crate::merge::load_all(&config_dir) else {
        return Vec::new();
    };
    merged
        .roots
        .into_iter()
        .filter(|root| matches_prefix(&root.path, current))
        .map(|root| CompletionCandidate::new(root.path))
        .collect()
}

/// Every currently-disabled root, for `roots enable`.
pub fn disabled_roots(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Ok(config_dir) = crate::xdg::config_dir() else {
        return Vec::new();
    };
    crate::roots::list_disabled(&config_dir)
        .into_iter()
        .filter(|path| matches_prefix(path, current))
        .map(CompletionCandidate::new)
        .collect()
}

/// Every `?` pending pattern in `dir`'s own decision file, for `decide
/// --add`/`--deny` - scoped to a directory rather than the command's
/// own `<path>` argument, since a completer for one argument can't see
/// another argument's already-typed value.
fn pending_patterns_in(dir: &Path, current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Ok(text) = std::fs::read_to_string(dir.join(crate::filenames::DECISION_FILE_NAME)) else {
        return Vec::new();
    };
    text.lines()
        .filter_map(|line| line.trim().strip_prefix('?'))
        .map(str::trim)
        .filter(|pattern| matches_prefix(pattern, current))
        .map(CompletionCandidate::new)
        .collect()
}

/// [`pending_patterns_in`] scoped to the current working directory.
pub fn pending_patterns(current: &std::ffi::OsStr) -> Vec<CompletionCandidate> {
    let Ok(cwd) = std::env::current_dir() else {
        return Vec::new();
    };
    pending_patterns_in(&cwd, current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn pending_patterns_reads_only_question_mark_lines() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(crate::filenames::DECISION_FILE_NAME),
            "+ node_modules\n? /build/should-review\n- vendor\n? .cache\n",
        )
        .unwrap();

        let result = pending_patterns_in(dir.path(), OsStr::new(""));

        let values: Vec<_> = result.iter().map(|c| c.get_value().to_owned()).collect();
        assert_eq!(values, vec!["/build/should-review", ".cache"]);
    }

    #[test]
    fn pending_patterns_filters_by_current_prefix() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join(crate::filenames::DECISION_FILE_NAME),
            "? /build/foo\n? /dist/bar\n",
        )
        .unwrap();

        let result = pending_patterns_in(dir.path(), OsStr::new("/build"));

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].get_value(), "/build/foo");
    }

    #[test]
    fn pending_patterns_is_empty_with_no_decision_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(pending_patterns_in(dir.path(), OsStr::new("")).is_empty());
    }

    #[test]
    fn registered_projects_filters_by_current_prefix() {
        let data_dir = tempfile::tempdir().unwrap();
        std::fs::write(
            data_dir
                .path()
                .join(crate::filenames::PROJECT_ROOTS_FILE_NAME),
            "/home/user1/projects/foo\n/home/user1/projects/bar\n",
        )
        .unwrap();

        let result = registered_projects_in(data_dir.path(), OsStr::new("/home/user1/projects/f"));

        assert_eq!(result.len(), 1);
        assert_eq!(result[0].get_value(), "/home/user1/projects/foo");
    }

    #[test]
    fn registered_projects_is_empty_with_no_list_file() {
        let data_dir = tempfile::tempdir().unwrap();
        assert!(registered_projects_in(data_dir.path(), OsStr::new("")).is_empty());
    }
}
