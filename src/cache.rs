//! The flat `compiled.tsv` cache format: tab-separated `(prefix, name)`
//! pairs, so readers needn't parse TOML. Rows are keyed by each entry in
//! `roots`, never a hardcoded `/`. The reader half (`parse`/`names_for`/
//! `longest_matching_prefix`) lives in `cache_core.rs`, pulled in below.

use crate::merge::MergedConfig;

include!("cache_core.rs");

/// Renders the merged config into `compiled.tsv` text. Writer-only;
/// each root's `watches` is already fully resolved, so this just
/// flattens the per-root lists into rows. A root or name that can't be
/// written as one `prefix\tname` row (see `representable_path`; names
/// also can't contain `/`) is skipped with a warning.
pub fn compile(config: &MergedConfig) -> String {
    let mut out = String::new();
    for root in &config.roots {
        if !crate::decision::representable_path(&root.path) {
            eprintln!(
                "warning: skipping root {:?}: not representable in compiled.tsv",
                root.path
            );
            continue;
        }
        for name in &root.watches {
            if name.contains('/') || !crate::decision::representable_path(name) {
                eprintln!(
                    "warning: skipping watched name {name:?}: must be one plain directory name"
                );
                continue;
            }
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
    #[test]
    fn compile_skips_roots_and_names_that_would_break_a_row() {
        let config = MergedConfig {
            roots: vec![
                ResolvedRoot {
                    path: "/a\nb".to_string(),
                    watches: vec!["node_modules".to_string()],
                },
                ResolvedRoot {
                    path: "/ok".to_string(),
                    watches: ["target", "x/y", "bad\tname", "**", " pad"]
                        .map(String::from)
                        .to_vec(),
                },
            ],
            ignore: Vec::new(),
            delete_convert_backup: false,
        };
        assert_eq!(compile(&config), "/ok\ttarget\n");
    }

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
            delete_convert_backup: false,
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
            delete_convert_backup: false,
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
            delete_convert_backup: false,
        };
        let text = compile(&config);
        assert_eq!(text, "/\tnode_modules\n/home\tdist\n");
    }
}
