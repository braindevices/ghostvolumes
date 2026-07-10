//! `ghostvolumes ensure <path>` (§6): invoked by the cd-hook on every
//! directory change. Proactively creates a project's `proactive`
//! names as empty subvolumes ahead of any tool run — this is what
//! neutralizes most of the static-binary LD_PRELOAD gap in practice.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::Path;

use crate::{btrfs, cache, registration};

enum TargetStatus {
    Missing,
    PlainDirectory,
    Subvolume,
}

fn target_status(path: &Path) -> TargetStatus {
    if !path.exists() {
        return TargetStatus::Missing;
    }
    if btrfs::is_subvolume(path).unwrap_or(false) {
        TargetStatus::Subvolume
    } else {
        TargetStatus::PlainDirectory
    }
}

fn hash_str(s: &str) -> u64 {
    let mut hasher = DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// `true` iff a warning for `project_root` was already issued this
/// session (touches a marker as a side effect if not). "Session" means
/// one interactive shell's lifetime: `ensure` is a fresh process every
/// invocation, so the dedup key is the invoking shell's PID (its own
/// immediate parent — `ensure`'s parent process *is* the shell that
/// ran it, so no extra argument or env var is needed to learn it) plus
/// a hash of the project path.
fn already_warned_this_session(
    runtime_dir: &Path,
    session_id: i32,
    project_root: &str,
) -> anyhow::Result<bool> {
    let marker_dir = runtime_dir.join("ghostvolumes").join("warned");
    std::fs::create_dir_all(&marker_dir)?;
    let marker = marker_dir.join(format!("{session_id}-{:x}", hash_str(project_root)));
    if marker.exists() {
        Ok(true)
    } else {
        std::fs::write(&marker, "")?;
        Ok(false)
    }
}

/// `${XDG_RUNTIME_DIR:-/tmp}` — normally tmpfs and cleared on logout,
/// so warning markers don't need explicit cleanup (§6).
pub fn runtime_dir() -> std::path::PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from("/tmp"))
}

#[allow(clippy::too_many_arguments)]
pub fn ensure(
    pwd: &Path,
    config_dir: &Path,
    data_dir: &Path,
    cache_path: &Path,
    runtime_dir: &Path,
    session_id: i32,
) -> anyhow::Result<()> {
    // Repo-local .ghostvolumes.toml registration (§6) — one cheap
    // stat() for the common case where it doesn't exist. Deliberately
    // unconditional on an existing project match (not gated behind a
    // root check): checking for a project match first would create a
    // chicken-and-egg problem for a repo's *first-ever* registration,
    // since that's exactly what turns "no project here" into "a
    // project is here now."
    registration::register_if_needed(pwd, config_dir, data_dir, cache_path)?;

    let text = std::fs::read_to_string(cache_path).unwrap_or_default();
    let rows = cache::parse(&text);
    let Some((project_root, names)) = cache::proactive_project_for(&rows, pwd) else {
        return Ok(()); // no project match => no-op immediately (§6)
    };

    for name in names {
        let target = Path::new(&project_root).join(&name);
        match target_status(&target) {
            TargetStatus::Missing => {
                btrfs::create_subvolume(Path::new(&project_root), &name)?;
            }
            TargetStatus::PlainDirectory => {
                if !already_warned_this_session(runtime_dir, session_id, &project_root)? {
                    eprintln!(
                        "ghostvolumes: {} already exists as a plain directory, not a subvolume \
                         (leaving it alone — see `ghostvolumes convert` to migrate it)",
                        target.display()
                    );
                }
            }
            TargetStatus::Subvolume => {} // already correct, no-op
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::btrfs_scratch_dir;
    use tempfile::tempdir;

    fn write_cache(path: &Path, rows: &[(&str, &str, bool)]) {
        let mut text = String::new();
        for (prefix, name, proactive) in rows {
            text.push_str(prefix);
            text.push('\t');
            text.push_str(name);
            if *proactive {
                text.push_str("\tproactive");
            }
            text.push('\n');
        }
        std::fs::write(path, text).unwrap();
    }

    fn config_dir() -> tempfile::TempDir {
        tempdir().unwrap()
    }

    #[test]
    fn no_project_match_is_a_pure_noop() {
        let dir = tempdir().unwrap();
        let cache_path = dir.path().join("compiled.tsv");
        write_cache(&cache_path, &[("/home/user1/app", "node_modules", true)]);

        let config = config_dir();
        ensure(
            Path::new("/tmp/unrelated"),
            config.path(),
            dir.path(),
            &cache_path,
            dir.path(),
            1234,
        )
        .unwrap();
        // No panic, no filesystem side effects to check beyond "it returned Ok".
    }

    #[test]
    fn missing_proactive_name_is_created_as_a_real_subvolume() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = tempdir().unwrap();
        let cache_path = cache_dir.path().join("compiled.tsv");
        write_cache(
            &cache_path,
            &[(scratch.path().to_str().unwrap(), "node_modules", true)],
        );

        let config = config_dir();
        ensure(
            scratch.path(),
            config.path(),
            cache_dir.path(),
            &cache_path,
            cache_dir.path(),
            1234,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&scratch.path().join("node_modules")).unwrap());
    }

    #[test]
    fn watch_only_name_is_never_proactively_created() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = tempdir().unwrap();
        let cache_path = cache_dir.path().join("compiled.tsv");
        // "dist" is watch-only (no proactive marker) - must not appear.
        write_cache(
            &cache_path,
            &[(scratch.path().to_str().unwrap(), "dist", false)],
        );

        let config = config_dir();
        ensure(
            scratch.path(),
            config.path(),
            cache_dir.path(),
            &cache_path,
            cache_dir.path(),
            1234,
        )
        .unwrap();

        assert!(!scratch.path().join("dist").exists());
    }

    #[test]
    fn existing_subvolume_is_left_alone() {
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "target").unwrap();
        let cache_dir = tempdir().unwrap();
        let cache_path = cache_dir.path().join("compiled.tsv");
        write_cache(
            &cache_path,
            &[(scratch.path().to_str().unwrap(), "target", true)],
        );

        let config = config_dir();
        ensure(
            scratch.path(),
            config.path(),
            cache_dir.path(),
            &cache_path,
            cache_dir.path(),
            1234,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&scratch.path().join("target")).unwrap());
    }

    #[test]
    fn existing_plain_directory_is_left_alone_and_warned_once() {
        let scratch = btrfs_scratch_dir();
        std::fs::create_dir(scratch.path().join("build")).unwrap();
        let cache_dir = tempdir().unwrap();
        let cache_path = cache_dir.path().join("compiled.tsv");
        write_cache(
            &cache_path,
            &[(scratch.path().to_str().unwrap(), "build", true)],
        );

        let config = config_dir();
        ensure(
            scratch.path(),
            config.path(),
            cache_dir.path(),
            &cache_path,
            cache_dir.path(),
            1234,
        )
        .unwrap();

        // Still a plain directory - never silently converted.
        assert!(!btrfs::is_subvolume(&scratch.path().join("build")).unwrap());
        assert!(scratch.path().join("build").is_dir());
    }

    #[test]
    fn already_warned_this_session_dedupes_by_session_and_project() {
        let dir = tempdir().unwrap();
        assert!(!already_warned_this_session(dir.path(), 1234, "/home/user1/app").unwrap());
        assert!(already_warned_this_session(dir.path(), 1234, "/home/user1/app").unwrap());
        // Different session (PID) => warns again.
        assert!(!already_warned_this_session(dir.path(), 5678, "/home/user1/app").unwrap());
        // Different project, same session => warns again.
        assert!(!already_warned_this_session(dir.path(), 1234, "/home/user1/other").unwrap());
    }

    #[test]
    fn nested_path_under_project_root_still_matches() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = tempdir().unwrap();
        let cache_path = cache_dir.path().join("compiled.tsv");
        write_cache(
            &cache_path,
            &[(scratch.path().to_str().unwrap(), "node_modules", true)],
        );

        let nested = scratch.path().join("src/deep/nested");
        std::fs::create_dir_all(&nested).unwrap();
        let config = config_dir();
        ensure(
            &nested,
            config.path(),
            cache_dir.path(),
            &cache_path,
            cache_dir.path(),
            1234,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&scratch.path().join("node_modules")).unwrap());
    }

    #[test]
    fn missing_cache_file_is_a_pure_noop_not_an_error() {
        let scratch = btrfs_scratch_dir();
        let cache_dir = tempdir().unwrap();
        let cache_path = cache_dir.path().join("compiled.tsv"); // never created

        let config = config_dir();
        ensure(
            scratch.path(),
            config.path(),
            cache_dir.path(),
            &cache_path,
            cache_dir.path(),
            1234,
        )
        .unwrap();
    }

    #[test]
    fn ghostvolumes_toml_at_pwd_gets_registered_and_its_proactive_names_get_created() {
        let scratch = btrfs_scratch_dir();
        std::fs::write(
            scratch.path().join(".ghostvolumes.toml"),
            "proactive = [\"node_modules\"]\n",
        )
        .unwrap();
        let cache_dir = tempdir().unwrap();
        let cache_path = cache_dir.path().join("compiled.tsv");
        // No pre-existing compiled.tsv entry for this path at all - the
        // project must come purely from .ghostvolumes.toml registration.
        std::fs::write(&cache_path, "").unwrap();

        let config = config_dir();
        ensure(
            scratch.path(),
            config.path(),
            cache_dir.path(),
            &cache_path,
            cache_dir.path(),
            1234,
        )
        .unwrap();

        assert!(btrfs::is_subvolume(&scratch.path().join("node_modules")).unwrap());
    }
}
