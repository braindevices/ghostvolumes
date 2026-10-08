//! `ghostvolumes init`: writes default config skeletons, recompiles an
//! existing `compiled.tsv` after an upgrade, and removes the LD_PRELOAD
//! shim a pre-snapshot-prune version left in the data dir.

use std::path::Path;

use crate::filenames;

const DEFAULTS_TOML: &str = r#"default-watches = [
    "node_modules",
    "target",
    ".venv",
    "build",
    ".cache",
    ".uv-cache",
    ".ruff_cache",
    ".pytest_cache",
]

default-ignore = [
    ".git",
    ".hg",
    ".svn",
    ".snapshots",
]
"#;

pub fn init(config_dir: &Path, data_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    // The shim is gone; a copy an older version installed is just litter.
    // Only a regular file is removed (never through a symlink). Running
    // processes that still have it mapped are unaffected.
    let legacy = data_dir.join(filenames::LEGACY_SHIM_FILE_NAME);
    if legacy.symlink_metadata().is_ok_and(|m| m.is_file()) {
        std::fs::remove_file(&legacy)?;
        eprintln!(
            "removed the retired LD_PRELOAD shim {} (unset LD_PRELOAD in any shell still using it)",
            legacy.display()
        );
    }

    std::fs::create_dir_all(config_dir.join(filenames::ROOTS_D_DIR))?;
    let defaults_path = config_dir
        .join(filenames::ROOTS_D_DIR)
        .join(filenames::DEFAULT_WATCHES_FILE_NAME);
    if !defaults_path.exists() {
        std::fs::write(&defaults_path, DEFAULTS_TOML)?;
    }

    // An upgrade: recompile `compiled.tsv`/`project-roots.list` in the
    // form this version expects (e.g. physical paths), so `init` alone
    // completes it. A reload failure is a warning, never an `init` failure.
    let cache_path = data_dir.join(filenames::COMPILED_CACHE_FILE_NAME);
    if cache_path.exists()
        && let Err(e) = crate::reload::reload(config_dir, &cache_path)
    {
        eprintln!("warning: `reload` failed ({e}) - fix it and re-run `ghostvolumes reload`");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tempfile::tempdir;

    /// Bundles the `config_dir`/`data_dir` pair every test below needs,
    /// plus the `TempDir` guard that must outlive them - eliminates the
    /// repeated `tempdir()` + two `.join()`s at the top of every test.
    struct TestDirs {
        _root: tempfile::TempDir,
        config_dir: PathBuf,
        data_dir: PathBuf,
    }

    fn test_dirs() -> TestDirs {
        let root = tempdir().unwrap();
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");
        TestDirs {
            _root: root,
            config_dir,
            data_dir,
        }
    }

    fn defaults_path(config_dir: &Path) -> PathBuf {
        config_dir
            .join(filenames::ROOTS_D_DIR)
            .join(filenames::DEFAULT_WATCHES_FILE_NAME)
    }

    #[test]
    fn rerunning_init_reloads_an_existing_cache_and_tolerates_failure() {
        let dirs = test_dirs();
        let scratch = crate::test_support::btrfs_scratch_dir();
        let real = scratch.path().canonicalize().unwrap().join("real");
        std::fs::create_dir(&real).unwrap();
        let link = real.with_file_name("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        init(&dirs.config_dir, &dirs.data_dir).unwrap();
        std::fs::write(
            dirs.config_dir
                .join(filenames::ROOTS_D_DIR)
                .join(filenames::AUTO_ROOTS_FILE_NAME),
            format!("[\"{}\"]", link.display()),
        )
        .unwrap();
        let cache = dirs.data_dir.join(filenames::COMPILED_CACHE_FILE_NAME);
        std::fs::write(&cache, format!("{}\tnode_modules\n", link.display())).unwrap();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();
        let text = std::fs::read_to_string(&cache).unwrap();
        assert!(text.starts_with(&format!("{}\t", real.display())), "{text}");

        // A root that's gone makes `reload` fail; `init` still succeeds.
        std::fs::remove_file(&link).unwrap();
        init(&dirs.config_dir, &dirs.data_dir).unwrap();
    }

    #[test]
    fn writes_default_ignore_when_absent() {
        let dirs = test_dirs();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        let text = std::fs::read_to_string(defaults_path(&dirs.config_dir)).unwrap();
        let parsed = crate::config::parse_roots(&text).unwrap();
        assert_eq!(
            parsed.default_ignore,
            Some(
                vec![".git", ".hg", ".svn", ".snapshots"]
                    .into_iter()
                    .map(String::from)
                    .collect()
            )
        );
    }

    #[test]
    fn creates_config_dot_d_directory() {
        let dirs = test_dirs();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        assert!(dirs.config_dir.join(filenames::ROOTS_D_DIR).is_dir());
    }

    #[test]
    fn writes_default_watches_when_absent() {
        let dirs = test_dirs();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        let text = std::fs::read_to_string(defaults_path(&dirs.config_dir)).unwrap();
        let parsed = crate::config::parse_roots(&text).unwrap();
        assert_eq!(
            parsed.default_watches,
            Some(
                vec![
                    "node_modules",
                    "target",
                    ".venv",
                    "build",
                    ".cache",
                    ".uv-cache",
                    ".ruff_cache",
                    ".pytest_cache"
                ]
                .into_iter()
                .map(String::from)
                .collect()
            )
        );
    }

    #[test]
    fn does_not_overwrite_existing_defaults_file() {
        let dirs = test_dirs();
        std::fs::create_dir_all(dirs.config_dir.join(filenames::ROOTS_D_DIR)).unwrap();
        std::fs::write(
            defaults_path(&dirs.config_dir),
            "default-watches = [\"custom\"]\n",
        )
        .unwrap();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        let text = std::fs::read_to_string(defaults_path(&dirs.config_dir)).unwrap();
        assert_eq!(text, "default-watches = [\"custom\"]\n");
    }

    #[test]
    fn idempotent_second_run_succeeds() {
        let dirs = test_dirs();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();
        init(&dirs.config_dir, &dirs.data_dir).unwrap();
    }

    #[test]
    fn a_leftover_shim_is_removed_but_never_through_a_symlink() {
        let dirs = test_dirs();
        std::fs::create_dir_all(&dirs.data_dir).unwrap();
        let legacy = dirs.data_dir.join(filenames::LEGACY_SHIM_FILE_NAME);
        std::fs::write(&legacy, b"\x7fELF").unwrap();
        init(&dirs.config_dir, &dirs.data_dir).unwrap();
        assert!(!legacy.exists());

        let target = dirs.data_dir.join("elsewhere");
        std::fs::write(&target, b"keep").unwrap();
        std::os::unix::fs::symlink(&target, &legacy).unwrap();
        init(&dirs.config_dir, &dirs.data_dir).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }
}
