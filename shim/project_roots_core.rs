// Project-roots list (ai-work/tasks/decision-model.plan.md §3): a
// plain-text file, one path per line, giving the decision-file
// walk-up a narrower stopping boundary than the broader `roots.d`
// entries alone. Deliberately not TOML/compiled: a registered
// project-root path needs no BTRFS validation (unlike `roots.d`), so
// there's no reason to route it through `reload`'s compile step - read
// live, same philosophy as decision files themselves (§3, §5).
//
// Dependency-free (plain `std` only), shared between the main CLI (via
// `include!`, from `src/project_roots.rs`) and the LD_PRELOAD shim (via
// `mod`, from `shim/preload.rs`).
//
// Plain `//` comments, not `//!`/`///`: this file gets spliced
// mid-file into src/project_roots.rs via `include!`.

/// Parses the file's raw text into its list of registered paths - one
/// non-empty, trimmed line each.
#[allow(dead_code)]
pub fn parse(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect()
}

/// `true` iff `path` is not already present in `text` (the file's
/// current content) - i.e., whether an append is needed. Callers do
/// the actual file I/O: a plain, single `O_APPEND` write of just
/// `path`, never a full rewrite - the same concurrency-safe discipline
/// established for decision files and the shim's log file (multiple
/// writers appending is safe by construction; rewriting the whole file
/// is not).
#[allow(dead_code)]
pub fn needs_append(text: &str, path: &str) -> bool {
    !parse(text).iter().any(|existing| existing == path)
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
}
