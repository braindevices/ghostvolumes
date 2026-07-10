//! The flat `compiled.tsv` cache format (§8.0): tab-separated
//! `(prefix, name)` pairs that the LD_PRELOAD shim and `ensure` both
//! read, so neither has to parse TOML. Global-default rows are keyed
//! by each entry in `roots` (NOT a hardcoded `/`) — this is what makes
//! `roots.d` a real, cheap, first-line filter rather than a paper
//! requirement: a row's prefix is never broader than an actual
//! configured root, so a path outside every root simply has no
//! matching row. A project's row lists only that project's own
//! `watch ∪ proactive` names, not the global defaults again — a reader
//! matches a target path against *every* row whose prefix is an
//! ancestor and unions the names, so a root row's names always apply
//! on top of any project-specific row nested under it. This reproduces
//! `pathmatch::resolve_watch_names`'s result without duplicating the
//! global defaults into every project's rows.
//!
//! `parse`/`names_for` (the reader half, needed by both this crate and
//! the LD_PRELOAD shim) live in `shim/cache_core.rs` and are pulled in
//! verbatim below — see that file's doc comment for why.

use std::collections::BTreeSet;

use crate::merge::MergedConfig;

include!("../shim/cache_core.rs");

/// Renders the merged config into `compiled.tsv` text. Writer-only;
/// the shim never calls this (it only reads compiled.tsv), so it stays
/// out of `shim/cache_core.rs` and can freely depend on `MergedConfig`
/// (serde-derived, not shim-safe).
pub fn compile(config: &MergedConfig) -> String {
    let mut out = String::new();
    for root in &config.roots {
        for name in &config.global_defaults {
            out.push_str(root);
            out.push('\t');
            out.push_str(name);
            out.push('\n');
        }
    }
    for project in &config.projects {
        let proactive_set: BTreeSet<&str> = project.proactive.iter().map(String::as_str).collect();
        let names: BTreeSet<&str> = project
            .watch
            .iter()
            .chain(project.proactive.iter())
            .map(String::as_str)
            .collect();
        for name in names {
            out.push_str(&project.path);
            out.push('\t');
            out.push_str(name);
            if proactive_set.contains(name) {
                out.push_str("\tproactive");
            }
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod compile_tests {
    use super::*;
    use crate::config::ProjectEntry;
    use crate::pathmatch;
    use std::path::Path;

    fn project(path: &str, watch: &[&str], proactive: &[&str]) -> ProjectEntry {
        ProjectEntry {
            path: path.to_string(),
            watch: watch.iter().map(|s| s.to_string()).collect(),
            proactive: proactive.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn sample_config() -> MergedConfig {
        MergedConfig {
            roots: vec!["/".to_string()],
            global_defaults: vec![
                "node_modules".to_string(),
                "target".to_string(),
                ".venv".to_string(),
                "build".to_string(),
            ],
            projects: vec![project(
                "/home/user1/projects/big-frontend",
                &["dist", ".next"],
                &["node_modules"],
            )],
        }
    }

    #[test]
    fn compile_matches_plan_example_shape() {
        let text = compile(&sample_config());
        // Global defaults get the "/" prefix, one row each, never
        // marked proactive.
        assert!(text.contains("/\tnode_modules\n"));
        assert!(text.contains("/\ttarget\n"));
        assert!(text.contains("/\t.venv\n"));
        assert!(text.contains("/\t.venv\n"));
        assert!(text.contains("/\tbuild\n"));
        // Project rows list only its own watch ∪ proactive, not the
        // global defaults again. "node_modules" is in project.proactive
        // so it carries the marker; "dist"/".next" (watch-only) don't.
        assert!(text.contains("/home/user1/projects/big-frontend\tdist\n"));
        assert!(text.contains("/home/user1/projects/big-frontend\t.next\n"));
        assert!(text.contains("/home/user1/projects/big-frontend\tnode_modules\tproactive\n"));
        assert!(!text.contains("/home/user1/projects/big-frontend\ttarget\n"));
    }

    #[test]
    fn parse_round_trips_compile_output() {
        let config = sample_config();
        let rows = parse(&compile(&config));
        assert_eq!(rows.len(), 4 + 3); // 4 global + 3 project-specific
        assert!(rows.contains(&("/".to_string(), "target".to_string(), false)));
        assert!(rows.contains(&(
            "/home/user1/projects/big-frontend".to_string(),
            "dist".to_string(),
            false
        )));
        assert!(rows.contains(&(
            "/home/user1/projects/big-frontend".to_string(),
            "node_modules".to_string(),
            true
        )));
    }

    #[test]
    fn names_for_matches_pathmatch_resolve_watch_names() {
        let config = sample_config();
        let rows = parse(&compile(&config));

        for path in [
            "/tmp/somewhere/else",
            "/home/user1/projects/big-frontend",
            "/home/user1/projects/big-frontend/packages/foo",
            "/home/user1/projects/big-frontend2/sub",
        ] {
            let expected = pathmatch::resolve_watch_names(
                Path::new(path),
                &config.roots,
                &config.global_defaults,
                &config.projects,
            );
            let actual = names_for(&rows, Path::new(path));
            assert_eq!(actual, expected, "mismatch for path {path}");
        }
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
            roots: vec!["/home/user1".to_string(), "/data/workspaces".to_string()],
            global_defaults: vec!["node_modules".to_string()],
            projects: vec![],
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
}
