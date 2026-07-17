//! `ghostvolumes discover` (ai-work/tasks/decision-model.plan.md §7,
//! redesigned per `ai-work/tasks/discover-redesign.plan.md`): a
//! read-only survey of an arbitrary starting path (no project
//! registration needed, unlike `convert`/`decide`) that classifies
//! every *undecided* directory it finds into three kinds and suggests
//! the exact `ghostvolumes decide` command to run for it — it never
//! registers a project or writes a decision file itself.
//!
//! "Undecided" means genuinely undecided: `decision::resolve` is
//! checked against `start` (the discover invocation's own argument) as
//! the walk-up boundary, the same logic `convert`/`decide` use against
//! their own registered boundary. Anything already covered by a `+`/
//! `-` anywhere between a match and `start` is skipped entirely — this
//! is what keeps the output meaningful instead of permanently re-
//! suggesting things that are already fully decided.
//!
//! Three kinds for a genuinely undecided match (no `+`/`-` on record at
//! all, in descending order of confidence), plus two drift kinds for a
//! match where the filesystem disagrees with an *existing* decision:
//! - [`MatchKind::ApprovedCandidate`]: a watched name that's already a
//!   real subvolume — suggests `--add`.
//! - [`MatchKind::UnwatchedSubvolume`]: already a real subvolume, but
//!   an unwatched name — same underlying signal
//!   (`convert`/`decide`'s own walk treats this identically, defaulting
//!   to yes when it can ask interactively), but discover has no way to
//!   ask, so it presents both `--add` and `--deny` rather than picking
//!   one.
//! - [`MatchKind::NotYetConverted`]: a watched name that's still a
//!   plain directory — report-only, no command suggested. `-` would
//!   misrepresent "nobody has decided" as "a human declined", the one
//!   thing `-` means everywhere else in this project, so this is never
//!   written as real decision syntax, only reported.
//! - [`MatchKind::DeniedButExists`]: recorded `-`, but it's already a
//!   subvolume anyway — a human declined this, yet it exists on disk
//!   (made by hand afterward, most likely). Flagged as drift rather
//!   than silently trusted either way.
//! - [`MatchKind::ApprovedNotConverted`]: recorded `+`, but still a
//!   plain directory — approved, just never materialized. Not really a
//!   conflict, just informational: run `convert` to catch it up.
//!
//! Only on-disk mismatches are covered — a `+`/`?` decision recorded
//! for a path that doesn't exist on disk at all isn't detected here,
//! since the walk only ever visits directories that actually exist.
//!
//! `ignore_patterns` (Phase 2, `ai-work/tasks/convert-project-model.plan.md`)
//! replaces what used to be a hardcoded `.git`-only skip — matched via
//! `decision::ignore_matches`, anchored to each directory's own
//! immediate parent as the walk descends (this only matters for an
//! anchored pattern; a bare name like `.git` matches by leaf name alone
//! regardless of anchor). Deliberately global-only here: `discover`
//! isn't tied to any one registered project the way `convert` is (it
//! walks an arbitrary starting path, `~` by default), so only the
//! `default-ignore` tier applies — no volume-root/project-root
//! `.ghostvolumes-ignore` file lookup, unlike `convert`'s walk.

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

/// Stat-walks from `start` (skipping anything matching
/// `ignore_patterns` or exactly matching an `ignore_paths` entry,
/// never descending into any of the three kinds above whether or not
/// it turns out to be a subvolume — walking into a multi-gigabyte
/// `node_modules` tree looking for more matches would be pointless and
/// slow).
///
/// `ignore_paths` is a plain list of absolute directories to never
/// scan at all — no report, no descent — distinct from
/// `ignore_patterns`'s gitignore-style name matching: a user-specified
/// exact path (e.g. a known-noisy cache directory) rather than a name
/// pattern applied everywhere in the tree.
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
/// if any — two suggested groups in the same lineage would, if both
/// were actually registered as projects, violate "no nested projects"
/// (`ai-work/tasks/nested-project-boundaries.plan.md`). `decide`'s own
/// `ensure_project_registered` already detects and warns about exactly
/// this the moment you try to register both, but discover can go
/// further and just not suggest two conflicting projects to begin
/// with — see `merge_nested_suggestions`.
fn shallowest_ancestor_suggestion<'a>(
    path: &Path,
    all_paths: &'a [PathBuf],
) -> Option<&'a PathBuf> {
    all_paths
        .iter()
        .filter(|p| p.as_path() != path && path.starts_with(p))
        .min_by_key(|p| p.components().count())
}

/// Folds every suggestion that's nested inside another suggestion into
/// that shallowest ancestor, so the report only ever proposes
/// registering one project per lineage — matching "no nested
/// projects" instead of merely warning about it. A folded-in name is
/// re-expressed as `decision::anchored_pattern` relative to the
/// ancestor (e.g. `/bb/cc/build`) so the single resulting `decide`
/// command still targets the exact original path.
///
/// `start` (the discover invocation's own argument) is excluded from
/// the pool of paths eligible to *absorb* another suggestion unless
/// `root_is_project` says otherwise — `start` is usually a broad,
/// arbitrary directory being surveyed (`$HOME`, a workspace folder),
/// not itself a project; without this, a `start` that happens to have
/// its own unrelated finding (e.g. a stray subvolume directly inside
/// it) would swallow every other suggestion found anywhere below it,
/// however unrelated, purely by virtue of being the shallowest path in
/// the whole report. `start`'s own findings are still reported as
/// their own group either way — this only controls whether other
/// suggestions can fold *into* it.
///
/// `no_project` is the same exclusion, but user-declared and always
/// applied regardless of `root_is_project` — for a known-not-a-project
/// container found *below* `start` (e.g. a workspace folder holding
/// many unrelated repos), not just `start` itself.
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

/// Renders suggestions as human-facing advice — a `ghostvolumes
/// decide` command per actionable group, plus a report-only line for
/// watched names that exist but aren't converted yet. Never anything
/// resembling raw decision-file syntax to paste in: `decide` is the
/// only thing that ever writes a decision, so it's the only thing this
/// ever tells you to run. Call `merge_nested_suggestions` first if
/// `suggestions` might contain nested paths — this only renders what
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
        // Same "closest enclosing file wins, walked up to the
        // boundary" semantics as convert/decide - here `start` is the
        // boundary, so a decision file at `start` itself still governs
        // a match found in a subdirectory.
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
        // The generalization this guards: `.git` has no special status
        // in the walk itself anymore - it's only skipped because
        // `default-ignore` names it, same as any other configured
        // pattern.
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
        // Its own direct match keeps its bare name; the two folded-in
        // descendants become anchored patterns relative to /a/b, each
        // pointing at its own exact original location - not just its
        // own immediate parent's name, since /a/b/c/d's "build" would
        // otherwise be indistinguishable from /a/b/c's.
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
        // /root here stands in for a broad, arbitrary directory being
        // surveyed ($HOME, a workspace folder) that happens to have its
        // own unrelated finding - it must not swallow an unrelated,
        // much deeper project just for being the shallowest path.
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
