//! `ghostvolumes init`: extracts the build-time-compiled shim bytes to
//! disk, writes default config skeletons. Does no compilation itself —
//! `rustc` only runs once, in `build.rs`, at `cargo install` time.

use std::path::Path;

use crate::filenames;

// Uses `env!(...)` directly, not `filenames::SHIM_FILE_NAME`, since
// `concat!` only accepts literal tokens, not a `const` reference. Both
// read the same `build.rs`-defined value, so they can't drift apart.
const PRELOAD_SO: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/",
    env!("GHOSTVOLUMES_SHIM_FILE_NAME")
));

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
    // Rename, never truncate in place: every preloaded process has the
    // old file mmapped, and rewriting its pages under it means SIGBUS.
    crate::atomic_write::write_atomically(&data_dir.join(filenames::SHIM_FILE_NAME), PRELOAD_SO)?;

    std::fs::create_dir_all(config_dir.join(filenames::ROOTS_D_DIR))?;
    let defaults_path = config_dir
        .join(filenames::ROOTS_D_DIR)
        .join(filenames::DEFAULT_WATCHES_FILE_NAME);
    if !defaults_path.exists() {
        std::fs::write(&defaults_path, DEFAULTS_TOML)?;
    }

    // An upgrade: recompile `compiled.tsv`/`project-roots.list` in the
    // form this version's shim expects (e.g. physical paths), so `init`
    // alone completes it. Never fails `init` — the shim is installed.
    let cache_path = data_dir.join(filenames::COMPILED_CACHE_FILE_NAME);
    if cache_path.exists()
        && let Err(e) = crate::reload::reload(config_dir, &cache_path)
    {
        eprintln!(
            "warning: shim installed, but `reload` failed ({e}) - fix it and re-run `ghostvolumes reload`"
        );
    }

    Ok(())
}

/// `false` if the installed shim isn't this binary's embedded one —
/// missing, or left over from before a `cargo install` upgrade (only
/// `init` copies it to disk), so it may disagree with this CLI's file
/// formats and lock paths.
pub fn shim_is_current(data_dir: &Path) -> bool {
    let path = data_dir.join(filenames::SHIM_FILE_NAME);
    std::fs::metadata(&path).is_ok_and(|m| m.len() == PRELOAD_SO.len() as u64)
        && std::fs::read(&path).is_ok_and(|bytes| bytes == PRELOAD_SO)
}

/// The shim passes every call through unless the data dir belongs to the
/// process's euid (see `data_dir_owned_by_euid` in `shim/preload.rs`).
pub fn data_dir_owned_by_euid(data_dir: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(data_dir).is_ok_and(|m| m.uid() == unsafe { libc::geteuid() })
}

/// One stderr line when `shim_is_current` is false; stdout stays clean
/// for `shell-init`'s `eval`.
pub fn warn_if_shim_stale(data_dir: &Path) {
    if !shim_is_current(data_dir) {
        eprintln!(
            "warning: the installed shim is missing or out of date - run `ghostvolumes init`"
        );
    }
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
    fn writes_preload_so_bytes() {
        let dirs = test_dirs();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        let written = std::fs::read(dirs.data_dir.join(filenames::SHIM_FILE_NAME)).unwrap();
        assert_eq!(written, PRELOAD_SO);
        assert!(!written.is_empty());
    }

    #[test]
    fn rerunning_replaces_the_shim_by_rename_not_in_place() {
        use std::os::unix::fs::MetadataExt;
        let dirs = test_dirs();
        let so = dirs.data_dir.join(filenames::SHIM_FILE_NAME);
        init(&dirs.config_dir, &dirs.data_dir).unwrap();
        // Holding the old inode open stands in for a process that has it
        // mmapped: an in-place rewrite would change what it sees.
        let old = std::fs::File::open(&so).unwrap();
        let old_ino = old.metadata().unwrap().ino();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        assert_ne!(std::fs::metadata(&so).unwrap().ino(), old_ino);
        assert_eq!(std::fs::read(&so).unwrap(), PRELOAD_SO);
        assert_eq!(old.metadata().unwrap().len(), PRELOAD_SO.len() as u64);
    }

    #[test]
    fn shim_is_current_tracks_the_installed_bytes() {
        let dirs = test_dirs();
        assert!(!shim_is_current(&dirs.data_dir), "missing");
        init(&dirs.config_dir, &dirs.data_dir).unwrap();
        assert!(shim_is_current(&dirs.data_dir));
        let so = dirs.data_dir.join(filenames::SHIM_FILE_NAME);
        let mut stale = PRELOAD_SO.to_vec();
        stale[0] ^= 1;
        std::fs::write(&so, stale).unwrap();
        assert!(
            !shim_is_current(&dirs.data_dir),
            "same size, different bytes"
        );
        init(&dirs.config_dir, &dirs.data_dir).unwrap();
        assert!(shim_is_current(&dirs.data_dir));
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
        assert!(shim_is_current(&dirs.data_dir));
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

        assert!(dirs.data_dir.join(filenames::SHIM_FILE_NAME).exists());
    }

    #[test]
    fn extracted_preload_so_is_a_valid_shared_object() {
        let dirs = test_dirs();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        let bytes = std::fs::read(dirs.data_dir.join(filenames::SHIM_FILE_NAME)).unwrap();
        // ELF magic number: 0x7f 'E' 'L' 'F'
        assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    }
}
