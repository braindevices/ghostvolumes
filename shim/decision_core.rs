// Decision-file parsing and matching (replaces the git-tracked gate,
// ai-work/tasks/decision-model.plan.md §1-§3). Dependency-free (plain
// `std` only), shared between the main CLI (via `include!`, from
// `src/decision.rs`) and the LD_PRELOAD shim (via `mod`, from
// `shim/preload.rs`).
//
// A decision file is gitignore-style, one per directory: each line is
// `+ <pattern>` (convert), `- <pattern>` (never convert), `? <pattern>`
// (still undecided - a machine-written marker, toggled in place into a
// `+`/`-` line once a real decision is recorded for the same pattern),
// a `#` comment (human-only, never written or touched by this tool),
// or blank. Deliberately not a full gitignore clone - just three
// pattern forms, resolved relative to the file's own directory:
//   /name           anchored: that exact single location only
//   name            unanchored: matches at any depth under this dir
//   /a/b/**/name    anchored prefix, arbitrary depth after it
//
// Plain `//` comments, not `//!`/`///`: this file gets spliced
// mid-file into src/decision.rs via `include!`, where an inner doc
// comment is only legal at the very start of a file/module.

// Fully-qualified paths throughout (rather than `use` at file scope):
// this file is included both mid-file into src/decision.rs (which
// already has its own `use`s in scope) and as its own `mod
// decision_core` inside the shim (a separate module scope) - qualifying
// every path keeps both contexts unambiguous without duplicate-import
// errors.

/// One parsed, non-comment, non-blank line: `+`/`-` polarity and the
/// raw pattern text (not yet matched against anything).
struct DecisionLine {
    convert: bool,
    pattern: String,
}

/// Parses one decision file's raw text into its meaningful lines, in
/// file order (so callers can apply "last matching line wins").
/// Ignores blank lines, `#` comments, and anything else that isn't
/// exactly `+`/`-`-prefixed - including `?` pending-marker lines the
/// shim (or `convert`, run non-interactively) appends (§4), which are
/// never meant to be matched against, only toggled into real `+`/`-`
/// lines later. No dedicated `?` handling needed here at all: it's
/// already inert via the same catch-all as any other non-decision line.
fn parse_lines(text: &str) -> alloc_free_vec::Vec<DecisionLine> {
    let mut lines = alloc_free_vec::Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let (marker, rest) = trimmed.split_at(1);
        let pattern = rest.trim_start();
        if pattern.is_empty() {
            continue;
        }
        let convert = match marker {
            "+" => true,
            "-" => false,
            _ => continue, // malformed line - ignore, don't error
        };
        lines.push(DecisionLine {
            convert,
            pattern: pattern.to_string(),
        });
    }
    lines
}

// `alloc_free_vec` is just `std::vec` - named module purely so the
// doc comment above reads naturally; there is no actual no-alloc
// constraint here (the shim already uses String/Vec elsewhere, e.g.
// cache_core.rs), this is plain std::vec::Vec.
mod alloc_free_vec {
    pub use std::vec::Vec;
}

/// Splits `pattern` (already stripped of its leading `/`) into the
/// path components before and after a single `**` segment, if present.
/// Only one `**` is supported (deliberately not a full gitignore
/// clone); a pattern with more than one is treated as having none
/// (falls back to exact-match semantics for the whole thing).
fn split_double_star(
    pattern: &str,
) -> Option<(alloc_free_vec::Vec<&str>, alloc_free_vec::Vec<&str>)> {
    let components: alloc_free_vec::Vec<&str> =
        pattern.split('/').filter(|c| !c.is_empty()).collect();
    let star_positions: alloc_free_vec::Vec<usize> = components
        .iter()
        .enumerate()
        .filter(|(_, c)| **c == "**")
        .map(|(i, _)| i)
        .collect();
    if star_positions.len() != 1 {
        return None;
    }
    let at = star_positions[0];
    let prefix = components[..at].to_vec();
    let suffix = components[at + 1..].to_vec();
    Some((prefix, suffix))
}

/// The candidate's path components relative to `file_dir`, or `None`
/// if `candidate` isn't under `file_dir` at all.
fn relative_components(
    file_dir: &std::path::Path,
    candidate: &std::path::Path,
) -> Option<alloc_free_vec::Vec<String>> {
    let relative = candidate.strip_prefix(file_dir).ok()?;
    Some(
        relative
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect(),
    )
}

/// Does `pattern` (written in a decision file whose own directory is
/// `file_dir`) match `candidate` (an absolute, already-resolved path)?
fn pattern_matches(file_dir: &std::path::Path, pattern: &str, candidate: &std::path::Path) -> bool {
    let Some(rel) = relative_components(file_dir, candidate) else {
        return false;
    };
    if let Some(anchored) = pattern.strip_prefix('/') {
        if let Some((prefix, suffix)) = split_double_star(anchored) {
            if rel.len() < prefix.len() + suffix.len() {
                return false;
            }
            let prefix_matches = rel
                .iter()
                .take(prefix.len())
                .map(String::as_str)
                .eq(prefix.iter().copied());
            let suffix_matches = rel[rel.len() - suffix.len()..]
                .iter()
                .map(String::as_str)
                .eq(suffix.iter().copied());
            prefix_matches && suffix_matches
        } else {
            let anchored_components: alloc_free_vec::Vec<&str> =
                anchored.split('/').filter(|c| !c.is_empty()).collect();
            rel.len() == anchored_components.len()
                && rel
                    .iter()
                    .map(String::as_str)
                    .eq(anchored_components.iter().copied())
        }
    } else {
        // Bare, unanchored name: matches at any depth under file_dir,
        // by leaf (final component) name only.
        rel.last().is_some_and(|leaf| leaf == pattern)
    }
}

/// Parses a `.ghostvolumes-ignore` file's raw text into a flat list of
/// patterns to never walk into at all — same three pattern forms a
/// decision file uses (`name`, `/name`, `/a/b/**/name`), but no
/// `+`/`-`/`?` prefix: every non-blank, non-`#`-comment line is just a
/// pattern, since there's nothing to decide here, only whether to
/// descend.
///
/// Dead code from the shim's own perspective (like `names_for`/
/// `longest_matching_prefix` in `cache_core.rs`) — the shim intercepts
/// `mkdir`/`mkdirat`, it never walks a directory tree, so only
/// `convert`/`discover` (CLI-side) ever call this.
#[allow(dead_code)]
pub fn parse_ignore_patterns(text: &str) -> std::vec::Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// `true` if `candidate` matches any of `patterns`, resolved relative
/// to `anchor_dir` — reuses `pattern_matches`'s exact grammar for a
/// different purpose than a decision file: skip descending into
/// `candidate` entirely (never even check it for a watched-name match),
/// not decide whether to convert it.
///
/// Dead code from the shim's own perspective — see
/// `parse_ignore_patterns` above.
#[allow(dead_code)]
pub fn ignore_matches(
    patterns: &[String],
    anchor_dir: &std::path::Path,
    candidate: &std::path::Path,
) -> bool {
    patterns
        .iter()
        .any(|pattern| pattern_matches(anchor_dir, pattern, candidate))
}

/// Resolves `candidate`'s decision from one decision file's raw text:
/// the *last* matching line wins (lets a user add a narrow override
/// after a broad rule). `None` if nothing in this file matches.
///
/// Called by `resolve` below, which both `decide()` (shim) and
/// `convert` (CLI) use.
pub fn resolve_in_file(
    file_dir: &std::path::Path,
    text: &str,
    candidate: &std::path::Path,
) -> Option<bool> {
    parse_lines(text)
        .into_iter()
        .rev()
        .find(|line| pattern_matches(file_dir, &line.pattern, candidate))
        .map(|line| line.convert)
}

/// Walks up from `candidate`'s parent directory to (and including)
/// `boundary`, checking each level for a decision file named
/// `file_name`, and resolving against the *closest* (first found in
/// the walk) one that has any matching pattern at all - a decision
/// file existing at some level without a matching line for this
/// specific candidate does NOT count as a resolution; the walk keeps
/// going past it. Returns `None` if nothing resolves anywhere up to
/// and including `boundary`.
///
/// `read_file` is injectable (rather than a hardcoded `std::fs::read_to_string`)
/// so this is unit-testable without real files on disk; the real
/// callers (shim and CLI) both pass a thin wrapper around
/// `std::fs::read_to_string`.
///
/// Wired into `decide()` (`shim/preload.rs`) as the git-tracked gate's
/// replacement, and into `convert` (`src/convert.rs`, CLI-side).
pub fn resolve(
    candidate: &std::path::Path,
    boundary: &std::path::Path,
    file_name: &str,
    read_file: impl Fn(&std::path::Path) -> Option<String>,
) -> Option<bool> {
    let start = candidate.parent()?;
    if !start.starts_with(boundary) {
        return None;
    }
    for ancestor in start.ancestors() {
        let candidate_file = ancestor.join(file_name);
        let decision =
            read_file(&candidate_file).and_then(|text| resolve_in_file(ancestor, &text, candidate));
        if let Some(decision) = decision {
            return Some(decision);
        }
        if ancestor == boundary {
            break;
        }
    }
    None
}

/// The anchored pattern text for `candidate`, relative to `boundary`
/// (the project-root decision file's own directory - ai-work/tasks/decision-model.plan.md
/// §1's "auto-added decisions always append to the top-level file
/// only"), e.g. `/packages/foo/node_modules`. `None` if `candidate`
/// isn't under `boundary` at all - shouldn't happen given how
/// `boundary` is computed by callers, but degrades safely rather than
/// panicking.
pub fn anchored_pattern(boundary: &std::path::Path, candidate: &std::path::Path) -> Option<String> {
    let rel = candidate.strip_prefix(boundary).ok()?;
    Some(format!("/{}", rel.to_string_lossy()))
}

/// The exact pending-marker line the shim (or `convert`, run
/// non-interactively) appends for a still-undecided candidate (§4).
/// `?`, not `#` — a `#` line is a pure, untouched human comment
/// forever; `?` means "the tool noted this pattern as
/// seen-but-undecided," a machine-owned annotation that a later real
/// decision for the *same* pattern can toggle in place
/// (`toggle_or_replace_pending`) rather than leaving as stale clutter
/// alongside the decision that supersedes it. `parse_lines` already
/// ignores any line that isn't exactly `+`/`-`-prefixed, so `?` needs
/// no changes anywhere in `resolve`/`resolve_in_file` — it's already
/// inert for decision-resolution purposes, same as a `#` comment.
#[allow(dead_code)]
pub fn pending_marker_line(pattern: &str) -> String {
    format!("? {pattern}")
}

/// `true` iff `text` (the decision file's current content) doesn't
/// already contain this exact pending-marker line - best-effort dedup
/// (§4): not airtight under concurrent appends from independent shim
/// processes (a check-then-write race), but harmless if it isn't, since
/// a duplicate marker is just an extra line to ignore or delete.
#[allow(dead_code)]
pub fn needs_pending_marker(text: &str, pattern: &str) -> bool {
    let line = pending_marker_line(pattern);
    !text.lines().any(|existing| existing.trim() == line)
}

/// Replaces an existing `? <pattern>` pending-marker line in place with
/// `decision_line` (e.g. `+ <pattern>` or `- <pattern>`), preserving
/// every other line's content and order unchanged. Only `pattern`
/// itself needs to match the marker being searched for -
/// `decision_line` can carry a completely different pattern (e.g. "a"/
/// every-match-of-this-name recording a broader pattern than the
/// anchored one its own pending marker used) and still lands in the
/// same spot the marker occupied, since this is a plain line-content
/// swap, not a match between the marker's pattern and the decision's
/// own. Appends `decision_line` at the end instead if no matching
/// pending marker is found - not every decision follows a prior
/// pending note, so there's nothing to toggle in that case.
#[allow(dead_code)]
pub fn toggle_or_replace_pending(text: &str, pattern: &str, decision_line: &str) -> String {
    let marker = pending_marker_line(pattern);
    let mut replaced = false;
    let mut out = String::new();
    for line in text.lines() {
        if !replaced && line.trim() == marker {
            out.push_str(decision_line);
            replaced = true;
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    if !replaced {
        out.push_str(decision_line);
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn anchored_pattern_matches_only_the_exact_location() {
        let dir = Path::new("/proj");
        assert!(pattern_matches(dir, "/build", Path::new("/proj/build")));
        assert!(!pattern_matches(dir, "/build", Path::new("/proj/a/build")));
    }

    #[test]
    fn unanchored_pattern_matches_at_any_depth_by_leaf_name() {
        let dir = Path::new("/proj");
        assert!(pattern_matches(
            dir,
            "node_modules",
            Path::new("/proj/node_modules")
        ));
        assert!(pattern_matches(
            dir,
            "node_modules",
            Path::new("/proj/packages/foo/node_modules")
        ));
        assert!(!pattern_matches(
            dir,
            "node_modules",
            Path::new("/proj/packages/foo/dist")
        ));
    }

    #[test]
    fn anchored_prefix_with_double_star_matches_arbitrary_depth_after_prefix() {
        let dir = Path::new("/proj");
        assert!(pattern_matches(
            dir,
            "/packages/foo/**/build",
            Path::new("/proj/packages/foo/build")
        ));
        assert!(pattern_matches(
            dir,
            "/packages/foo/**/build",
            Path::new("/proj/packages/foo/x/y/build")
        ));
        assert!(!pattern_matches(
            dir,
            "/packages/foo/**/build",
            Path::new("/proj/packages/bar/build")
        ));
    }

    #[test]
    fn pattern_never_matches_a_path_outside_the_file_directory() {
        let dir = Path::new("/proj");
        assert!(!pattern_matches(dir, "build", Path::new("/other/build")));
    }

    #[test]
    fn parse_ignore_patterns_skips_blank_lines_and_comments() {
        let text = "\n.git\n# a comment\n  .hg  \n";
        assert_eq!(
            parse_ignore_patterns(text),
            vec![".git".to_string(), ".hg".to_string()]
        );
    }

    #[test]
    fn parse_ignore_patterns_empty_text_yields_empty() {
        assert!(parse_ignore_patterns("").is_empty());
    }

    #[test]
    fn ignore_matches_true_when_any_pattern_matches() {
        let patterns = vec![".git".to_string(), ".hg".to_string()];
        assert!(ignore_matches(
            &patterns,
            Path::new("/proj"),
            Path::new("/proj/.hg")
        ));
    }

    #[test]
    fn ignore_matches_false_when_nothing_matches() {
        let patterns = vec![".git".to_string()];
        assert!(!ignore_matches(
            &patterns,
            Path::new("/proj"),
            Path::new("/proj/src")
        ));
    }

    #[test]
    fn ignore_matches_supports_anchored_and_double_star_patterns_too() {
        // Same grammar as decision-file patterns - not just bare names.
        let patterns = vec!["/vendor/**/cache".to_string()];
        assert!(ignore_matches(
            &patterns,
            Path::new("/proj"),
            Path::new("/proj/vendor/a/b/cache")
        ));
    }

    #[test]
    fn resolve_in_file_last_matching_line_wins() {
        let text = "+ node_modules\n- /packages/foo/node_modules\n";
        let decision = resolve_in_file(
            Path::new("/proj"),
            text,
            Path::new("/proj/packages/foo/node_modules"),
        );
        assert_eq!(decision, Some(false));
    }

    #[test]
    fn resolve_in_file_ignores_comments_and_blank_lines() {
        let text = "# a comment\n\n+ node_modules\n";
        assert_eq!(
            resolve_in_file(Path::new("/proj"), text, Path::new("/proj/node_modules")),
            Some(true)
        );
    }

    #[test]
    fn resolve_in_file_none_when_nothing_matches() {
        let text = "+ target\n";
        assert_eq!(
            resolve_in_file(Path::new("/proj"), text, Path::new("/proj/node_modules")),
            None
        );
    }

    #[test]
    fn resolve_walks_up_to_the_closest_file_with_a_matching_line() {
        // Decision file at /proj (broad) says "+", but a closer,
        // nested one at /proj/packages/foo overrides with "-".
        let files = [
            (Path::new("/proj/.decisions"), "+ node_modules\n"),
            (
                Path::new("/proj/packages/foo/.decisions"),
                "- node_modules\n",
            ),
        ];
        let read = |p: &Path| {
            files
                .iter()
                .find(|(fp, _)| *fp == p)
                .map(|(_, t)| t.to_string())
        };
        let decision = resolve(
            Path::new("/proj/packages/foo/node_modules"),
            Path::new("/proj"),
            ".decisions",
            read,
        );
        assert_eq!(decision, Some(false));
    }

    #[test]
    fn resolve_keeps_walking_past_a_file_with_no_matching_line() {
        // Nested decision file exists but doesn't mention this name;
        // the walk must continue up to the broader file instead of
        // stopping just because *some* file exists.
        let files = [
            (Path::new("/proj/.decisions"), "+ node_modules\n"),
            (Path::new("/proj/packages/foo/.decisions"), "+ dist\n"),
        ];
        let read = |p: &Path| {
            files
                .iter()
                .find(|(fp, _)| *fp == p)
                .map(|(_, t)| t.to_string())
        };
        let decision = resolve(
            Path::new("/proj/packages/foo/node_modules"),
            Path::new("/proj"),
            ".decisions",
            read,
        );
        assert_eq!(decision, Some(true));
    }

    #[test]
    fn resolve_none_when_no_decision_file_exists_anywhere() {
        let read = |_: &Path| None;
        let decision = resolve(
            Path::new("/proj/packages/foo/node_modules"),
            Path::new("/proj"),
            ".decisions",
            read,
        );
        assert_eq!(decision, None);
    }

    #[test]
    fn resolve_never_walks_past_the_boundary() {
        // A decision file sitting *above* the boundary must never be
        // consulted, even if nothing below it resolves anything.
        let files = [(Path::new("/.decisions"), "+ node_modules\n")];
        let read = |p: &Path| {
            files
                .iter()
                .find(|(fp, _)| *fp == p)
                .map(|(_, t)| t.to_string())
        };
        let decision = resolve(
            Path::new("/proj/node_modules"),
            Path::new("/proj"),
            ".decisions",
            read,
        );
        assert_eq!(decision, None);
    }

    #[test]
    fn anchored_pattern_is_relative_to_the_boundary_with_a_leading_slash() {
        assert_eq!(
            anchored_pattern(
                Path::new("/proj"),
                Path::new("/proj/packages/foo/node_modules")
            ),
            Some("/packages/foo/node_modules".to_string())
        );
    }

    #[test]
    fn anchored_pattern_none_outside_the_boundary() {
        assert_eq!(
            anchored_pattern(Path::new("/proj"), Path::new("/other/node_modules")),
            None
        );
    }

    #[test]
    fn pending_marker_line_prefixes_with_a_question_mark() {
        assert_eq!(
            pending_marker_line("/packages/foo/node_modules"),
            "? /packages/foo/node_modules"
        );
    }

    #[test]
    fn needs_pending_marker_true_when_absent() {
        assert!(needs_pending_marker(
            "+ dist\n",
            "/packages/foo/node_modules"
        ));
    }

    #[test]
    fn needs_pending_marker_false_when_already_present() {
        let text = "+ dist\n? /packages/foo/node_modules\n";
        assert!(!needs_pending_marker(text, "/packages/foo/node_modules"));
    }

    #[test]
    fn needs_pending_marker_true_for_empty_file() {
        assert!(needs_pending_marker("", "/node_modules"));
    }

    #[test]
    fn toggle_or_replace_pending_replaces_the_marker_in_place() {
        let text = "+ dist\n? /node_modules\n- vendor\n";
        assert_eq!(
            toggle_or_replace_pending(text, "/node_modules", "+ /node_modules"),
            "+ dist\n+ /node_modules\n- vendor\n"
        );
    }

    #[test]
    fn toggle_or_replace_pending_appends_when_no_marker_exists() {
        let text = "+ dist\n";
        assert_eq!(
            toggle_or_replace_pending(text, "/node_modules", "- /node_modules"),
            "+ dist\n- /node_modules\n"
        );
    }

    #[test]
    fn toggle_or_replace_pending_only_replaces_the_exact_pattern() {
        let text = "? /node_modules\n? /packages/foo/node_modules\n";
        assert_eq!(
            toggle_or_replace_pending(text, "/node_modules", "+ /node_modules"),
            "+ /node_modules\n? /packages/foo/node_modules\n"
        );
    }

    #[test]
    fn toggle_or_replace_pending_lands_a_differently_patterned_replacement_in_the_same_spot() {
        // "a"/every-match-of-this-name records a broader pattern than
        // the anchored one its own pending marker used - still an
        // in-place swap, not a remove-then-append-at-the-end: the
        // replacement's pattern never has to match the search pattern.
        let text = "# a comment\n? /packages/foo/node_modules\n# another comment\n";
        assert_eq!(
            toggle_or_replace_pending(text, "/packages/foo/node_modules", "+ node_modules"),
            "# a comment\n+ node_modules\n# another comment\n"
        );
    }
}
