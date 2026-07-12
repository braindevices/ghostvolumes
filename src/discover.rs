//! `ghostvolumes discover` (ai-work/tasks/decision-model.plan.md §7):
//! finds subvolumes that already exist (pre-adoption, or manual `btrfs
//! subvolume create` usage before GhostVolumes was installed) and
//! suggests decision-file lines to record them — `+ name` per matched
//! name, grouped by the decision file each group belongs in. No
//! git-tracked gating (VCS detection was dropped entirely — the only
//! safety net is now an explicit, recorded human decision, same as
//! everywhere else in this design).

use std::path::{Path, PathBuf};

use crate::{btrfs, decision};

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
    pub names: Vec<String>,
}

/// Groups matches by parent directory — each group is one project's
/// worth of `+ name` lines to add to that directory's own decision
/// file.
pub fn group_by_parent(matches: Vec<DiscoveredMatch>) -> Vec<ProjectSuggestion> {
    let mut by_parent: std::collections::BTreeMap<PathBuf, Vec<String>> = Default::default();
    for m in matches {
        by_parent.entry(m.parent).or_default().push(m.name);
    }
    by_parent
        .into_iter()
        .map(|(parent, mut names)| {
            names.sort();
            names.dedup();
            ProjectSuggestion {
                path: parent,
                names,
            }
        })
        .collect()
}

/// Renders suggestions as ready-to-paste decision-file content: one
/// header comment naming the file to save it as, followed by its
/// `+ name` lines.
pub fn format_decisions(suggestions: &[ProjectSuggestion]) -> String {
    let mut out = String::new();
    for s in suggestions {
        out.push_str(&format!(
            "# {}\n",
            s.path.join(decision::DECISION_FILE_NAME).display()
        ));
        for name in &s.names {
            out.push_str(&format!("+ {name}\n"));
        }
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
    fn group_by_parent_groups_and_dedupes() {
        let matches = vec![
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "node_modules".to_string(),
            },
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "target".to_string(),
            },
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "target".to_string(),
            },
        ];
        let suggestions = group_by_parent(matches);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(
            suggestions[0].names,
            vec!["node_modules".to_string(), "target".to_string()]
        );
    }

    #[test]
    fn group_by_parent_separates_distinct_parents() {
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
        let suggestions = group_by_parent(matches);
        assert_eq!(suggestions.len(), 2);
    }

    #[test]
    fn format_decisions_emits_a_header_and_plus_lines() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/home/user1/projects/big-frontend"),
            names: vec!["node_modules".to_string(), "target".to_string()],
        }];
        let text = format_decisions(&suggestions);
        assert!(text.contains("# /home/user1/projects/big-frontend/.ghostvolumes-decisions\n"));
        assert!(text.contains("+ node_modules\n"));
        assert!(text.contains("+ target\n"));
    }
}
