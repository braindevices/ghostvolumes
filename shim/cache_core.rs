// Reader + matcher for the flat `compiled.tsv` cache format (§8.0).
// Dependency-free (plain `std` only, no crate dependencies) so this
// exact file can be shared between the main CLI (via `include!`, from
// `src/cache.rs`) and the LD_PRELOAD shim (via `mod`, from
// `shim/preload.rs`), which is compiled standalone by bare `rustc` and
// can't link crates.io crates. The writer (`compile()`) lives only in
// `src/cache.rs`, since it needs `MergedConfig` (serde-derived) — the
// shim never writes this file, only reads it.
//
// Rows are `(prefix, name, is_proactive)`. The third column exists
// because the shim's reactive matching (`names_for`) needs the full
// `watch ∪ proactive` union, but `ensure`'s proactive matching
// (`proactive_project_for`) needs *only* the `proactive` subset, never
// `watch`-only names (§4) — that distinction can't be recovered from
// an undifferentiated union, a row has to carry it explicitly.
//
// Plain `//` comments, not `//!`/`///` doc comments: this file gets
// spliced mid-file into src/cache.rs via `include!`, where an inner
// doc comment (`//!`) is only legal at the very start of a file/module
// — it would fail to compile once included partway through another
// file's content.

// Fully-qualified paths throughout (rather than `use` at file scope):
// this file is included both mid-file into src/cache.rs (which already
// has its own `use`s in scope) and as its own `mod cache_core` inside
// the shim (a separate module scope, where a bare `use` here wouldn't
// see anything the includer imported) - qualifying every path keeps
// both contexts unambiguous without duplicate-import errors.

/// Parses `compiled.tsv` text back into `(prefix, name, is_proactive)`
/// triples — `str::split('\t')` per line, no external crate. A row's
/// optional third field is `is_proactive` iff it's literally
/// `"proactive"`; two-column rows (global-default and `watch`-only
/// rows) default to `false`.
pub fn parse(text: &str) -> Vec<(String, String, bool)> {
    text.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(3, '\t');
            let prefix = parts.next()?;
            let name = parts.next()?;
            let is_proactive = parts.next() == Some("proactive");
            Some((prefix.to_string(), name.to_string(), is_proactive))
        })
        .collect()
}

/// The names that apply to `path`: the union of every row whose
/// prefix is an ancestor-or-self of `path`, regardless of the
/// `is_proactive` marker. This is the shim's reactive matching logic —
/// it doesn't care which names are also proactive. Dead code from the
/// main crate's own production-code perspective (only its tests and
/// the shim's separate `mod`-based compilation call it) — allowed
/// rather than removed, since it's very much alive there.
#[allow(dead_code)]
pub fn names_for(
    rows: &[(String, String, bool)],
    path: &std::path::Path,
) -> std::collections::BTreeSet<String> {
    rows.iter()
        .filter(|(prefix, _, _)| path.starts_with(std::path::Path::new(prefix)))
        .map(|(_, name, _)| name.clone())
        .collect()
}

/// The proactive project (if any) whose root is the longest
/// ancestor-or-self of `path`, and its proactive names — used by
/// `ensure` (§6), which needs both *where* to create a name (the
/// project root, not `path` itself, which may be nested deeper) and
/// *which* names (only ones marked `proactive`, never `watch`-only,
/// per §4). Longest-prefix-wins matches `pathmatch::resolve_watch_names`'s
/// tie-break for nested project entries.
pub fn proactive_project_for(
    rows: &[(String, String, bool)],
    path: &std::path::Path,
) -> Option<(String, std::collections::BTreeSet<String>)> {
    let project_root = rows
        .iter()
        .filter(|(prefix, _, is_proactive)| {
            *is_proactive && path.starts_with(std::path::Path::new(prefix))
        })
        .map(|(prefix, _, _)| prefix.as_str())
        .max_by_key(|p| p.len())?
        .to_string();
    let names = rows
        .iter()
        .filter(|(prefix, _, is_proactive)| *is_proactive && *prefix == project_root)
        .map(|(_, name, _)| name.clone())
        .collect();
    Some((project_root, names))
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
                ("/home/user1".to_string(), "node_modules".to_string(), false),
                ("/data".to_string(), "target".to_string(), false),
            ]
        );
    }

    #[test]
    fn parse_recognizes_the_proactive_marker() {
        let rows = parse("/home/user1/app\tnode_modules\tproactive\n/home/user1/app\tdist\n");
        assert_eq!(
            rows,
            vec![
                (
                    "/home/user1/app".to_string(),
                    "node_modules".to_string(),
                    true
                ),
                ("/home/user1/app".to_string(), "dist".to_string(), false),
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
    fn names_for_unions_every_matching_ancestor_row_regardless_of_proactive_marker() {
        let rows = vec![
            ("/home/user1".to_string(), "node_modules".to_string(), false),
            ("/home/user1".to_string(), "target".to_string(), false),
            (
                "/home/user1/projects/app".to_string(),
                "dist".to_string(),
                true,
            ),
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
        let rows = vec![(
            "/home/user1".to_string(),
            "node_modules".to_string(),
            false,
        )];
        assert!(names_for(&rows, Path::new("/etc/somewhere")).is_empty());
    }

    #[test]
    fn names_for_respects_component_boundaries_not_string_prefix() {
        let rows = vec![(
            "/home/user1/projects/big-frontend".to_string(),
            "dist".to_string(),
            false,
        )];
        // "big-frontend2" shares a string prefix with "big-frontend"
        // but is not actually under it.
        assert!(names_for(&rows, Path::new("/home/user1/projects/big-frontend2/sub")).is_empty());
    }

    #[test]
    fn proactive_project_for_excludes_watch_only_names() {
        let rows = vec![
            (
                "/home/user1/app".to_string(),
                "node_modules".to_string(),
                true,
            ),
            ("/home/user1/app".to_string(), "dist".to_string(), false),
        ];
        let (root, names) = proactive_project_for(&rows, Path::new("/home/user1/app")).unwrap();
        assert_eq!(root, "/home/user1/app");
        assert_eq!(names, BTreeSet::from(["node_modules".to_string()]));
    }

    #[test]
    fn proactive_project_for_ignores_global_default_rows() {
        // Global-default (root-keyed) rows are never proactive, even
        // though they apply reactively everywhere under the root.
        let rows = vec![(
            "/home/user1".to_string(),
            "node_modules".to_string(),
            false,
        )];
        assert!(proactive_project_for(&rows, Path::new("/home/user1/anything")).is_none());
    }

    #[test]
    fn proactive_project_for_returns_none_outside_every_row_prefix() {
        let rows = vec![(
            "/home/user1/app".to_string(),
            "node_modules".to_string(),
            true,
        )];
        assert!(proactive_project_for(&rows, Path::new("/etc/elsewhere")).is_none());
    }

    #[test]
    fn proactive_project_for_matches_nested_paths_to_the_project_root() {
        let rows = vec![(
            "/home/user1/app".to_string(),
            "node_modules".to_string(),
            true,
        )];
        let (root, _) =
            proactive_project_for(&rows, Path::new("/home/user1/app/src/deep")).unwrap();
        assert_eq!(root, "/home/user1/app");
    }
}
