//! `ghostvolumes init` (§8.1): extracts the build-time-compiled shim
//! bytes to disk, writes default config skeletons. Does no compilation
//! at all — `rustc` only ever runs once, in `build.rs`, at `cargo
//! install` build time.

use std::path::Path;

use crate::filenames;

// `env!("GHOSTVOLUMES_SHIM_FILE_NAME")` - not `filenames::SHIM_FILE_NAME`
// - since `concat!` only accepts literal tokens (which `env!` expands
// to at this same compile time), not a `const` reference. Both this
// and `filenames::SHIM_FILE_NAME` read the same `build.rs`-defined
// value, so they can't drift apart even though neither references the
// other directly.
const PRELOAD_SO: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/",
    env!("GHOSTVOLUMES_SHIM_FILE_NAME")
));

const DEFAULT_WATCHED: &str = r#"names = [
    "node_modules",
    "target",
    ".venv",
    ".cache",
    "build",
]
"#;

pub fn init(config_dir: &Path, data_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(data_dir.join(filenames::SHIM_FILE_NAME), PRELOAD_SO)?;

    for sub in [filenames::ROOTS_D_DIR, filenames::WATCHED_D_DIR] {
        std::fs::create_dir_all(config_dir.join(sub))?;
    }
    let defaults_path = config_dir
        .join(filenames::WATCHED_D_DIR)
        .join(filenames::DEFAULT_WATCHED_FILE_NAME);
    if !defaults_path.exists() {
        std::fs::write(&defaults_path, DEFAULT_WATCHED)?;
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
            .join(filenames::WATCHED_D_DIR)
            .join(filenames::DEFAULT_WATCHED_FILE_NAME)
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
    fn creates_config_dot_d_directories() {
        let dirs = test_dirs();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        for sub in [filenames::ROOTS_D_DIR, filenames::WATCHED_D_DIR] {
            assert!(dirs.config_dir.join(sub).is_dir());
        }
    }

    #[test]
    fn writes_default_watched_names_when_absent() {
        let dirs = test_dirs();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        let text = std::fs::read_to_string(defaults_path(&dirs.config_dir)).unwrap();
        let parsed = crate::config::parse_watched(&text).unwrap();
        assert_eq!(
            parsed.names,
            vec!["node_modules", "target", ".venv", "build"]
        );
    }

    #[test]
    fn does_not_overwrite_existing_defaults_file() {
        let dirs = test_dirs();
        std::fs::create_dir_all(dirs.config_dir.join(filenames::WATCHED_D_DIR)).unwrap();
        std::fs::write(defaults_path(&dirs.config_dir), "names = [\"custom\"]\n").unwrap();

        init(&dirs.config_dir, &dirs.data_dir).unwrap();

        let text = std::fs::read_to_string(defaults_path(&dirs.config_dir)).unwrap();
        assert_eq!(text, "names = [\"custom\"]\n");
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
