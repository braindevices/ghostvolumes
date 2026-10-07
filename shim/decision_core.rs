// Decision-file parsing and matching (replaces the git-tracked gate).
// Shared between the CLI (via `include!`) and the shim (via `mod`).
// One decision file per directory, gitignore-style: `+ <pattern>`
// (convert), `- <pattern>` (never), `? <pattern>` (pending marker,
// toggled in place later), `#` comment, or blank. Three pattern forms:
// `/name` (anchored exact), `name` (any depth, by leaf), `/a/b/**/name`
// (anchored prefix, arbitrary depth after). Plain `//` comments (not
// `//!`/`///`) since this file is spliced mid-file via `include!`.

// Fully-qualified paths throughout (not `use` at file scope): this file
// is included both mid-file into src/decision.rs and as its own module
// in the shim, so qualifying every path keeps both scopes unambiguous.

/// One parsed, non-comment, non-blank line: `+`/`-` polarity and the
/// raw pattern text (not yet matched against anything).
struct DecisionLine {
    convert: bool,
    pattern: String,
}

/// Parses one decision file's raw text into its meaningful lines, in
/// file order (so callers can apply "last matching line wins"). Ignores
/// blank lines, `#` comments, invalid patterns (see `valid_pattern`),
/// and anything not exactly `+`/`-`-prefixed — `?` pending-marker lines
/// fall into that catch-all too. Never panics on arbitrary text (a BOM
/// or non-ASCII first char used to, aborting the shim's host process).
fn parse_lines(text: &str) -> alloc_free_vec::Vec<DecisionLine> {
    let mut lines = alloc_free_vec::Vec::new();
    for line in text.lines() {
        let trimmed = line.trim().trim_start_matches('\u{feff}').trim_start();
        let (convert, rest) = if let Some(rest) = trimmed.strip_prefix('+') {
            (true, rest)
        } else if let Some(rest) = trimmed.strip_prefix('-') {
            (false, rest)
        } else {
            continue; // blank, comment, `?` marker, or malformed - ignore
        };
        let pattern = rest.trim_start();
        if pattern.is_empty() || !valid_pattern(pattern) {
            continue;
        }
        lines.push(DecisionLine {
            convert,
            pattern: pattern.to_string(),
        });
    }
    lines
}

/// A pattern names a location under its decision file's directory, so
/// `.`/`..` components (which could point anywhere, e.g. `+ /../x`) and
/// control characters (newline injection into a decision file) are
/// never valid.
pub fn valid_pattern(pattern: &str) -> bool {
    !pattern.chars().any(char::is_control)
        && pattern.split('/').all(|c| c != "." && c != "..")
}

/// Can this path be written into our one-entry-per-line files (decision
/// files, `compiled.tsv`, `project-roots.list`) and read back unchanged
/// with the same meaning? Every component must be non-empty (a leading
/// `/` aside), free of control characters (a newline or tab would split
/// the line or the row), free of leading/trailing whitespace (the
/// readers trim), and not `.`, `..` or `**` (which mean something else).
/// Names that fail are skipped with a warning, never escaped: no sensible
/// project names a volatile directory like that.
pub fn representable_path(path: &str) -> bool {
    match path.strip_prefix('/') {
        Some("") => true, // `/` itself
        Some(rest) => rest.split('/').all(representable_component),
        None => path.split('/').all(representable_component),
    }
}

fn representable_component(c: &str) -> bool {
    !c.is_empty()
        && c.trim() == c
        && !c.chars().any(char::is_control)
        && !matches!(c, "." | ".." | "**")
}

// `alloc_free_vec` is just `std::vec`, named so the doc comment above
// reads naturally - no actual no-alloc constraint here.
mod alloc_free_vec {
    pub use std::vec::Vec;
}

/// Decision/ignore files can come from a cloned repo, so they're never
/// read past this size (a symlink to `/dev/zero` is refused anyway).
pub const MAX_DECISION_FILE_BYTES: u64 = 1 << 20;

/// Opens a decision/ignore file only if it is a regular file — never
/// through a symlink (a committed `.ghostvolumes-decisions ->
/// ~/.bashrc` would get our appends), FIFO or device — at most
/// `MAX_DECISION_FILE_BYTES`. The `lstat` result is re-checked on the
/// opened fd (same dev/ino), so a swap in between is caught; a missing
/// file is created with `O_EXCL`, which never follows a symlink.
pub fn open_regular_file(
    path: &std::path::Path,
    append: bool,
) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::MetadataExt;
    let refuse = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("refusing {}: not a regular file", path.display()),
        )
    };
    let before = match std::fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(e) if append && e.kind() == std::io::ErrorKind::NotFound => {
            return std::fs::OpenOptions::new()
                .append(true)
                .create_new(true)
                .open(path);
        }
        Err(e) => return Err(e),
    };
    if !before.file_type().is_file() || before.len() > MAX_DECISION_FILE_BYTES {
        return Err(refuse());
    }
    let file = std::fs::OpenOptions::new()
        .read(!append)
        .append(append)
        .open(path)?;
    let after = file.metadata()?;
    if after.dev() != before.dev() || after.ino() != before.ino() {
        return Err(refuse());
    }
    Ok(file)
}

/// `open_regular_file`'s text, or `None` if it's absent, unsafe or
/// not UTF-8.
pub fn read_regular_file(path: &std::path::Path) -> Option<String> {
    use std::io::Read;
    let mut text = String::new();
    open_regular_file(path, false)
        .ok()?
        .take(MAX_DECISION_FILE_BYTES)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// Splits `pattern` (leading `/` already stripped) into path components
/// before/after a single `**` segment. Only one `**` is supported; more
/// than one falls back to exact-match semantics for the whole thing.
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

/// Parses a `.ghostvolumes-ignore` file into a flat list of patterns to
/// never walk into — same three pattern forms as a decision file, but no
/// `+`/`-`/`?` prefix. Dead code from the shim's perspective; only
/// `convert`/`discover` (CLI-side) call this.
#[allow(dead_code)]
pub fn parse_ignore_patterns(text: &str) -> std::vec::Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

/// `true` if `candidate` matches any of `patterns`, resolved relative to
/// `anchor_dir` — used to skip descending into `candidate` entirely,
/// not to decide whether to convert it. Dead code from the shim's
/// perspective, see `parse_ignore_patterns` above.
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
/// the *last* matching line wins (lets a narrow override follow a
/// broad rule). `None` if nothing in this file matches.
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

/// Walks up from `candidate`'s parent to (and including) `boundary`,
/// resolving against the closest decision file with any *matching*
/// line — a file with no matching line does NOT stop the walk.
/// `read_file` is injectable for testability without real files.
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

/// The anchored pattern text for `candidate`, relative to `boundary`,
/// e.g. `/packages/foo/node_modules`. `None` if `candidate` isn't under
/// `boundary`, or the result wouldn't be a `valid_pattern` (a directory
/// name with a newline would otherwise forge extra decision lines).
pub fn anchored_pattern(boundary: &std::path::Path, candidate: &std::path::Path) -> Option<String> {
    let rel = candidate.strip_prefix(boundary).ok()?;
    let pattern = format!("/{}", rel.to_string_lossy());
    (valid_pattern(&pattern) && representable_path(&pattern)).then_some(pattern)
}

/// The exact pending-marker line appended for a still-undecided
/// candidate (§4). `?`, not `#`: a `#` line is a permanent human
/// comment, `?` is a machine-owned marker `toggle_or_replace_pending`
/// can later replace in place. Already inert for resolution purposes,
/// same as a `#` comment.
#[allow(dead_code)]
pub fn pending_marker_line(pattern: &str) -> String {
    format!("? {pattern}")
}

/// `true` iff `text` doesn't already contain this exact pending-marker
/// line - best-effort dedup (§4), not airtight under concurrent
/// appends, but harmless if it isn't (just an extra line to ignore).
#[allow(dead_code)]
pub fn needs_pending_marker(text: &str, pattern: &str) -> bool {
    let line = pending_marker_line(pattern);
    !text.lines().any(|existing| existing.trim() == line)
}

/// Every anchored, wildcard-free `+`/`?` line's own pattern — surfaces
/// a candidate a filesystem walk could never discover on its own (not
/// yet on disk, or not matching a watched name). `-` and
/// wildcarded/unanchored patterns are excluded. Dead code from the
/// shim's perspective — it only appends markers, never resolves them.
#[allow(dead_code)]
pub fn parse_anchored_exact_patterns(text: &str) -> std::vec::Vec<String> {
    text.lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            let rest = trimmed
                .strip_prefix('+')
                .or_else(|| trimmed.strip_prefix('?'))?;
            let pattern = rest.trim_start();
            (pattern.starts_with('/') && !pattern.contains("**") && valid_pattern(pattern))
                .then(|| pattern.to_string())
        })
        .collect()
}

/// Replaces an existing `? <pattern>` pending-marker line in place with
/// `decision_line` (e.g. `+ <pattern>`), preserving every other line's
/// order. Only `pattern` must match the marker; `decision_line` may
/// carry a different pattern and still lands in the same spot. Appends
/// at the end instead if no matching marker is found.
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
    fn regular_file_io_refuses_symlinks_and_special_files() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("bashrc");
        std::fs::write(&target, "orig\n").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert_eq!(read_regular_file(&link), None);
        assert!(open_regular_file(&link, true).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "orig\n");

        let zero = dir.path().join("zero");
        std::os::unix::fs::symlink("/dev/zero", &zero).unwrap();
        assert_eq!(read_regular_file(&zero), None);
        assert_eq!(read_regular_file(Path::new("/dev/null")), None);

        let fifo = dir.path().join("fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        assert_eq!(read_regular_file(&fifo), None, "must not block on open");
        assert!(open_regular_file(&fifo, true).is_err());

        let subdir = dir.path().join("as_dir");
        std::fs::create_dir(&subdir).unwrap();
        assert_eq!(read_regular_file(&subdir), None);

        let big = dir.path().join("big");
        std::fs::write(&big, vec![b'#'; MAX_DECISION_FILE_BYTES as usize + 1]).unwrap();
        assert_eq!(read_regular_file(&big), None);
    }

    #[test]
    fn regular_file_io_reads_appends_and_creates() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("decisions");
        assert_eq!(read_regular_file(&path), None);
        open_regular_file(&path, true).unwrap().write_all(b"+ a\n").unwrap();
        open_regular_file(&path, true).unwrap().write_all(b"+ b\n").unwrap();
        assert_eq!(read_regular_file(&path).as_deref(), Some("+ a\n+ b\n"));
    }

    #[test]
    fn parse_lines_never_panics_and_skips_invalid_lines() {
        let text = "\u{feff} + a\n\u{e9} b\n\u{2192} c\n+ /../x\n- ./y\n+\n?\n+ z\n";
        let parsed: Vec<(bool, String)> = parse_lines(text)
            .into_iter()
            .map(|l| (l.convert, l.pattern))
            .collect();
        assert_eq!(parsed, vec![(true, "a".into()), (true, "z".into())]);
    }

    #[test]
    fn valid_pattern_rejects_dot_components_and_control_chars() {
        for bad in ["/..", "/../x", "a/../b", "./a", "/a/.", "a\nb", "a\rb", "a\tb"] {
            assert!(!valid_pattern(bad), "{bad:?}");
        }
        for good in ["node_modules", "/build", "/a/**/b", "/.venv", "..a", "a..b"] {
            assert!(valid_pattern(good), "{good:?}");
        }
    }

    #[test]
    fn a_dot_dot_plus_line_never_resolves_outside_its_directory() {
        assert_eq!(
            resolve_in_file(Path::new("/proj"), "+ /../victim\n", Path::new("/proj/../victim")),
            None
        );
    }

    #[test]
    fn representable_path_accepts_ordinary_paths_only() {
        for good in ["/", "/home/u/proj", "node_modules", "/a/.venv", "/a b/c"] {
            assert!(representable_path(good), "{good:?}");
        }
        for bad in ["/a\nb", "/a\tb", "/a/ b", "/a/b ", "/a/./b", "/a/../b", "/a/**", "/a//b", ""] {
            assert!(!representable_path(bad), "{bad:?}");
        }
    }

    #[test]
    fn anchored_pattern_none_for_whitespace_edged_or_wildcard_names() {
        for name in ["build ", " build", "**"] {
            assert_eq!(
                anchored_pattern(Path::new("/proj"), &Path::new("/proj/a").join(name)),
                None,
                "{name:?}"
            );
        }
    }

    #[test]
    fn anchored_pattern_none_for_a_name_with_a_newline() {
        assert_eq!(
            anchored_pattern(
                Path::new("/proj"),
                Path::new("/proj/a\n+ node_modules\n#/node_modules")
            ),
            None
        );
    }

    #[test]
    fn parse_anchored_exact_patterns_skips_invalid_patterns() {
        assert_eq!(
            parse_anchored_exact_patterns("+ /../x\n? /a/./b\n+ /ok\n"),
            vec!["/ok".to_string()]
        );
    }

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
    fn parse_anchored_exact_patterns_extracts_plus_and_pending_in_order() {
        let text = "+ dist\n? /packages/foo/node_modules\n# a comment\n+ /venv2\n- /cache\n";
        assert_eq!(
            parse_anchored_exact_patterns(text),
            vec!["/packages/foo/node_modules".to_string(), "/venv2".to_string()]
        );
    }

    #[test]
    fn parse_anchored_exact_patterns_excludes_unanchored_and_wildcarded_and_denied() {
        let text = "+ dist\n+ /**/venv\n? node_modules\n- /venv2\n";
        assert!(parse_anchored_exact_patterns(text).is_empty());
    }

    #[test]
    fn parse_anchored_exact_patterns_empty_for_empty_text() {
        assert!(parse_anchored_exact_patterns("").is_empty());
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
        // Replacement pattern differs from the search pattern - still an
        // in-place swap, not a remove-then-append.
        let text = "# a comment\n? /packages/foo/node_modules\n# another comment\n";
        assert_eq!(
            toggle_or_replace_pending(text, "/packages/foo/node_modules", "+ node_modules"),
            "# a comment\n+ node_modules\n# another comment\n"
        );
    }
}
