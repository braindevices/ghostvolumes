// Reader + matcher for the flat `compiled.tsv` cache format (§8.0).
// Dependency-free (plain `std` only, no crate dependencies) so this
// exact file can be shared between the main CLI (via `include!`, from
// `src/cache.rs`) and the LD_PRELOAD shim (via `mod`, from
// `shim/preload.rs`), which is compiled standalone by bare `rustc` and
// can't link crates.io crates. The writer (`compile()`) lives only in
// `src/cache.rs`, since it needs `MergedConfig` (serde-derived) — the
// shim never writes this file, only reads it.
//
// Rows are `(prefix, name)`. Plain two-column format: the "proactive"
// third column that used to exist here was removed along with `ensure`
// and per-project `.ghostvolumes.toml`/`projects.d`
// (ai-work/tasks/decision-model.plan.md §7) — decision files are the
// entire per-project mechanism now, and `compiled.tsv` only ever needs
// to answer "which names are watched under which root."
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

/// The names that apply to `path`: the union of every row whose
/// prefix is an ancestor-or-self of `path`. This is the shim's
/// reactive matching logic. Dead code from the main crate's own
/// production-code perspective (only its tests and the shim's separate
/// `mod`-based compilation call it) — allowed rather than removed,
/// since it's very much alive there.
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

/// The longest (most specific) row prefix that is an ancestor-or-self
/// of `path` — the decision-file walk-up's stopping boundary
/// (ai-work/tasks/decision-model.plan.md §3). Rows mix broad
/// `roots.d`-derived entries with narrower registered project-root
/// entries; using the longest match rather than just any match means
/// the walk-up stops at the nearest boundary instead of a broader,
/// possibly-shared one further out. `None` if `path` isn't under any
/// row's prefix at all (the existing root/name filter already
/// rejected it before this ever runs).
///
/// Wired into `decide()` (`shim/preload.rs`, via `walkup_boundary`) and
/// into `convert`/`intercept` (CLI-side) - dead code only from the main
/// crate's own perspective (only its tests and the shim's separate
/// `mod`-based compilation call it, same as `names_for` above) -
/// allowed rather than removed.
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
