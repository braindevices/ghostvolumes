//! `ghostvolumes reload` (§8.0): load + merge config, validate every
//! configured root is still BTRFS-backed, compile to `compiled.tsv`,
//! write atomically. Also invoked automatically at the end of
//! `scan --save`.

use std::path::Path;

use crate::atomic_write::write_atomically;
use crate::{cache, merge};

/// Real entry point: validates roots via the actual `statfs`-based
/// check. See `reload_with_validator` for the testable core.
pub fn reload(config_dir: &Path, cache_path: &Path) -> anyhow::Result<()> {
    reload_with_validator(config_dir, cache_path, crate::btrfs::is_btrfs)
}

/// Core logic with an injectable BTRFS-validator, so the merge →
/// validate → compile → write pipeline is testable without a real
/// BTRFS filesystem (this sandbox has none at all).
fn reload_with_validator(
    config_dir: &Path,
    cache_path: &Path,
    is_btrfs: impl Fn(&Path) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let config = merge::load_all(config_dir)?;

    for root in &config.roots {
        let root_path = Path::new(root);
        let backed_by_btrfs = is_btrfs(root_path).map_err(|e| {
            anyhow::anyhow!(
                "configured root {root} could not be checked ({e}) — config is stale; \
                 re-run `ghostvolumes scan --save` or fix roots.d manually"
            )
        })?;
        if !backed_by_btrfs {
            anyhow::bail!(
                "configured root {root} is not BTRFS-backed — config is stale; \
                 re-run `ghostvolumes scan --save` or fix roots.d manually"
            );
        }
    }

    let text = cache::compile(&config);
    write_atomically(cache_path, &text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filenames;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Bundles the `config_dir`/`cache_path` pair every test below
    /// needs, plus the `TempDir` guard that must outlive them -
    /// eliminates the repeated `tempdir()` + two `.join()`s at the top
    /// of every test. Config is *not* written here - callers that want
    /// it call `write_config_dir(&paths.config_dir)` themselves
    /// (`empty_config_with_no_roots_needs_no_validation` deliberately
    /// doesn't).
    struct TestPaths {
        _root: tempfile::TempDir,
        config_dir: PathBuf,
        cache_path: PathBuf,
    }

    fn test_paths() -> TestPaths {
        let root = tempdir().unwrap();
        let config_dir = root.path().join("config");
        let cache_path = root
            .path()
            .join("data")
            .join(filenames::COMPILED_CACHE_FILE_NAME);
        TestPaths {
            _root: root,
            config_dir,
            cache_path,
        }
    }

    fn write_config_dir(dir: &Path) {
        fs::create_dir_all(dir.join(filenames::ROOTS_D_DIR)).unwrap();
        fs::create_dir_all(dir.join(filenames::WATCHED_D_DIR)).unwrap();
        fs::write(
            dir.join(filenames::ROOTS_D_DIR)
                .join(filenames::AUTO_ROOTS_FILE_NAME),
            r#"roots = ["/home/user1"]"#,
        )
        .unwrap();
        fs::write(
            dir.join(filenames::WATCHED_D_DIR)
                .join(filenames::DEFAULT_WATCHED_FILE_NAME),
            r#"names = ["node_modules"]"#,
        )
        .unwrap();
    }

    #[test]
    fn happy_path_writes_compiled_cache() {
        let paths = test_paths();
        write_config_dir(&paths.config_dir);

        reload_with_validator(&paths.config_dir, &paths.cache_path, |_| Ok(true)).unwrap();

        let text = fs::read_to_string(&paths.cache_path).unwrap();
        assert_eq!(text, "/home/user1\tnode_modules\n");
    }

    #[test]
    fn stale_non_btrfs_root_fails_loudly_and_does_not_write() {
        let paths = test_paths();
        write_config_dir(&paths.config_dir);

        let err =
            reload_with_validator(&paths.config_dir, &paths.cache_path, |_| Ok(false)).unwrap_err();
        assert!(err.to_string().contains("/home/user1"));
        assert!(err.to_string().contains("scan --save"));
        assert!(!paths.cache_path.exists());
    }

    #[test]
    fn validation_failure_does_not_clobber_existing_cache() {
        let paths = test_paths();
        write_config_dir(&paths.config_dir);
        fs::create_dir_all(paths.cache_path.parent().unwrap()).unwrap();
        fs::write(&paths.cache_path, "previous-good-cache-content").unwrap();

        let result = reload_with_validator(&paths.config_dir, &paths.cache_path, |_| Ok(false));
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&paths.cache_path).unwrap(),
            "previous-good-cache-content"
        );
    }

    #[test]
    fn validator_error_propagates() {
        let paths = test_paths();
        write_config_dir(&paths.config_dir);

        let err = reload_with_validator(&paths.config_dir, &paths.cache_path, |_| {
            Err(anyhow::anyhow!("root vanished"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("root vanished"));
        assert!(!paths.cache_path.exists());
    }

    #[test]
    fn empty_config_with_no_roots_needs_no_validation() {
        // paths.config_dir is never created — merge::load_all tolerates missing dirs.
        let paths = test_paths();

        reload_with_validator(&paths.config_dir, &paths.cache_path, |_| {
            panic!("validator must not be called when there are no roots")
        })
        .unwrap();
        assert_eq!(fs::read_to_string(&paths.cache_path).unwrap(), "");
    }

    #[test]
    fn real_reload_fails_on_this_sandbox_since_nothing_here_is_btrfs() {
        // Exercises the *actual* `reload()` entry point (real statfs
        // check) end-to-end. This sandbox has no BTRFS anywhere, so
        // the only branch reachable here is the failure path — which
        // is exactly what should happen on a non-BTRFS filesystem.
        let paths = test_paths();
        write_config_dir(&paths.config_dir); // roots = ["/home/user1"], not BTRFS here

        let err = reload(&paths.config_dir, &paths.cache_path).unwrap_err();
        assert!(err.to_string().contains("/home/user1"));
        assert!(!paths.cache_path.exists());
    }
}
