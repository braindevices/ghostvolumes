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
    use std::fs;
    use tempfile::tempdir;

    fn write_config_dir(dir: &Path) {
        fs::create_dir_all(dir.join("roots.d")).unwrap();
        fs::create_dir_all(dir.join("watched.d")).unwrap();
        fs::write(
            dir.join("roots.d/00-auto.toml"),
            r#"roots = ["/home/user1"]"#,
        )
        .unwrap();
        fs::write(
            dir.join("watched.d/00-defaults.toml"),
            r#"names = ["node_modules"]"#,
        )
        .unwrap();
    }

    #[test]
    fn happy_path_writes_compiled_cache() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        write_config_dir(&config_dir);
        let cache_path = dir.path().join("data").join("compiled.tsv");

        reload_with_validator(&config_dir, &cache_path, |_| Ok(true)).unwrap();

        let text = fs::read_to_string(&cache_path).unwrap();
        assert_eq!(text, "/home/user1\tnode_modules\n");
    }

    #[test]
    fn stale_non_btrfs_root_fails_loudly_and_does_not_write() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        write_config_dir(&config_dir);
        let cache_path = dir.path().join("data").join("compiled.tsv");

        let err = reload_with_validator(&config_dir, &cache_path, |_| Ok(false)).unwrap_err();
        assert!(err.to_string().contains("/home/user1"));
        assert!(err.to_string().contains("scan --save"));
        assert!(!cache_path.exists());
    }

    #[test]
    fn validation_failure_does_not_clobber_existing_cache() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        write_config_dir(&config_dir);
        let cache_path = dir.path().join("data").join("compiled.tsv");
        fs::create_dir_all(cache_path.parent().unwrap()).unwrap();
        fs::write(&cache_path, "previous-good-cache-content").unwrap();

        let result = reload_with_validator(&config_dir, &cache_path, |_| Ok(false));
        assert!(result.is_err());
        assert_eq!(
            fs::read_to_string(&cache_path).unwrap(),
            "previous-good-cache-content"
        );
    }

    #[test]
    fn validator_error_propagates() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        write_config_dir(&config_dir);
        let cache_path = dir.path().join("data").join("compiled.tsv");

        let err = reload_with_validator(&config_dir, &cache_path, |_| {
            Err(anyhow::anyhow!("root vanished"))
        })
        .unwrap_err();
        assert!(err.to_string().contains("root vanished"));
        assert!(!cache_path.exists());
    }

    #[test]
    fn empty_config_with_no_roots_needs_no_validation() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config"); // never created — merge::load_all tolerates missing dirs
        let cache_path = dir.path().join("data").join("compiled.tsv");

        reload_with_validator(&config_dir, &cache_path, |_| {
            panic!("validator must not be called when there are no roots")
        })
        .unwrap();
        assert_eq!(fs::read_to_string(&cache_path).unwrap(), "");
    }

    #[test]
    fn real_reload_fails_on_this_sandbox_since_nothing_here_is_btrfs() {
        // Exercises the *actual* `reload()` entry point (real statfs
        // check) end-to-end. This sandbox has no BTRFS anywhere, so
        // the only branch reachable here is the failure path — which
        // is exactly what should happen on a non-BTRFS filesystem.
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join("config");
        write_config_dir(&config_dir); // roots = ["/home/user1"], not BTRFS here
        let cache_path = dir.path().join("data").join("compiled.tsv");

        let err = reload(&config_dir, &cache_path).unwrap_err();
        assert!(err.to_string().contains("/home/user1"));
        assert!(!cache_path.exists());
    }
}
