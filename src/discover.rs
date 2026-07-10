//! `ghostvolumes discover` (§7): bootstraps `projects.d` entries from
//! subvolumes that already exist (pre-adoption, or manual `btrfs
//! subvolume create` usage before GhostVolumes was installed).

use std::path::{Path, PathBuf};

use crate::btrfs;

pub struct DiscoveredMatch {
    pub parent: PathBuf,
    pub name: String,
}

/// Stat-walks from `start` (skipping `.git`, never descending into a
/// watched-name match whether or not it turns out to be a subvolume —
/// walking into a multi-gigabyte `node_modules` tree looking for more
/// matches would be pointless and slow), recording every watched-name
/// entry that's a real subvolume (inode 256).
pub fn walk(
    start: &Path,
    max_depth: Option<u32>,
    watched_names: &[String],
) -> Vec<DiscoveredMatch> {
    let mut matches = Vec::new();
    walk_inner(start, max_depth, watched_names, 0, &mut matches);
    matches
}

fn walk_inner(
    dir: &Path,
    max_depth: Option<u32>,
    watched_names: &[String],
    depth: u32,
    matches: &mut Vec<DiscoveredMatch>,
) {
    if let Some(max) = max_depth
        && depth > max
    {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".git" {
            continue;
        }
        let path = entry.path();
        if watched_names.iter().any(|w| w == name_str.as_ref()) {
            if btrfs::is_subvolume(&path).unwrap_or(false) {
                matches.push(DiscoveredMatch {
                    parent: dir.to_path_buf(),
                    name: name_str.to_string(),
                });
            }
            continue; // never descend into a watched-name match
        }
        walk_inner(&path, max_depth, watched_names, depth + 1, matches);
    }
}

pub struct ProjectSuggestion {
    pub path: PathBuf,
    pub proactive: Vec<String>,
    pub skipped_git_tracked: Vec<String>,
}

/// Groups matches by parent directory and partitions each parent's
/// names by the git-tracked gate — tracked names are never suggested
/// as `proactive`, but are annotated rather than silently dropped
/// (§4's git-tracked gate, applied at the `discover` call site per §7).
pub fn group_and_gate(
    matches: Vec<DiscoveredMatch>,
    is_git_tracked: impl Fn(&Path) -> bool,
) -> Vec<ProjectSuggestion> {
    let mut by_parent: std::collections::BTreeMap<PathBuf, Vec<String>> = Default::default();
    for m in matches {
        by_parent.entry(m.parent).or_default().push(m.name);
    }

    by_parent
        .into_iter()
        .map(|(parent, names)| {
            let mut proactive = Vec::new();
            let mut skipped_git_tracked = Vec::new();
            for name in names {
                if is_git_tracked(&parent.join(&name)) {
                    skipped_git_tracked.push(name);
                } else {
                    proactive.push(name);
                }
            }
            proactive.sort();
            skipped_git_tracked.sort();
            ProjectSuggestion {
                path: parent,
                proactive,
                skipped_git_tracked,
            }
        })
        .collect()
}

/// Renders suggestions as ready-to-paste TOML matching `projects.d`'s
/// `[[project]]` shape — a project block only for parents with at
/// least one surviving (non-git-tracked) name, plus a comment for any
/// git-tracked names skipped there, so nothing is silently omitted.
pub fn format_toml(suggestions: &[ProjectSuggestion]) -> String {
    let mut out = String::new();
    for s in suggestions {
        if !s.skipped_git_tracked.is_empty() {
            out.push_str(&format!(
                "# skipped (git-tracked): {} in {}\n",
                s.skipped_git_tracked.join(", "),
                s.path.display()
            ));
        }
        if s.proactive.is_empty() {
            continue;
        }
        out.push_str("[[project]]\n");
        out.push_str(&format!("path = {:?}\n", s.path.display().to_string()));
        out.push_str(&format!(
            "proactive = [{}]\n",
            s.proactive
                .iter()
                .map(|n| format!("{n:?}"))
                .collect::<Vec<_>>()
                .join(", ")
        ));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::btrfs_scratch_dir;

    fn watched() -> Vec<String> {
        vec!["node_modules".to_string(), "target".to_string()]
    }

    #[test]
    fn finds_subvolume_matching_watched_name() {
        let dir = btrfs_scratch_dir();
        btrfs::create_subvolume(dir.path(), "node_modules").unwrap();

        let matches = walk(dir.path(), None, &watched());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].parent, dir.path());
        assert_eq!(matches[0].name, "node_modules");
    }

    #[test]
    fn plain_directory_with_watched_name_is_not_a_match() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir(dir.path().join("node_modules")).unwrap();

        assert!(walk(dir.path(), None, &watched()).is_empty());
    }

    #[test]
    fn does_not_descend_into_a_matched_subvolume() {
        let dir = btrfs_scratch_dir();
        btrfs::create_subvolume(dir.path(), "node_modules").unwrap();
        // If discover recursed into it, it would find this nested
        // "target" subvolume too - it must not.
        btrfs::create_subvolume(&dir.path().join("node_modules"), "target").unwrap();

        let matches = walk(dir.path(), None, &watched());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "node_modules");
    }

    #[test]
    fn skips_dot_git_directory() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join(".git/node_modules")).unwrap();
        // Even if it were a subvolume, .git itself must never be
        // descended into.
        assert!(walk(dir.path(), None, &watched()).is_empty());
    }

    #[test]
    fn finds_nested_matches_in_unrelated_subdirectories() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("projects/app")).unwrap();
        btrfs::create_subvolume(&dir.path().join("projects/app"), "target").unwrap();

        let matches = walk(dir.path(), None, &watched());
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].parent, dir.path().join("projects/app"));
        assert_eq!(matches[0].name, "target");
    }

    #[test]
    fn max_depth_limits_recursion() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("a/b/c")).unwrap();
        btrfs::create_subvolume(&dir.path().join("a/b/c"), "target").unwrap();

        // depth 0 = dir itself, 1 = a/, 2 = a/b/, 3 = a/b/c/ contents
        assert!(walk(dir.path(), Some(2), &watched()).is_empty());
        assert_eq!(walk(dir.path(), Some(3), &watched()).len(), 1);
    }

    #[test]
    fn group_and_gate_separates_tracked_from_untracked() {
        let matches = vec![
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "node_modules".to_string(),
            },
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "target".to_string(),
            },
        ];
        let suggestions = group_and_gate(matches, |path| path.ends_with("target"));
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].proactive, vec!["node_modules".to_string()]);
        assert_eq!(
            suggestions[0].skipped_git_tracked,
            vec!["target".to_string()]
        );
    }

    #[test]
    fn group_and_gate_groups_by_parent() {
        let matches = vec![
            DiscoveredMatch {
                parent: PathBuf::from("/a"),
                name: "target".to_string(),
            },
            DiscoveredMatch {
                parent: PathBuf::from("/b"),
                name: "node_modules".to_string(),
            },
        ];
        let suggestions = group_and_gate(matches, |_| false);
        assert_eq!(suggestions.len(), 2);
    }

    #[test]
    fn format_toml_emits_parseable_project_blocks() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/home/user1/projects/big-frontend"),
            proactive: vec!["node_modules".to_string()],
            skipped_git_tracked: vec![],
        }];
        let text = format_toml(&suggestions);
        let parsed = crate::config::parse_projects(&text).unwrap();
        assert_eq!(parsed.project.len(), 1);
        assert_eq!(parsed.project[0].path, "/home/user1/projects/big-frontend");
        assert_eq!(
            parsed.project[0].proactive,
            vec!["node_modules".to_string()]
        );
    }

    #[test]
    fn format_toml_annotates_skipped_names_without_emitting_them_as_proactive() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/home/user1/projects/x"),
            proactive: vec![],
            skipped_git_tracked: vec!["vendor".to_string()],
        }];
        let text = format_toml(&suggestions);
        assert!(text.contains("skipped"));
        assert!(text.contains("vendor"));
        assert!(!text.contains("[[project]]")); // nothing left to suggest
    }

    #[test]
    fn format_toml_mixes_active_and_skipped_for_same_parent() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/home/user1/projects/x"),
            proactive: vec!["node_modules".to_string()],
            skipped_git_tracked: vec!["vendor".to_string()],
        }];
        let text = format_toml(&suggestions);
        assert!(text.contains("skipped (git-tracked): vendor"));
        let parsed = crate::config::parse_projects(&text).unwrap();
        assert_eq!(
            parsed.project[0].proactive,
            vec!["node_modules".to_string()]
        );
    }
}
