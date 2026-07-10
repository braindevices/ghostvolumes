//! Reference implementation of the path-matching rules from plan §4,
//! operating directly on `MergedConfig` rather than the compiled
//! cache. **Not called by production code** — the actual runtime paths
//! (the LD_PRELOAD shim and `ensure`) both read `compiled.tsv` instead
//! (via `cache::names_for`/`cache::proactive_project_for`), since
//! re-parsing/re-merging TOML on every intercepted syscall or `cd`
//! would defeat the zero-overhead goal. This module exists purely as a
//! tested spec: `cache.rs`'s `names_for_matches_pathmatch_resolve_watch_names`
//! test proves the compiled-cache implementation produces byte-for-byte
//! the same result as this one across representative paths, so the two
//! can't silently drift apart even though only one of them actually runs.
//!
//! `is_git_tracked` (the other half of §4's git-tracked gate) is
//! deliberately kept out of this module — it needs to shell out to
//! `git`, so it lives in `crate::git` (Step 5) and is FS/process
//! dependent. This module stays pure and FS-free so it's cheaply
//! unit-testable: `resolve_proactive_names` here returns the
//! *candidate* names for a project match; filtering out git-tracked
//! ones would be a separate step composed at the call site.
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::Path;

use crate::config::ProjectEntry;

/// `true` iff `path` is under (or equal to) at least one entry in
/// `roots`. This is the cheapest possible rejection and, per plan §4,
/// must be the first thing both functions below check — made an
/// explicit, unconditional first step here rather than a documented
/// precondition callers have to remember (an implicit version of this
/// exact check being skipped is what caused the compiled.tsv
/// hardcoded-"/" bug this module's roots parameter now prevents).
pub fn is_under_any_root(roots: &[String], path: &Path) -> bool {
    roots.iter().any(|root| path.starts_with(Path::new(root)))
}

/// The project entry whose `path` is the longest (most specific)
/// ancestor-or-self of `path`, if any. Longest-prefix-wins is the tie
/// break for nested project entries (not specified in the plan, since
/// project entries aren't expected to usefully nest in practice, but
/// a project further down the tree is the more specific match).
fn find_project<'a>(projects: &'a [ProjectEntry], path: &Path) -> Option<&'a ProjectEntry> {
    projects
        .iter()
        .filter(|p| path.starts_with(Path::new(&p.path)))
        .max_by_key(|p| p.path.len())
}

/// Reference spec for the LD_PRELOAD shim's reactive matching (see
/// module docs — not what actually runs; `cache::names_for` does).
/// Outside every configured root, nothing applies at all. Inside a
/// root, always includes the global defaults regardless of project
/// match — the safety net described in §4 that catches unexpected
/// paths even without a project entry.
pub fn resolve_watch_names(
    path: &Path,
    roots: &[String],
    global_defaults: &[String],
    projects: &[ProjectEntry],
) -> BTreeSet<String> {
    if !is_under_any_root(roots, path) {
        return BTreeSet::new();
    }
    let mut names: BTreeSet<String> = global_defaults.iter().cloned().collect();
    if let Some(project) = find_project(projects, path) {
        names.extend(project.watch.iter().cloned());
        names.extend(project.proactive.iter().cloned());
    }
    names
}

/// Reference spec for the cd-hook's proactive matching (see module
/// docs — not what actually runs; `cache::proactive_project_for` does).
/// Outside every configured root, or no project match => no names at
/// all — proactive creation requires an explicit per-project opt-in.
/// Returns raw candidate names; the git-tracked gate would be applied
/// by the caller.
pub fn resolve_proactive_names<'a>(
    path: &Path,
    roots: &[String],
    projects: &'a [ProjectEntry],
) -> &'a [String] {
    if !is_under_any_root(roots, path) {
        return &[];
    }
    match find_project(projects, path) {
        Some(project) => &project.proactive,
        None => &[],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(path: &str, watch: &[&str], proactive: &[&str]) -> ProjectEntry {
        ProjectEntry {
            path: path.to_string(),
            watch: watch.iter().map(|s| s.to_string()).collect(),
            proactive: proactive.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn set(names: &[&str]) -> BTreeSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    /// Every existing test predates the `roots` parameter and wants
    /// "matches everywhere" semantics, so `["/"]` is the neutral root
    /// list that doesn't change any of their expectations.
    fn everywhere() -> Vec<String> {
        vec!["/".to_string()]
    }

    #[test]
    fn no_project_match_returns_global_defaults_only() {
        let projects = vec![project(
            "/home/user1/projects/a",
            &["dist"],
            &["node_modules"],
        )];
        let defaults = vec!["node_modules".to_string(), "target".to_string()];
        let result = resolve_watch_names(
            Path::new("/tmp/somewhere/else"),
            &everywhere(),
            &defaults,
            &projects,
        );
        assert_eq!(result, set(&["node_modules", "target"]));
    }

    #[test]
    fn project_match_unions_defaults_watch_and_proactive() {
        let projects = vec![project(
            "/home/user1/projects/big-frontend",
            &["dist", ".next"],
            &["node_modules"],
        )];
        let defaults = vec!["node_modules".to_string(), "target".to_string()];
        let result = resolve_watch_names(
            Path::new("/home/user1/projects/big-frontend/packages/foo"),
            &everywhere(),
            &defaults,
            &projects,
        );
        assert_eq!(result, set(&["node_modules", "target", "dist", ".next"]));
    }

    #[test]
    fn prefix_match_requires_component_boundary_not_string_prefix() {
        // "/home/user1/projects/big-frontend2" must NOT match the
        // "big-frontend" project even though it shares a string prefix.
        let projects = vec![project("/home/user1/projects/big-frontend", &["dist"], &[])];
        let defaults = vec!["node_modules".to_string()];
        let result = resolve_watch_names(
            Path::new("/home/user1/projects/big-frontend2/sub"),
            &everywhere(),
            &defaults,
            &projects,
        );
        assert_eq!(result, set(&["node_modules"]));
    }

    #[test]
    fn nested_projects_longest_prefix_wins() {
        let projects = vec![
            project("/home/user1/projects", &["outer-only"], &[]),
            project("/home/user1/projects/inner", &["inner-only"], &[]),
        ];
        let defaults = vec!["node_modules".to_string()];
        let result = resolve_watch_names(
            Path::new("/home/user1/projects/inner/src"),
            &everywhere(),
            &defaults,
            &projects,
        );
        assert_eq!(result, set(&["node_modules", "inner-only"]));
    }

    #[test]
    fn exact_project_root_matches() {
        let projects = vec![project("/home/user1/projects/rust-app", &[], &["target"])];
        let defaults = vec!["node_modules".to_string()];
        let result = resolve_watch_names(
            Path::new("/home/user1/projects/rust-app"),
            &everywhere(),
            &defaults,
            &projects,
        );
        assert_eq!(result, set(&["node_modules", "target"]));
    }

    #[test]
    fn proactive_no_project_match_is_empty() {
        let projects = vec![project("/home/user1/projects/a", &[], &["target"])];
        let result = resolve_proactive_names(Path::new("/tmp/elsewhere"), &everywhere(), &projects);
        assert!(result.is_empty());
    }

    #[test]
    fn proactive_project_match_returns_proactive_names() {
        let projects = vec![project(
            "/home/user1/projects/rust-app",
            &["dist"],
            &["target"],
        )];
        let result = resolve_proactive_names(
            Path::new("/home/user1/projects/rust-app"),
            &everywhere(),
            &projects,
        );
        assert_eq!(result, &["target".to_string()]);
    }

    #[test]
    fn path_outside_every_configured_root_gets_nothing_even_with_project_match() {
        // A restricted roots list (not "/") must reject paths outside
        // it even if a [[project]] entry would otherwise match — the
        // bug this module's `roots` parameter exists to prevent.
        let roots = vec!["/home/user1".to_string()];
        let projects = vec![project("/data/other/rust-app", &[], &["target"])];
        let defaults = vec!["node_modules".to_string()];

        let watch = resolve_watch_names(
            Path::new("/data/other/rust-app"),
            &roots,
            &defaults,
            &projects,
        );
        assert!(
            watch.is_empty(),
            "path outside every root must get no names at all"
        );

        let proactive =
            resolve_proactive_names(Path::new("/data/other/rust-app"), &roots, &projects);
        assert!(proactive.is_empty());
    }

    #[test]
    fn path_inside_one_of_several_roots_still_matches() {
        let roots = vec!["/home/user1".to_string(), "/data/workspaces".to_string()];
        let defaults = vec!["node_modules".to_string()];
        let result = resolve_watch_names(
            Path::new("/data/workspaces/some-project"),
            &roots,
            &defaults,
            &[],
        );
        assert_eq!(result, set(&["node_modules"]));
    }

    #[test]
    fn is_under_any_root_checks_component_boundaries() {
        let roots = vec!["/home/user1".to_string()];
        assert!(is_under_any_root(
            &roots,
            Path::new("/home/user1/projects/a")
        ));
        assert!(is_under_any_root(&roots, Path::new("/home/user1")));
        assert!(!is_under_any_root(
            &roots,
            Path::new("/home/user12/projects/a")
        ));
        assert!(!is_under_any_root(&roots, Path::new("/tmp")));
    }
}
