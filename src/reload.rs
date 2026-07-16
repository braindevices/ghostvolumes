//! `ghostvolumes reload` (§8.0): load + merge config, validate every
//! configured root is still BTRFS-backed, compile to `compiled.tsv`,
//! write atomically. Also invoked automatically at the end of
//! `scan --save`.

use std::path::Path;

use crate::atomic_write::write_atomically;
use crate::{cache, filenames, merge};

/// Real entry point: validates roots via the actual `statfs`-based
/// check. See `reload_with_validator` for the testable core.
pub fn reload(config_dir: &Path, cache_path: &Path) -> anyhow::Result<()> {
    reload_with_validator(config_dir, cache_path, crate::btrfs::is_btrfs)
}

/// Blocking-locks `<data_dir>/reload.lock` for the whole
/// read-merge-validate-write sequence below (ai-work/tasks/atomic-file-io.plan.md
/// §1), fully serializing concurrent `reload`/`scan --save` runs rather
/// than just avoiding the byte-level temp-file corruption `atomic_write`
/// already prevents on its own. `cache_path`'s parent is always the
/// data dir in every real caller (`main.rs` always constructs it as
/// `data_dir.join(COMPILED_CACHE_FILE_NAME)`) - deriving it here avoids
/// adding a `data_dir` parameter nothing else in this function needs.
/// Returns the held `File` - the caller keeps it alive for as long as
/// the lock should be held; dropping it releases the lock.
fn lock_for_reload(cache_path: &Path) -> anyhow::Result<std::fs::File> {
    let data_dir = cache_path.parent().ok_or_else(|| {
        anyhow::anyhow!(
            "cache path {} has no parent directory",
            cache_path.display()
        )
    })?;
    let lock_path = data_dir.join(filenames::RELOAD_LOCK_FILE_NAME);
    let lock_file = crate::lock::open_lock_file(&lock_path)?;
    lock_file.lock()?;
    Ok(lock_file)
}

/// Core logic with an injectable BTRFS-validator, so the merge →
/// validate → compile → write pipeline is testable without a real
/// BTRFS filesystem (this sandbox has none at all).
fn reload_with_validator(
    config_dir: &Path,
    cache_path: &Path,
    is_btrfs: impl Fn(&Path) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let _lock = lock_for_reload(cache_path)?;

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

    #[test]
    fn concurrent_reload_calls_serialize_via_the_reload_lock() {
        let paths = test_paths();
        write_config_dir(&paths.config_dir);

        // Hold reload.lock ourselves first, simulating another
        // in-flight reload - reload_with_validator must block on it
        // rather than proceeding concurrently.
        let data_dir = paths.cache_path.parent().unwrap();
        std::fs::create_dir_all(data_dir).unwrap();
        let lock_path = data_dir.join(filenames::RELOAD_LOCK_FILE_NAME);
        let lock_file = crate::lock::open_lock_file(&lock_path).unwrap();
        lock_file.lock().unwrap();

        let config_dir = paths.config_dir.clone();
        let cache_path = paths.cache_path.clone();
        let handle = std::thread::spawn(move || {
            reload_with_validator(&config_dir, &cache_path, |_| Ok(true)).unwrap();
        });

        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(
            !handle.is_finished(),
            "reload_with_validator should still be blocked while the lock is held"
        );

        drop(lock_file);
        handle.join().unwrap();
        assert_eq!(
            fs::read_to_string(&paths.cache_path).unwrap(),
            "/home/user1\tnode_modules\n"
        );
    }
}
