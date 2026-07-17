// Reader + matcher for the flat `compiled.tsv` cache format (§8.0).
// Dependency-free (`std` only), shared between the CLI (`include!`) and
// the shim (`mod`). The writer (`compile()`) stays in `src/cache.rs`
// only, since it needs `MergedConfig` — the shim never writes this
// file, only reads it. Rows are plain `(prefix, name)` pairs.

// Fully-qualified paths throughout, not `use` at file scope: this file
// is included both mid-file into src/cache.rs and as its own `mod` in
// the shim, two different scopes a bare `use` here can't serve both of.

/// Parses `compiled.tsv` text back into `(prefix, name)` pairs —
/// `str::split_once('\t')` per line, no external crate.
pub fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (prefix, name) = line.split_once('\t')?;
            Some((prefix.to_string(), name.to_string()))
        })
        .collect()
}

/// The names that apply to `path`: the union of every row whose prefix
/// is an ancestor-or-self of `path`. This is the shim's reactive
/// matching logic; dead code from the main crate's own perspective but
/// alive via the shim's separate `mod`-based compilation.
#[allow(dead_code)]
pub fn names_for(
    rows: &[(String, String)],
    path: &std::path::Path,
) -> std::collections::BTreeSet<String> {
    rows.iter()
        .filter(|(prefix, _)| path.starts_with(std::path::Path::new(prefix)))
        .map(|(_, name)| name.clone())
        .collect()
}


#[allow(dead_code)]
pub fn longest_matching_prefix(
    rows: &[(String, String)],
    path: &std::path::Path,
) -> Option<String> {
    rows.iter()
        .map(|(prefix, _)| prefix.as_str())
        .filter(|prefix| path.starts_with(std::path::Path::new(prefix)))
        .max_by_key(|prefix| prefix.len())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;
    use std::path::Path;

    #[test]
    fn parse_splits_prefix_and_name_on_tab() {
        let rows = parse("/home/user1\tnode_modules\n/data\ttarget\n");
        assert_eq!(
            rows,
            vec![
                ("/home/user1".to_string(), "node_modules".to_string()),
                ("/data".to_string(), "target".to_string()),
            ]
        );
    }

    #[test]
    fn parse_ignores_lines_without_a_tab() {
        assert!(parse("not-a-valid-row\n").is_empty());
    }

    #[test]
    fn parse_empty_text_yields_empty() {
        assert!(parse("").is_empty());
    }

    #[test]
    fn names_for_unions_every_matching_ancestor_row() {
        let rows = vec![
            ("/home/user1".to_string(), "node_modules".to_string()),
            ("/home/user1".to_string(), "target".to_string()),
            ("/home/user1/projects/app".to_string(), "dist".to_string()),
        ];
        let names = names_for(&rows, Path::new("/home/user1/projects/app/sub"));
        assert_eq!(
            names,
            BTreeSet::from([
                "node_modules".to_string(),
                "target".to_string(),
                "dist".to_string()
            ])
        );
    }

    #[test]
    fn names_for_excludes_paths_outside_every_row_prefix() {
        let rows = vec![("/home/user1".to_string(), "node_modules".to_string())];
        assert!(names_for(&rows, Path::new("/etc/somewhere")).is_empty());
    }

    #[test]
    fn longest_matching_prefix_prefers_the_narrower_of_two_ancestor_matches() {
        let rows = vec![
            ("/home/user1".to_string(), "node_modules".to_string()),
            ("/home/user1/projects/app".to_string(), "dist".to_string()),
        ];
        let matched = longest_matching_prefix(&rows, Path::new("/home/user1/projects/app/src"));
        assert_eq!(matched, Some("/home/user1/projects/app".to_string()));
    }

    #[test]
    fn longest_matching_prefix_none_when_nothing_matches() {
        let rows = vec![("/home/user1".to_string(), "node_modules".to_string())];
        assert_eq!(
            longest_matching_prefix(&rows, Path::new("/etc/elsewhere")),
            None
        );
    }

    #[test]
    fn longest_matching_prefix_matches_the_prefix_itself() {
        let rows = vec![("/home/user1".to_string(), "node_modules".to_string())];
        assert_eq!(
            longest_matching_prefix(&rows, Path::new("/home/user1")),
            Some("/home/user1".to_string())
        );
    }

    #[test]
    fn longest_matching_prefix_respects_component_boundaries() {
        let rows = vec![(
            "/home/user1/projects/big-frontend".to_string(),
            "dist".to_string(),
        )];
        assert_eq!(
            longest_matching_prefix(&rows, Path::new("/home/user1/projects/big-frontend2/sub")),
            None
        );
    }

    #[test]
    fn names_for_respects_component_boundaries_not_string_prefix() {
        let rows = vec![(
            "/home/user1/projects/big-frontend".to_string(),
            "dist".to_string(),
        )];
        // "big-frontend2" shares a string prefix with "big-frontend"
        // but is not actually under it.
        assert!(names_for(&rows, Path::new("/home/user1/projects/big-frontend2/sub")).is_empty());
    }
}
