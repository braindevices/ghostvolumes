//! `ghostvolumes discover`: read-only survey that classifies every
//! *undecided* directory into [`MatchKind`] kinds and suggests a
//! `ghostvolumes decide` command, without registering a project or
//! writing a decision file. A match already covered by a `+`/`-`
//! between it and `start` is skipped so it isn't re-suggested.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::{btrfs, decision, filenames};

/// Why a directory was surfaced — see the module doc comment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    ApprovedCandidate,
    UnwatchedSubvolume,
    NotYetConverted,
    DeniedButExists,
    ApprovedNotConverted,
}

pub struct DiscoveredMatch {
    pub parent: PathBuf,
    pub name: String,
    pub kind: MatchKind,
}

fn read_decision_file(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// Classifies one candidate given what `decision::resolve` found (`None`
/// for genuinely undecided — no entry, or only a `?` pending marker)
/// against the actual filesystem state. `None` return means "consistent,
/// nothing to report" (approved-and-converted, or denied-and-plain).
fn classify(resolved: Option<bool>, is_watched: bool, is_subvolume: bool) -> Option<MatchKind> {
    match resolved {
        None if is_subvolume && is_watched => Some(MatchKind::ApprovedCandidate),
        None if is_subvolume => Some(MatchKind::UnwatchedSubvolume),
        None => Some(MatchKind::NotYetConverted),
        Some(false) if is_subvolume => Some(MatchKind::DeniedButExists),
        Some(true) if !is_subvolume => Some(MatchKind::ApprovedNotConverted),
        Some(_) => None,
    }
}

/// Stat-walks from `start`, skipping anything matching `ignore_patterns`
/// or an `ignore_paths` entry, and never descends into a matched
/// directory (a match is never itself re-scanned for nested matches).
/// `ignore_paths` is exact absolute directories to skip entirely (no
/// report, no descent); `ignore_patterns` is gitignore-style name
/// matching applied throughout the tree.
pub fn walk(
    start: &Path,
    max_depth: Option<u32>,
    watched_names: &[String],
    ignore_patterns: &[String],
    ignore_paths: &[PathBuf],
) -> Vec<DiscoveredMatch> {
    let mut matches = Vec::new();
    walk_inner(
        start,
        start,
        max_depth,
        watched_names,
        ignore_patterns,
        ignore_paths,
        0,
        &mut matches,
    );
    matches
}

#[allow(clippy::too_many_arguments)]
fn walk_inner(
    start: &Path,
    dir: &Path,
    max_depth: Option<u32>,
    watched_names: &[String],
    ignore_patterns: &[String],
    ignore_paths: &[PathBuf],
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
        let path = entry.path();
        if decision::ignore_matches(ignore_patterns, dir, &path) || ignore_paths.contains(&path) {
            continue;
        }

        let is_watched = watched_names.iter().any(|w| w == name_str.as_ref());
        let is_subvolume = btrfs::is_subvolume(&path).unwrap_or(false);

        if is_watched || is_subvolume {
            let resolved = decision::resolve(
                &path,
                start,
                filenames::DECISION_FILE_NAME,
                read_decision_file,
            );
            if let Some(kind) = classify(resolved, is_watched, is_subvolume) {
                matches.push(DiscoveredMatch {
                    parent: dir.to_path_buf(),
                    name: name_str.to_string(),
                    kind,
                });
            }
            continue; // never descend into any of these kinds
        }
        walk_inner(
            start,
            &path,
            max_depth,
            watched_names,
            ignore_patterns,
            ignore_paths,
            depth + 1,
            matches,
        );
    }
}

#[derive(Default)]
pub struct ProjectSuggestion {
    pub path: PathBuf,
    pub approved: Vec<String>,
    pub unwatched_subvolumes: Vec<String>,
    pub not_yet_converted: Vec<String>,
    pub denied_but_exists: Vec<String>,
    pub approved_not_converted: Vec<String>,
}

impl ProjectSuggestion {
    fn sort_and_dedup(&mut self) {
        for names in [
            &mut self.approved,
            &mut self.unwatched_subvolumes,
            &mut self.not_yet_converted,
            &mut self.denied_but_exists,
            &mut self.approved_not_converted,
        ] {
            names.sort();
            names.dedup();
        }
    }
}

/// Groups matches by parent directory — each group is one directory's
/// worth of findings, split by kind.
pub fn group_by_parent(matches: Vec<DiscoveredMatch>) -> Vec<ProjectSuggestion> {
    let mut by_parent: BTreeMap<PathBuf, ProjectSuggestion> = BTreeMap::new();
    for m in matches {
        let entry = by_parent
            .entry(m.parent.clone())
            .or_insert_with(|| ProjectSuggestion {
                path: m.parent.clone(),
                ..Default::default()
            });
        match m.kind {
            MatchKind::ApprovedCandidate => entry.approved.push(m.name),
            MatchKind::UnwatchedSubvolume => entry.unwatched_subvolumes.push(m.name),
            MatchKind::NotYetConverted => entry.not_yet_converted.push(m.name),
            MatchKind::DeniedButExists => entry.denied_but_exists.push(m.name),
            MatchKind::ApprovedNotConverted => entry.approved_not_converted.push(m.name),
        }
    }
    by_parent
        .into_values()
        .map(|mut s| {
            s.sort_and_dedup();
            s
        })
        .collect()
}

/// The shallowest *other* suggested path that's an ancestor of `path`,
/// if any — two suggested groups in the same lineage would violate "no
/// nested projects" if both were registered. See `merge_nested_suggestions`.
fn shallowest_ancestor_suggestion<'a>(
    path: &Path,
    all_paths: &'a [PathBuf],
) -> Option<&'a PathBuf> {
    all_paths
        .iter()
        .filter(|p| p.as_path() != path && path.starts_with(p))
        .min_by_key(|p| p.components().count())
}

/// Folds every suggestion nested inside another into that shallowest
/// ancestor, so the report proposes at most one project per lineage.
/// `start` can't absorb other suggestions unless `root_is_project` is
/// set; `no_project` applies that same exclusion to other paths.
pub fn merge_nested_suggestions(
    suggestions: Vec<ProjectSuggestion>,
    start: &Path,
    root_is_project: bool,
    no_project: &[PathBuf],
) -> Vec<ProjectSuggestion> {
    let merge_candidates: Vec<PathBuf> = suggestions
        .iter()
        .map(|s| s.path.clone())
        .filter(|p| (root_is_project || p != start) && !no_project.contains(p))
        .collect();
    let merge_targets: BTreeMap<PathBuf, PathBuf> = suggestions
        .iter()
        .filter_map(|s| {
            shallowest_ancestor_suggestion(&s.path, &merge_candidates)
                .map(|a| (s.path.clone(), a.clone()))
        })
        .collect();

    let mut by_path: BTreeMap<PathBuf, ProjectSuggestion> = suggestions
        .into_iter()
        .map(|s| (s.path.clone(), s))
        .collect();

    for (child_path, ancestor_path) in &merge_targets {
        let Some(child) = by_path.remove(child_path) else {
            continue;
        };
        if let Some(ancestor) = by_path.get_mut(ancestor_path) {
            fold_nested_child(ancestor, ancestor_path, &child);
        }
    }

    by_path
        .into_values()
        .map(|mut s| {
            s.sort_and_dedup();
            s
        })
        .collect()
}

fn fold_nested_child(
    ancestor: &mut ProjectSuggestion,
    ancestor_path: &Path,
    child: &ProjectSuggestion,
) {
    let anchor = |name: &str| -> String {
        decision::anchored_pattern(ancestor_path, &child.path.join(name))
            .unwrap_or_else(|| name.to_string())
    };
    ancestor
        .approved
        .extend(child.approved.iter().map(|n| anchor(n)));
    ancestor
        .unwatched_subvolumes
        .extend(child.unwatched_subvolumes.iter().map(|n| anchor(n)));
    ancestor
        .not_yet_converted
        .extend(child.not_yet_converted.iter().map(|n| anchor(n)));
    ancestor
        .denied_but_exists
        .extend(child.denied_but_exists.iter().map(|n| anchor(n)));
    ancestor
        .approved_not_converted
        .extend(child.approved_not_converted.iter().map(|n| anchor(n)));
}

/// Renders suggestions as human-facing advice — a `ghostvolumes decide`
/// command per actionable group, plus a report-only line for watched
/// names not yet converted. Call `merge_nested_suggestions` first if
/// `suggestions` might contain nested paths; this only renders what
/// it's given.
pub fn format_report(suggestions: &[ProjectSuggestion]) -> String {
    let mut out = String::new();
    for s in suggestions {
        out.push_str(&format!("{}\n", s.path.display()));
        if !s.approved.is_empty() {
            out.push_str("  already a subvolume, needs a decision:\n");
            let flags: String = s
                .approved
                .iter()
                .map(|name| format!(" --add {name}"))
                .collect();
            out.push_str(&format!(
                "    ghostvolumes decide {}{flags}\n",
                s.path.display()
            ));
        }
        if !s.unwatched_subvolumes.is_empty() {
            out.push_str("  already a subvolume, but not a watched name - needs clarification:\n");
            for name in &s.unwatched_subvolumes {
                out.push_str(&format!(
                    "    ghostvolumes decide {} --add {name}   # or --deny {name}\n",
                    s.path.display()
                ));
            }
        }
        if !s.not_yet_converted.is_empty() {
            out.push_str(&format!(
                "  watched names present but not yet converted (informational only): {}\n",
                s.not_yet_converted.join(", ")
            ));
        }
        if !s.denied_but_exists.is_empty() {
            out.push_str(&format!(
                "  DRIFT: recorded as denied ('-') but already a subvolume - decision and filesystem disagree: {}\n",
                s.denied_but_exists.join(", ")
            ));
            let flags: String = s
                .denied_but_exists
                .iter()
                .map(|name| format!(" --add {name}"))
                .collect();
            out.push_str(&format!(
                "    to override the recorded '-': ghostvolumes decide {}{flags}\n",
                s.path.display()
            ));
        }
        if !s.approved_not_converted.is_empty() {
            out.push_str(&format!(
                "  approved ('+') but not yet converted - run to materialize: ghostvolumes convert {}   # {}\n",
                s.path.display(),
                s.approved_not_converted.join(", ")
            ));
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

    fn kinds_and_names(matches: &[DiscoveredMatch]) -> Vec<(MatchKind, &str)> {
        matches.iter().map(|m| (m.kind, m.name.as_str())).collect()
    }

    #[test]
    fn finds_an_undecided_subvolume_matching_a_watched_name() {
        let dir = btrfs_scratch_dir();
        btrfs::create_subvolume(dir.path(), "node_modules").unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].parent, dir.path());
        assert_eq!(matches[0].name, "node_modules");
        assert_eq!(matches[0].kind, MatchKind::ApprovedCandidate);
    }

    #[test]
    fn an_already_decided_watched_subvolume_is_not_reported_at_all() {
        let dir = btrfs_scratch_dir();
        btrfs::create_subvolume(dir.path(), "node_modules").unwrap();
        std::fs::write(
            dir.path().join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();

        assert!(walk(dir.path(), None, &watched(), &[], &[]).is_empty());
    }

    #[test]
    fn a_denied_watched_subvolume_that_is_consistent_is_not_reported() {
        // "-" recorded and it's still a plain directory (the expected
        // state for a real decline) - nothing to flag.
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        std::fs::write(
            dir.path().join(filenames::DECISION_FILE_NAME),
            "- node_modules\n",
        )
        .unwrap();

        assert!(walk(dir.path(), None, &watched(), &[], &[]).is_empty());
    }

    #[test]
    fn a_denied_name_that_is_already_a_subvolume_is_flagged_as_drift() {
        // "-" recorded, but it's a real subvolume anyway - the decision
        // and the filesystem disagree.
        let dir = btrfs_scratch_dir();
        btrfs::create_subvolume(dir.path(), "node_modules").unwrap();
        std::fs::write(
            dir.path().join(filenames::DECISION_FILE_NAME),
            "- node_modules\n",
        )
        .unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
        assert_eq!(
            kinds_and_names(&matches),
            vec![(MatchKind::DeniedButExists, "node_modules")]
        );
    }

    #[test]
    fn an_approved_name_that_is_still_plain_is_flagged_as_not_yet_converted_drift() {
        // "+" recorded, but it's still a plain directory - approved,
        // just never materialized.
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        std::fs::write(
            dir.path().join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
        assert_eq!(
            kinds_and_names(&matches),
            vec![(MatchKind::ApprovedNotConverted, "node_modules")]
        );
    }

    #[test]
    fn a_decision_recorded_above_start_still_suppresses_the_match() {
        // `start` is the boundary, so a decision file there still
        // governs a match found in a subdirectory.
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        btrfs::create_subvolume(&dir.path().join("nested"), "node_modules").unwrap();
        std::fs::write(
            dir.path().join(filenames::DECISION_FILE_NAME),
            "+ node_modules\n",
        )
        .unwrap();

        assert!(walk(dir.path(), None, &watched(), &[], &[]).is_empty());
    }

    #[test]
    fn an_unwatched_but_already_subvolume_directory_is_flagged_for_clarification() {
        let dir = btrfs_scratch_dir();
        btrfs::create_subvolume(dir.path(), "totally-unwatched-name").unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
        assert_eq!(
            kinds_and_names(&matches),
            vec![(MatchKind::UnwatchedSubvolume, "totally-unwatched-name")]
        );
    }

    #[test]
    fn a_watched_name_that_is_still_a_plain_directory_is_reported_as_not_yet_converted() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
        assert_eq!(
            kinds_and_names(&matches),
            vec![(MatchKind::NotYetConverted, "node_modules")]
        );
    }

    #[test]
    fn an_unwatched_plain_directory_is_not_reported_at_all() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("totally-unwatched-plain-dir")).unwrap();

        assert!(walk(dir.path(), None, &watched(), &[], &[]).is_empty());
    }

    #[test]
    fn does_not_descend_into_a_matched_subvolume() {
        let dir = btrfs_scratch_dir();
        btrfs::create_subvolume(dir.path(), "node_modules").unwrap();
        // If discover recursed into it, it would find this nested
        // "target" subvolume too - it must not.
        btrfs::create_subvolume(&dir.path().join("node_modules"), "target").unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "node_modules");
    }

    #[test]
    fn does_not_descend_into_a_not_yet_converted_watched_directory_either() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("node_modules")).unwrap();
        btrfs::create_subvolume(&dir.path().join("node_modules"), "target").unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
        assert_eq!(
            kinds_and_names(&matches),
            vec![(MatchKind::NotYetConverted, "node_modules")]
        );
    }

    #[test]
    fn a_configured_ignore_pattern_is_never_descended_into() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join(".git/node_modules")).unwrap();
        // Even if it were a subvolume, .git itself must never be
        // descended into - once it's actually configured as an ignore
        // pattern (unlike the bare name below, which isn't).
        assert!(walk(dir.path(), None, &watched(), &[".git".to_string()], &[]).is_empty());
    }

    #[test]
    fn without_a_matching_ignore_pattern_a_dot_directory_is_walked_into_like_any_other() {
        // `.git` has no special status in the walk; it's skipped only
        // because `default-ignore` names it, like any other pattern.
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        btrfs::create_subvolume(&dir.path().join(".git"), "node_modules").unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].parent, dir.path().join(".git"));
    }

    #[test]
    fn a_custom_ignore_pattern_is_never_descended_into() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join(".hg/target")).unwrap();

        assert!(walk(dir.path(), None, &watched(), &[".hg".to_string()], &[]).is_empty());
    }

    #[test]
    fn an_ignore_pattern_skips_even_a_watched_name_match() {
        // Ignoring is checked before the watched-name check at all - a
        // `.ghostvolumes-ignore`'d directory is skipped even if it would
        // otherwise match a watched name.
        let dir = btrfs_scratch_dir();
        btrfs::create_subvolume(dir.path(), "node_modules").unwrap();

        assert!(
            walk(
                dir.path(),
                None,
                &watched(),
                &["node_modules".to_string()],
                &[]
            )
            .is_empty()
        );
    }

    #[test]
    fn an_ignore_path_is_never_scanned_at_all() {
        // Distinct from a name pattern: this is an exact absolute path
        // (e.g. a known-noisy cache directory) rather than a pattern
        // applied anywhere in the tree.
        let dir = btrfs_scratch_dir();
        let noisy = dir.path().join("noisy-container");
        std::fs::create_dir_all(noisy.join("nested")).unwrap();
        btrfs::create_subvolume(&noisy.join("nested"), "node_modules").unwrap();

        assert!(walk(dir.path(), None, &watched(), &[], &[noisy]).is_empty());
    }

    #[test]
    fn an_ignore_path_does_not_affect_an_unrelated_sibling() {
        let dir = btrfs_scratch_dir();
        let noisy = dir.path().join("noisy-container");
        std::fs::create_dir_all(&noisy).unwrap();
        btrfs::create_subvolume(dir.path(), "node_modules").unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[noisy]);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].name, "node_modules");
    }

    #[test]
    fn finds_nested_matches_in_unrelated_subdirectories() {
        let dir = btrfs_scratch_dir();
        std::fs::create_dir_all(dir.path().join("projects/app")).unwrap();
        btrfs::create_subvolume(&dir.path().join("projects/app"), "target").unwrap();

        let matches = walk(dir.path(), None, &watched(), &[], &[]);
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
        assert!(walk(dir.path(), Some(2), &watched(), &[], &[]).is_empty());
        assert_eq!(walk(dir.path(), Some(3), &watched(), &[], &[]).len(), 1);
    }

    #[test]
    fn group_by_parent_groups_and_dedupes_within_each_kind() {
        let matches = vec![
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "node_modules".to_string(),
                kind: MatchKind::ApprovedCandidate,
            },
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "target".to_string(),
                kind: MatchKind::ApprovedCandidate,
            },
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "target".to_string(),
                kind: MatchKind::ApprovedCandidate,
            },
        ];
        let suggestions = group_by_parent(matches);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(
            suggestions[0].approved,
            vec!["node_modules".to_string(), "target".to_string()]
        );
    }

    #[test]
    fn group_by_parent_separates_distinct_parents() {
        let matches = vec![
            DiscoveredMatch {
                parent: PathBuf::from("/a"),
                name: "target".to_string(),
                kind: MatchKind::ApprovedCandidate,
            },
            DiscoveredMatch {
                parent: PathBuf::from("/b"),
                name: "node_modules".to_string(),
                kind: MatchKind::ApprovedCandidate,
            },
        ];
        let suggestions = group_by_parent(matches);
        assert_eq!(suggestions.len(), 2);
    }

    #[test]
    fn group_by_parent_separates_kinds_within_the_same_directory() {
        let matches = vec![
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "node_modules".to_string(),
                kind: MatchKind::ApprovedCandidate,
            },
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "weird-name".to_string(),
                kind: MatchKind::UnwatchedSubvolume,
            },
            DiscoveredMatch {
                parent: PathBuf::from("/p"),
                name: "target".to_string(),
                kind: MatchKind::NotYetConverted,
            },
        ];
        let suggestions = group_by_parent(matches);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].approved, vec!["node_modules".to_string()]);
        assert_eq!(
            suggestions[0].unwatched_subvolumes,
            vec!["weird-name".to_string()]
        );
        assert_eq!(suggestions[0].not_yet_converted, vec!["target".to_string()]);
    }

    #[test]
    fn format_report_suggests_a_decide_command_for_an_approved_candidate() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/home/user1/projects/big-frontend"),
            approved: vec!["node_modules".to_string(), "target".to_string()],
            ..Default::default()
        }];
        let text = format_report(&suggestions);
        assert!(text.contains("/home/user1/projects/big-frontend\n"));
        assert!(text.contains(
            "ghostvolumes decide /home/user1/projects/big-frontend --add node_modules --add target\n"
        ));
    }

    #[test]
    fn format_report_offers_both_add_and_deny_for_an_unwatched_subvolume() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/p"),
            unwatched_subvolumes: vec!["weird-name".to_string()],
            ..Default::default()
        }];
        let text = format_report(&suggestions);
        assert!(text.contains("needs clarification"));
        assert!(text.contains("ghostvolumes decide /p --add weird-name"));
        assert!(text.contains("--deny weird-name"));
    }

    #[test]
    fn format_report_never_suggests_a_command_for_not_yet_converted_names() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/p"),
            not_yet_converted: vec!["node_modules".to_string()],
            ..Default::default()
        }];
        let text = format_report(&suggestions);
        assert!(text.contains("informational only"));
        assert!(text.contains("node_modules"));
        assert!(!text.contains("ghostvolumes decide"));
    }

    #[test]
    fn format_report_flags_denied_but_existing_drift_with_an_override_command() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/p"),
            denied_but_exists: vec!["node_modules".to_string()],
            ..Default::default()
        }];
        let text = format_report(&suggestions);
        assert!(text.contains("DRIFT"));
        assert!(text.contains("recorded as denied"));
        assert!(text.contains("ghostvolumes decide /p --add node_modules"));
    }

    #[test]
    fn format_report_suggests_converting_an_approved_but_unmaterialized_name() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/p"),
            approved_not_converted: vec!["node_modules".to_string()],
            ..Default::default()
        }];
        let text = format_report(&suggestions);
        assert!(text.contains("approved"));
        assert!(text.contains("not yet converted"));
        assert!(text.contains("ghostvolumes convert /p"));
        assert!(text.contains("node_modules"));
    }

    #[test]
    fn merge_nested_suggestions_folds_every_descendant_into_the_shallowest_ancestor() {
        let suggestions = vec![
            ProjectSuggestion {
                path: PathBuf::from("/a/b"),
                approved: vec!["build".to_string()],
                ..Default::default()
            },
            ProjectSuggestion {
                path: PathBuf::from("/a/b/c"),
                approved: vec!["build".to_string()],
                ..Default::default()
            },
            ProjectSuggestion {
                path: PathBuf::from("/a/b/c/d"),
                approved: vec!["build".to_string()],
                ..Default::default()
            },
        ];
        let merged = merge_nested_suggestions(suggestions, Path::new("/a"), false, &[]);
        // Only one project left - both descendants folded into /a/b,
        // not left as separate conflicting suggestions.
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].path, PathBuf::from("/a/b"));
        // Folded-in descendants become anchored patterns relative to
        // /a/b, pointing at their exact original location.
        assert_eq!(
            merged[0].approved,
            vec![
                "/c/build".to_string(),
                "/c/d/build".to_string(),
                "build".to_string(),
            ]
        );
    }

    #[test]
    fn merge_nested_suggestions_leaves_unrelated_siblings_alone() {
        let suggestions = vec![
            ProjectSuggestion {
                path: PathBuf::from("/a"),
                approved: vec!["build".to_string()],
                ..Default::default()
            },
            ProjectSuggestion {
                path: PathBuf::from("/b"),
                approved: vec!["build".to_string()],
                ..Default::default()
            },
        ];
        let merged = merge_nested_suggestions(suggestions, Path::new("/"), false, &[]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn merge_nested_suggestions_does_not_fold_into_the_discover_start_path_by_default() {
        // /root must not swallow an unrelated, deeper project just for
        // being the shallowest path.
        let suggestions = vec![
            ProjectSuggestion {
                path: PathBuf::from("/root"),
                unwatched_subvolumes: vec!["leftover-tmp".to_string()],
                ..Default::default()
            },
            ProjectSuggestion {
                path: PathBuf::from("/root/projects/foo"),
                approved: vec!["build".to_string()],
                ..Default::default()
            },
        ];
        let merged = merge_nested_suggestions(suggestions, Path::new("/root"), false, &[]);
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn root_is_project_lets_the_start_path_absorb_nested_suggestions_after_all() {
        let suggestions = vec![
            ProjectSuggestion {
                path: PathBuf::from("/root/projects/foo"),
                unwatched_subvolumes: vec!["leftover-tmp".to_string()],
                ..Default::default()
            },
            ProjectSuggestion {
                path: PathBuf::from("/root/projects/foo/nested"),
                approved: vec!["build".to_string()],
                ..Default::default()
            },
        ];
        let merged =
            merge_nested_suggestions(suggestions, Path::new("/root/projects/foo"), true, &[]);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].path, PathBuf::from("/root/projects/foo"));
    }

    #[test]
    fn no_project_excludes_a_known_container_found_below_start_from_absorbing_anything() {
        // /root/workspace stands in for a known container - a workspace
        // folder holding many unrelated repos - found *below* start,
        // not start itself, so --root-is-project wouldn't help here.
        let suggestions = vec![
            ProjectSuggestion {
                path: PathBuf::from("/root/workspace"),
                unwatched_subvolumes: vec!["leftover-tmp".to_string()],
                ..Default::default()
            },
            ProjectSuggestion {
                path: PathBuf::from("/root/workspace/some-repo"),
                approved: vec!["build".to_string()],
                ..Default::default()
            },
        ];
        let merged = merge_nested_suggestions(
            suggestions,
            Path::new("/root"),
            false,
            &[PathBuf::from("/root/workspace")],
        );
        assert_eq!(merged.len(), 2);
    }

    #[test]
    fn format_report_prints_a_single_command_with_all_the_folded_in_patterns() {
        let suggestions = vec![ProjectSuggestion {
            path: PathBuf::from("/a/b"),
            approved: vec!["build".to_string(), "/c/build".to_string()],
            ..Default::default()
        }];
        let text = format_report(&suggestions);
        assert!(text.contains("ghostvolumes decide /a/b --add build --add /c/build\n"));
    }
}
