// Pure, git-independent version-computation logic, split out of
// `build.rs` so it can be exercised by real `#[test]`s: `cargo test`
// never compiles `build.rs` itself with `--cfg test` (it isn't a test
// target Cargo knows about), so a `#[cfg(test)] mod tests` written
// directly inside `build.rs` would silently never run. `include!`'d
// verbatim into both `build.rs` (where it's used for real) and a
// `#[cfg(test)]`-only module in `src/main.rs` (where it's tested) -
// the same `include!`-sharing pattern this project already uses for
// `shim/*_core.rs` between the LD_PRELOAD shim and the main CLI, just
// one-directional here since only `build.rs` needs the real behavior
// at build time.

/// Parses a `vX.Y.Z` (or bare `X.Y.Z`) tag name into its numeric parts.
fn parse_tag(tag: &str) -> Option<(u64, u64, u64)> {
    let stripped = tag.strip_prefix('v').unwrap_or(tag);
    let mut parts = stripped.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next()?.parse().ok()?;
    Some((major, minor, patch))
}

/// Maps this project's GitFlow-shaped branch model
/// (`.github/workflows/ci.yml`: `main` = release, `develop` =
/// pre-release, plus `hotfix/*`/`feature/*`) onto a full SemVer version
/// for `--version`, bumping the base number (not just adding a suffix)
/// off `tag` - the latest release - rather than trusting
/// `cargo_pkg_version` to already reflect the next release.
///
/// That bump is what keeps SemVer precedence correct after a release:
/// a pre-release suffix left on the *same* base number as the release
/// just tagged (e.g. "0.3.2-alpha" right after tagging v0.3.2) would
/// sort *below* that release, making ongoing development look older
/// than what's already shipped. `develop` and `feature/*` branches
/// build toward the next *minor* release (`X.(Y+1).0-`), matching this
/// project's release rhythm where `develop` accumulates features;
/// `hotfix/*` branches patch an already-released version directly
/// (`X.Y.(Z+1)-`). `main`/`master`/detached HEAD trust the latest tag
/// directly (`{major}.{minor}.{patch}`, no suffix) rather than
/// `cargo_pkg_version` - the latter is only the fallback when no tag
/// is reachable at all (see `latest_tag_version`'s doc comment), so
/// `Cargo.toml`'s `version` field no longer needs a human to keep it
/// in sync with each release tag.
fn compute_version(branch: Option<&str>, tag: Option<(u64, u64, u64)>, cargo_pkg_version: &str) -> String {
    match branch {
        Some("main") | Some("master") | None => match tag {
            Some((major, minor, patch)) => format!("{major}.{minor}.{patch}"),
            None => cargo_pkg_version.to_string(),
        },
        Some("develop") => match tag {
            Some((major, minor, _patch)) => format!("{major}.{}.0-alpha", minor + 1),
            None => format!("{cargo_pkg_version}-alpha"),
        },
        Some(b) if b.starts_with("hotfix/") => match tag {
            Some((major, minor, patch)) => format!("{major}.{minor}.{}-rc", patch + 1),
            None => format!("{cargo_pkg_version}-rc"),
        },
        Some(_) => match tag {
            Some((major, minor, _patch)) => format!("{major}.{}.0-dev", minor + 1),
            None => format!("{cargo_pkg_version}-dev"),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_v_prefixed_tag() {
        assert_eq!(parse_tag("v0.3.2"), Some((0, 3, 2)));
    }

    #[test]
    fn parses_bare_tag() {
        assert_eq!(parse_tag("1.2.3"), Some((1, 2, 3)));
    }

    #[test]
    fn rejects_malformed_tag() {
        assert_eq!(parse_tag("not-a-version"), None);
        assert_eq!(parse_tag("v1.2"), None);
    }

    #[test]
    fn main_uses_latest_tag_even_when_cargo_pkg_version_disagrees() {
        assert_eq!(
            compute_version(Some("main"), Some((0, 8, 0)), "0.3.2"),
            "0.8.0"
        );
        assert_eq!(compute_version(None, Some((0, 8, 0)), "0.3.2"), "0.8.0");
    }

    #[test]
    fn main_falls_back_to_cargo_pkg_version_without_a_tag() {
        assert_eq!(
            compute_version(Some("main"), None, "0.3.2"),
            "0.3.2"
        );
        assert_eq!(compute_version(None, None, "0.3.2"), "0.3.2");
    }

    #[test]
    fn develop_bumps_minor_with_alpha_suffix() {
        assert_eq!(
            compute_version(Some("develop"), Some((0, 3, 2)), "0.3.2"),
            "0.4.0-alpha"
        );
    }

    #[test]
    fn hotfix_bumps_patch_with_rc_suffix() {
        assert_eq!(
            compute_version(Some("hotfix/urgent-fix"), Some((0, 3, 2)), "0.3.2"),
            "0.3.3-rc"
        );
    }

    #[test]
    fn feature_branch_bumps_minor_with_dev_suffix() {
        assert_eq!(
            compute_version(Some("feature/foo"), Some((0, 3, 2)), "0.3.2"),
            "0.4.0-dev"
        );
    }

    #[test]
    fn falls_back_to_cargo_pkg_version_without_a_tag() {
        assert_eq!(
            compute_version(Some("develop"), None, "0.3.2"),
            "0.3.2-alpha"
        );
        assert_eq!(compute_version(Some("hotfix/x"), None, "0.3.2"), "0.3.2-rc");
        assert_eq!(compute_version(Some("feature/x"), None, "0.3.2"), "0.3.2-dev");
    }
}
