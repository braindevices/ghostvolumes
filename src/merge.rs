//! Loads and merges `*.d/` config directories per plan §2: `roots.d` /
//! `watched.d` are simple sets (union + dedupe, no precedence).
//! `projects.d` (per-project config) was removed along with `ensure`
//! and per-project `.ghostvolumes.toml` entirely
//! (ai-work/tasks/decision-model.plan.md §7) — decision files are the
//! entire per-project mechanism now.

use std::collections::BTreeSet;
use std::path::Path;

use crate::{config, filenames};

#[derive(Debug, PartialEq, Eq, Default)]
pub struct MergedConfig {
    pub roots: Vec<String>,
    pub global_defaults: Vec<String>,
}

/// Lexically-sorted list of `*.toml` files directly inside `dir`.
/// Returns an empty list if `dir` doesn't exist — a `*.d/` dir with
/// nothing in it yet is a normal, not an error, state.
fn list_toml_files(dir: &Path) -> anyhow::Result<Vec<std::path::PathBuf>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut files: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("toml"))
        .collect();
    files.sort();
    Ok(files)
}

fn load_roots_dir(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut set = BTreeSet::new();
    for file in list_toml_files(dir)? {
        let text = std::fs::read_to_string(&file)?;
        let parsed = config::parse_roots(&text)?;
        set.extend(parsed.roots);
    }
    Ok(set.into_iter().collect())
}

fn load_watched_dir(dir: &Path) -> anyhow::Result<Vec<String>> {
    let mut set = BTreeSet::new();
    for file in list_toml_files(dir)? {
        let text = std::fs::read_to_string(&file)?;
        let parsed = config::parse_watched(&text)?;
        set.extend(parsed.names);
    }
    Ok(set.into_iter().collect())
}

/// Loads `roots.d/` and `watched.d/` under `config_dir` (e.g.
/// `~/.config/ghostvolumes`) and merges each per the rules above.
pub fn load_all(config_dir: &Path) -> anyhow::Result<MergedConfig> {
    Ok(MergedConfig {
        roots: load_roots_dir(&config_dir.join(filenames::ROOTS_D_DIR))?,
        global_defaults: load_watched_dir(&config_dir.join(filenames::WATCHED_D_DIR))?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).unwrap();
    }

    #[test]
    fn roots_union_and_dedupe_across_files() {
        let dir = tempdir().unwrap();
        let roots_d = dir.path().join(filenames::ROOTS_D_DIR);
        fs::create_dir(&roots_d).unwrap();
        write(
            &roots_d,
            filenames::AUTO_ROOTS_FILE_NAME,
            r#"roots = ["/", "/home"]"#,
        );
        write(&roots_d, "10-local.toml", r#"roots = ["/home", "/dbs"]"#);

        let merged = load_roots_dir(&roots_d).unwrap();
        assert_eq!(merged, vec!["/", "/dbs", "/home"]); // sorted, deduped
    }

    #[test]
    fn watched_union_and_dedupe_across_files() {
        let dir = tempdir().unwrap();
        let watched_d = dir.path().join(filenames::WATCHED_D_DIR);
        fs::create_dir(&watched_d).unwrap();
        write(
            &watched_d,
            filenames::DEFAULT_WATCHED_FILE_NAME,
            r#"names = ["node_modules", "target"]"#,
        );
        write(
            &watched_d,
            "10-local.toml",
            r#"names = [".venv", "target"]"#,
        );

        let merged = load_watched_dir(&watched_d).unwrap();
        assert_eq!(merged, vec![".venv", "node_modules", "target"]);
    }

    #[test]
    fn missing_dir_yields_empty_not_error() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.d");
        assert!(load_roots_dir(&missing).unwrap().is_empty());
        assert!(load_watched_dir(&missing).unwrap().is_empty());
    }

    #[test]
    fn non_toml_files_are_ignored() {
        let dir = tempdir().unwrap();
        let roots_d = dir.path().join(filenames::ROOTS_D_DIR);
        fs::create_dir(&roots_d).unwrap();
        write(
            &roots_d,
            filenames::AUTO_ROOTS_FILE_NAME,
            r#"roots = ["/"]"#,
        );
        write(&roots_d, "README.md", "not a config file");

        let merged = load_roots_dir(&roots_d).unwrap();
        assert_eq!(merged, vec!["/"]);
    }

    #[test]
    fn load_all_combines_both_dirs() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path();
        for sub in [filenames::ROOTS_D_DIR, filenames::WATCHED_D_DIR] {
            fs::create_dir(config_dir.join(sub)).unwrap();
        }
        write(
            &config_dir.join(filenames::ROOTS_D_DIR),
            filenames::AUTO_ROOTS_FILE_NAME,
            r#"roots = ["/"]"#,
        );
        write(
            &config_dir.join(filenames::WATCHED_D_DIR),
            filenames::DEFAULT_WATCHED_FILE_NAME,
            r#"names = ["node_modules"]"#,
        );

        let merged = load_all(config_dir).unwrap();
        assert_eq!(merged.roots, vec!["/"]);
        assert_eq!(merged.global_defaults, vec!["node_modules"]);
    }
}
