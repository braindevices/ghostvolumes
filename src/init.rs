//! `ghostvolumes init` (§8.1): extracts the build-time-compiled shim
//! bytes to disk, writes default config skeletons. Does no compilation
//! at all — `rustc` only ever runs once, in `build.rs`, at `cargo
//! install` build time.

use std::path::Path;

/// The shim's on-disk filename — deliberately not a generic
/// `preload.so` some other tool could also be using: this exact name
/// is what shows up in `LD_PRELOAD`, `ps`, `/proc/*/maps`, and
/// `preload_guard`'s own refusal message, so it needs to be
/// identifiable at a glance. Matches `build.rs`'s compiled-artifact
/// name exactly (the `include_bytes!` path below).
pub const SHIM_FILE_NAME: &str = "libghostvolumes_shim.so";

const PRELOAD_SO: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libghostvolumes_shim.so"));

const DEFAULT_WATCHED: &str = "names = [\"node_modules\", \"target\", \".venv\", \"build\"]\n";

pub fn init(config_dir: &Path, data_dir: &Path) -> anyhow::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    std::fs::write(data_dir.join(SHIM_FILE_NAME), PRELOAD_SO)?;

    for sub in ["roots.d", "watched.d"] {
        std::fs::create_dir_all(config_dir.join(sub))?;
    }
    let defaults_path = config_dir.join("watched.d").join("00-defaults.toml");
    if !defaults_path.exists() {
        std::fs::write(&defaults_path, DEFAULT_WATCHED)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_preload_so_bytes() {
        let root = tempdir().unwrap();
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");

        init(&config_dir, &data_dir).unwrap();

        let written = std::fs::read(data_dir.join(SHIM_FILE_NAME)).unwrap();
        assert_eq!(written, PRELOAD_SO);
        assert!(!written.is_empty());
    }

    #[test]
    fn creates_config_dot_d_directories() {
        let root = tempdir().unwrap();
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");

        init(&config_dir, &data_dir).unwrap();

        for sub in ["roots.d", "watched.d"] {
            assert!(config_dir.join(sub).is_dir());
        }
    }

    #[test]
    fn writes_default_watched_names_when_absent() {
        let root = tempdir().unwrap();
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");

        init(&config_dir, &data_dir).unwrap();

        let text = std::fs::read_to_string(config_dir.join("watched.d/00-defaults.toml")).unwrap();
        let parsed = crate::config::parse_watched(&text).unwrap();
        assert_eq!(
            parsed.names,
            vec!["node_modules", "target", ".venv", "build"]
        );
    }

    #[test]
    fn does_not_overwrite_existing_defaults_file() {
        let root = tempdir().unwrap();
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");
        std::fs::create_dir_all(config_dir.join("watched.d")).unwrap();
        std::fs::write(
            config_dir.join("watched.d/00-defaults.toml"),
            "names = [\"custom\"]\n",
        )
        .unwrap();

        init(&config_dir, &data_dir).unwrap();

        let text = std::fs::read_to_string(config_dir.join("watched.d/00-defaults.toml")).unwrap();
        assert_eq!(text, "names = [\"custom\"]\n");
    }

    #[test]
    fn idempotent_second_run_succeeds() {
        let root = tempdir().unwrap();
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");

        init(&config_dir, &data_dir).unwrap();
        init(&config_dir, &data_dir).unwrap();

        assert!(data_dir.join(SHIM_FILE_NAME).exists());
    }

    #[test]
    fn extracted_preload_so_is_a_valid_shared_object() {
        let root = tempdir().unwrap();
        let config_dir = root.path().join("config");
        let data_dir = root.path().join("data");

        init(&config_dir, &data_dir).unwrap();

        let bytes = std::fs::read(data_dir.join(SHIM_FILE_NAME)).unwrap();
        // ELF magic number: 0x7f 'E' 'L' 'F'
        assert_eq!(&bytes[0..4], &[0x7f, b'E', b'L', b'F']);
    }
}
