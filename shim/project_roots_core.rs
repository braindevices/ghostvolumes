// Project-roots list (§3): a plain-text file, one path per line, giving
// the decision-file walk-up a narrower stopping boundary than the
// broader `roots.d` entries alone. Deliberately not TOML/compiled:
// read live, same philosophy as decision files (§3, §5). Dependency-free,
// shared via `include!` (CLI) / `mod` (shim).

/// Strips a single trailing `/` from `path` — except when `path` is
/// exactly `"/"`, which must keep it — so the same directory compares
/// and displays identically regardless of a trailing slash (shell
/// tab-completion often appends one; `convert`'s boundaries never do).
#[allow(dead_code)]
pub fn normalize_root_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Parses the file's raw text into its list of registered paths - one
/// non-empty, trimmed, slash-normalized line each. Normalizing at parse
/// time (not just at write time) means every reader sees a consistent
/// view without needing the raw file rewritten.
#[allow(dead_code)]
pub fn parse(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(normalize_root_path)
        .collect()
}

/// `true` iff `path` is not already present in `text`, i.e. whether an
/// append is needed. Callers do the actual I/O as a single `O_APPEND`
/// write, never a full rewrite: concurrent appends are safe, rewrites
/// are not.
#[allow(dead_code)]
pub fn needs_append(text: &str, path: &str) -> bool {
    let normalized = normalize_root_path(path);
    !parse(text).iter().any(|existing| existing == &normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_splits_on_lines_and_trims() {
        let text = "/home/user1/projects/app\n  /data/workspaces/other  \n";
        assert_eq!(
            parse(text),
            vec![
                "/home/user1/projects/app".to_string(),
                "/data/workspaces/other".to_string(),
            ]
        );
    }

    #[test]
    fn parse_ignores_blank_lines() {
        let text = "/a\n\n\n/b\n";
        assert_eq!(parse(text), vec!["/a".to_string(), "/b".to_string()]);
    }

    #[test]
    fn parse_empty_text_yields_empty() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn needs_append_true_when_path_absent() {
        assert!(needs_append("/a\n/b\n", "/c"));
    }

    #[test]
    fn needs_append_false_when_path_already_present() {
        assert!(!needs_append("/a\n/b\n", "/b"));
    }

    #[test]
    fn needs_append_true_for_empty_file() {
        assert!(needs_append("", "/a"));
    }

    #[test]
    fn normalize_root_path_strips_a_trailing_slash() {
        assert_eq!(normalize_root_path("/foo/bar/"), "/foo/bar");
        assert_eq!(normalize_root_path("/foo/bar///"), "/foo/bar");
    }

    #[test]
    fn normalize_root_path_leaves_a_path_with_no_trailing_slash_alone() {
        assert_eq!(normalize_root_path("/foo/bar"), "/foo/bar");
    }

    #[test]
    fn normalize_root_path_keeps_the_bare_root_as_a_single_slash() {
        assert_eq!(normalize_root_path("/"), "/");
        assert_eq!(normalize_root_path("///"), "/");
    }

    #[test]
    fn parse_normalizes_a_trailing_slash_on_every_line() {
        let text = "/home/user1/projects/app/\n/data/workspaces/other\n";
        assert_eq!(
            parse(text),
            vec![
                "/home/user1/projects/app".to_string(),
                "/data/workspaces/other".to_string(),
            ]
        );
    }

    #[test]
    fn needs_append_recognizes_a_stored_path_regardless_of_trailing_slash_on_either_side() {
        assert!(!needs_append("/a\n", "/a/"));
        assert!(!needs_append("/a/\n", "/a"));
    }
}
