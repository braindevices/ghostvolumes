//! `ghostvolumes reload`: load + merge config, validate every configured
//! root is still BTRFS-backed, compile to `compiled.tsv`, write
//! atomically. Also invoked automatically at the end of `scan --save`.

use std::path::Path;

use crate::atomic_write::write_atomically;
use crate::{cache, filenames, merge};

/// Real entry point: validates roots via the actual `statfs`-based
/// check. See `reload_with_validator` for the testable core.
pub fn reload(config_dir: &Path, cache_path: &Path) -> anyhow::Result<()> {
    reload_with_validator(config_dir, cache_path, crate::btrfs::is_btrfs)
}

/// Blocking-locks `<data_dir>/reload.lock` for the whole
/// read-merge-validate-write sequence, serializing concurrent
/// `reload`/`scan --save` runs. Dropping the returned `File` releases it.
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

    let mut config = merge::load_all(config_dir)?;

    for root in &config.roots {
        let root_path = Path::new(&root.path);
        let backed_by_btrfs = is_btrfs(root_path).map_err(|e| {
            anyhow::anyhow!(
                "configured root {} could not be checked ({e}) — config is stale; \
                 re-run `ghostvolumes roots scan --save` or fix roots.d manually",
                root.path
            )
        })?;
        if !backed_by_btrfs {
            anyhow::bail!(
                "configured root {} is not BTRFS-backed — config is stale; \
                 re-run `ghostvolumes roots scan --save` or fix roots.d manually",
                root.path
            );
        }
    }

    // The shim matches on kernel-resolved (physical) paths, so roots are
    // compiled in that form too, e.g. `/home` -> `/var/home`.
    for root in &mut config.roots {
        if let Ok(real) = Path::new(&root.path).canonicalize() {
            root.path = real.display().to_string();
        }
    }
    let text = cache::compile(&config);
    write_atomically(cache_path, &text)?;
    if let Some(data_dir) = cache_path.parent() {
        crate::projects::canonicalize_entries(&data_dir.join(filenames::PROJECT_ROOTS_FILE_NAME))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filenames;
    use std::fs;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Bundles the `config_dir`/`cache_path` pair every test needs, plus
    /// the `TempDir` guard that must outlive them. Config is *not*
    /// written here; callers call `write_config_dir` themselves.
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
        fs::write(
            dir.join(filenames::ROOTS_D_DIR)
                .join(filenames::AUTO_ROOTS_FILE_NAME),
            "[\"/home/user1\"]",
        )
        .unwrap();
        fs::write(
            dir.join(filenames::ROOTS_D_DIR)
                .join(filenames::DEFAULT_WATCHES_FILE_NAME),
            r#"default-watches = ["node_modules"]"#,
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
    fn roots_and_project_roots_are_compiled_as_physical_paths() {
        let paths = test_paths();
        let real = paths
            .config_dir
            .parent()
            .unwrap()
            .canonicalize()
            .unwrap()
            .join("real");
        fs::create_dir_all(real.join("proj")).unwrap();
        let link = real.parent().unwrap().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        fs::create_dir_all(paths.config_dir.join(filenames::ROOTS_D_DIR)).unwrap();
        fs::write(
            paths
                .config_dir
                .join(filenames::ROOTS_D_DIR)
                .join(filenames::AUTO_ROOTS_FILE_NAME),
            format!("[\"{}\"]", link.display()),
        )
        .unwrap();
        fs::write(
            paths
                .config_dir
                .join(filenames::ROOTS_D_DIR)
                .join(filenames::DEFAULT_WATCHES_FILE_NAME),
            r#"default-watches = ["node_modules"]"#,
        )
        .unwrap();
        let list = paths
            .cache_path
            .parent()
            .unwrap()
            .join(filenames::PROJECT_ROOTS_FILE_NAME);
        fs::create_dir_all(list.parent().unwrap()).unwrap();
        let (via_link, real_proj) = (link.join("proj"), real.join("proj"));
        fs::write(
            &list,
            format!("{}\n{}\n/gone\n", via_link.display(), real_proj.display()),
        )
        .unwrap();

        reload_with_validator(&paths.config_dir, &paths.cache_path, |_| Ok(true)).unwrap();

        assert_eq!(
            fs::read_to_string(&paths.cache_path).unwrap(),
            format!("{}\tnode_modules\n", real.display())
        );
        assert_eq!(
            fs::read_to_string(&list).unwrap(),
            format!("{}\n/gone\n", real_proj.display()),
            "canonicalized, deduplicated, missing entry kept"
        );

        // An entry that is itself a symlink is never widened to its target.
        let self_link = real.join("proj-link");
        std::os::unix::fs::symlink(real.parent().unwrap(), &self_link).unwrap();
        fs::write(&list, format!("{}\n", self_link.display())).unwrap();
        reload_with_validator(&paths.config_dir, &paths.cache_path, |_| Ok(true)).unwrap();
        assert_eq!(
            fs::read_to_string(&list).unwrap(),
            format!("{}\n", self_link.display())
        );
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
        // Exercises the real reload() entry point end-to-end; this
        // sandbox has no BTRFS, so only the failure path is reachable.
        let paths = test_paths();
        write_config_dir(&paths.config_dir); // "/home/user1" root, not BTRFS here

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
