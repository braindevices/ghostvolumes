//! The flat `compiled.tsv` cache format (§8.0): tab-separated
//! `(prefix, name)` pairs that the LD_PRELOAD shim reads, so it doesn't
//! have to parse TOML. Rows are keyed by each entry in `roots` (NOT a
//! hardcoded `/`) — this is what makes `roots.d` a real, cheap,
//! first-line filter rather than a paper requirement: a row's prefix is
//! never broader than an actual configured root, so a path outside
//! every root simply has no matching row.
//!
//! Per-project rows (a project's own `watch ∪ proactive` names) were
//! removed along with per-project `.ghostvolumes.toml`/`projects.d`
//! entirely (ai-work/tasks/decision-model.plan.md §7) — decision files
//! are the entire per-project mechanism now; `compiled.tsv` only ever
//! needs the global default names under each configured root.
//!
//! `parse`/`names_for`/`longest_matching_prefix` (the reader half,
//! needed by both this crate and the LD_PRELOAD shim) live in
//! `shim/cache_core.rs` and are pulled in verbatim below — see that
//! file's doc comment for why.

use crate::merge::MergedConfig;

include!("../shim/cache_core.rs");

/// Renders the merged config into `compiled.tsv` text. Writer-only;
/// the shim never calls this (it only reads compiled.tsv), so it stays
/// out of `shim/cache_core.rs` and can freely depend on `MergedConfig`
/// (serde-derived, not shim-safe). Each root's `watches` is already
/// fully resolved by `merge.rs` (its own override, or `default-watches`,
/// with disabled roots dropped already) — this just flattens the
/// per-root lists into rows, no cross-product needed here anymore.
pub fn compile(config: &MergedConfig) -> String {
    let mut out = String::new();
    for root in &config.roots {
        for name in &root.watches {
            out.push_str(&root.path);
            out.push('\t');
            out.push_str(name);
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod compile_tests {
    use super::*;
    use crate::merge::ResolvedRoot;
    use std::path::Path;

    fn sample_config() -> MergedConfig {
        MergedConfig {
            roots: vec![ResolvedRoot {
                path: "/".to_string(),
                watches: vec![
                    "node_modules".to_string(),
                    "target".to_string(),
                    ".venv".to_string(),
                    "build".to_string(),
                ],
            }],
            ignore: Vec::new(),
        }
    }

    #[test]
    fn compile_matches_plan_example_shape() {
        let text = compile(&sample_config());
        assert!(text.contains("/\tnode_modules\n"));
        assert!(text.contains("/\ttarget\n"));
        assert!(text.contains("/\t.venv\n"));
        assert!(text.contains("/\tbuild\n"));
    }

    #[test]
    fn parse_round_trips_compile_output() {
        let config = sample_config();
        let rows = parse(&compile(&config));
        assert_eq!(rows.len(), 4);
        assert!(rows.contains(&("/".to_string(), "target".to_string())));
    }

    #[test]
    fn empty_config_compiles_to_empty_cache() {
        let config = MergedConfig::default();
        assert_eq!(compile(&config), "");
        assert!(parse("").is_empty());
    }

    #[test]
    fn restricted_roots_produce_root_keyed_rows_not_a_hardcoded_slash() {
        let config = MergedConfig {
            roots: vec![
                ResolvedRoot {
                    path: "/home/user1".to_string(),
                    watches: vec!["node_modules".to_string()],
                },
                ResolvedRoot {
                    path: "/data/workspaces".to_string(),
                    watches: vec!["node_modules".to_string()],
                },
            ],
            ignore: Vec::new(),
        };
        let text = compile(&config);
        assert_eq!(
            text,
            "/home/user1\tnode_modules\n/data/workspaces\tnode_modules\n"
        );
        assert!(!text.contains("/\tnode_modules"));

        let rows = parse(&text);
        // A path outside both configured roots gets nothing, even
        // though it would have matched a hardcoded "/" row.
        assert!(names_for(&rows, Path::new("/etc/somewhere/node_modules")).is_empty());
        assert!(!names_for(&rows, Path::new("/data/workspaces/app")).is_empty());
    }

    #[test]
    fn each_root_uses_its_own_already_resolved_watch_list() {
        let config = MergedConfig {
            roots: vec![
                ResolvedRoot {
                    path: "/".to_string(),
                    watches: vec!["node_modules".to_string()],
                },
                ResolvedRoot {
                    path: "/home".to_string(),
                    watches: vec!["dist".to_string()],
                },
            ],
            ignore: Vec::new(),
        };
        let text = compile(&config);
        assert_eq!(text, "/\tnode_modules\n/home\tdist\n");
    }
}
