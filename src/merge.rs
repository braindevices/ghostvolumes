//! Loads and layers `roots.d/*.toml`: files are read in sorted order
//! and merged with **last file wins per field**, for `default-watches`,
//! `default-ignore`, and each root's own `enabled`/`watches`. A root
//! disabled by any file is dropped entirely, with no cascade to other
//! roots nested under its path. Per-root/per-project ignore patterns
//! live in `.ghostvolumes-ignore` files instead, read directly by
//! `convert`/`discover`, not merged here.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use crate::{config, filenames};

/// One root path, fully resolved: `enabled = false` roots are filtered
/// out before this stage, and `watches` already reflects its own
/// override or `default-watches`.
#[derive(Debug, PartialEq, Eq, Default)]
pub struct ResolvedRoot {
    pub path: String,
    pub watches: Vec<String>,
}

#[derive(Debug, PartialEq, Default)]
pub struct MergedConfig {
    pub roots: Vec<ResolvedRoot>,
    /// The fully-merged `default-ignore` list (last file wins, same as
    /// `default-watches`) — global, not per-root.
    pub ignore: Vec<String>,
}

impl MergedConfig {
    /// Every watched name across every resolved root, deduped and
    /// sorted, for callers that don't need per-root scoping (e.g.
    /// `discover`). `cache::compile` still uses each root's own list.
    pub fn all_watched_names(&self) -> Vec<String> {
        let mut set: BTreeSet<String> = BTreeSet::new();
        for root in &self.roots {
            set.extend(root.watches.iter().cloned());
        }
        set.into_iter().collect()
    }
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

fn load_roots_dir(dir: &Path) -> anyhow::Result<MergedConfig> {
    let mut default_watches: Vec<String> = Vec::new();
    let mut default_ignore: Vec<String> = Vec::new();
    let mut roots: BTreeMap<String, config::RawRootEntry> = BTreeMap::new();

    for file in list_toml_files(dir)? {
        let text = std::fs::read_to_string(&file)?;
        let parsed = config::parse_roots(&text)?;
        if let Some(dw) = parsed.default_watches {
            default_watches = dw;
        }
        if let Some(di) = parsed.default_ignore {
            default_ignore = di;
        }
        for (path, entry) in parsed.roots {
            let merged_entry = roots.entry(path).or_default();
            if entry.enabled.is_some() {
                merged_entry.enabled = entry.enabled;
            }
            if entry.watches.is_some() {
                merged_entry.watches = entry.watches;
            }
        }
    }

    let resolved = roots
        .into_iter()
        .filter(|(_, entry)| entry.enabled.unwrap_or(true))
        .map(|(path, entry)| ResolvedRoot {
            path,
            watches: entry.watches.unwrap_or_else(|| default_watches.clone()),
        })
        .collect();

    Ok(MergedConfig {
        roots: resolved,
        ignore: default_ignore,
    })
}

/// Loads `roots.d/` under `config_dir` (e.g. `~/.config/ghostvolumes`).
pub fn load_all(config_dir: &Path) -> anyhow::Result<MergedConfig> {
    load_roots_dir(&config_dir.join(filenames::ROOTS_D_DIR))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn write(dir: &Path, name: &str, contents: &str) {
        fs::write(dir.join(name), contents).unwrap();
    }

    fn names(watches: &[&str]) -> Vec<String> {
        watches.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn missing_dir_yields_empty_not_error() {
        let dir = tempdir().unwrap();
        let missing = dir.path().join("does-not-exist.d");
        assert_eq!(load_roots_dir(&missing).unwrap(), MergedConfig::default());
    }

    #[test]
    fn non_toml_files_are_ignored() {
        let dir = tempdir().unwrap();
        write(dir.path(), "00-auto.toml", "[\"/\"]\n");
        write(dir.path(), "README.md", "not a config file");

        let merged = load_roots_dir(dir.path()).unwrap();
        assert_eq!(
            merged.roots,
            vec![ResolvedRoot {
                path: "/".to_string(),
                watches: vec![]
            }]
        );
    }

    #[test]
    fn a_root_with_no_override_falls_back_to_default_watches() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "10-local.toml",
            r#"
                default-watches = ["node_modules", "target"]
                ["/home"]
            "#,
        );

        let merged = load_roots_dir(dir.path()).unwrap();
        assert_eq!(
            merged.roots,
            vec![ResolvedRoot {
                path: "/home".to_string(),
                watches: names(&["node_modules", "target"])
            }]
        );
    }

    #[test]
    fn a_root_s_own_watches_replaces_default_watches_rather_than_unioning() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "10-local.toml",
            r#"
                default-watches = ["node_modules", "target"]
                ["/home/dracula/subvolumize-home"]
                watches = ["dist"]
            "#,
        );

        let merged = load_roots_dir(dir.path()).unwrap();
        assert_eq!(
            merged.roots,
            vec![ResolvedRoot {
                path: "/home/dracula/subvolumize-home".to_string(),
                watches: names(&["dist"])
            }]
        );
    }

    #[test]
    fn a_later_file_s_default_watches_replaces_an_earlier_file_s_entirely() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "00-defaults.toml",
            r#"default-watches = ["node_modules", "target"]"#,
        );
        write(
            dir.path(),
            "10-local.toml",
            r#"default-watches = [".venv"]"#,
        );
        write(dir.path(), "20-more.toml", r#"["/home"]"#);

        let merged = load_roots_dir(dir.path()).unwrap();
        assert_eq!(
            merged.roots,
            vec![ResolvedRoot {
                path: "/home".to_string(),
                watches: names(&[".venv"])
            }]
        );
    }

    #[test]
    fn a_later_file_disabling_a_root_drops_it_entirely() {
        let dir = tempdir().unwrap();
        write(dir.path(), "00-auto.toml", r#"["/noisy-mount"]"#);
        write(
            dir.path(),
            "10-local.toml",
            r#"
                ["/noisy-mount"]
                enabled = false
            "#,
        );

        let merged = load_roots_dir(dir.path()).unwrap();
        assert!(merged.roots.is_empty());
    }

    #[test]
    fn disabling_one_root_does_not_cascade_to_a_root_nested_under_it() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "10-local.toml",
            r#"
                default-watches = ["node_modules"]
                ["/"]
                enabled = false
                ["/home"]
            "#,
        );

        let merged = load_roots_dir(dir.path()).unwrap();
        assert_eq!(
            merged.roots,
            vec![ResolvedRoot {
                path: "/home".to_string(),
                watches: names(&["node_modules"])
            }]
        );
    }

    #[test]
    fn a_root_untouched_by_a_later_file_keeps_its_earlier_file_s_fields() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "00-auto.toml",
            r#"
                ["/home"]
                watches = ["node_modules"]
            "#,
        );
        // Mentions a *different* root only - "/home" isn't touched here,
        // so its own watches from 00-auto.toml above must survive.
        write(dir.path(), "10-local.toml", r#"["/dbs"]"#);

        let merged = load_roots_dir(dir.path()).unwrap();
        assert_eq!(
            merged.roots,
            vec![
                ResolvedRoot {
                    path: "/dbs".to_string(),
                    watches: vec![]
                },
                ResolvedRoot {
                    path: "/home".to_string(),
                    watches: names(&["node_modules"])
                },
            ]
        );
    }

    #[test]
    fn all_watched_names_unions_and_dedupes_across_every_resolved_root() {
        let config = MergedConfig {
            roots: vec![
                ResolvedRoot {
                    path: "/".to_string(),
                    watches: names(&["node_modules", "target"]),
                },
                ResolvedRoot {
                    path: "/home".to_string(),
                    watches: names(&["target", "dist"]),
                },
            ],
            ignore: Vec::new(),
        };
        assert_eq!(
            config.all_watched_names(),
            names(&["dist", "node_modules", "target"])
        );
    }

    #[test]
    fn default_ignore_falls_through_when_no_file_mentions_it() {
        let dir = tempdir().unwrap();
        write(dir.path(), "10-local.toml", r#"["/home"]"#);

        let merged = load_roots_dir(dir.path()).unwrap();
        assert!(merged.ignore.is_empty());
    }

    #[test]
    fn default_ignore_is_merged_last_file_wins_same_as_default_watches() {
        let dir = tempdir().unwrap();
        write(
            dir.path(),
            "00-defaults.toml",
            r#"default-ignore = [".git", ".hg"]"#,
        );
        write(dir.path(), "10-local.toml", r#"default-ignore = [".svn"]"#);
        write(dir.path(), "20-more.toml", r#"["/home"]"#);

        let merged = load_roots_dir(dir.path()).unwrap();
        assert_eq!(merged.ignore, vec![".svn".to_string()]);
    }

    #[test]
    fn load_all_joins_the_roots_d_subdirectory() {
        let dir = tempdir().unwrap();
        let roots_d = dir.path().join(filenames::ROOTS_D_DIR);
        fs::create_dir(&roots_d).unwrap();
        write(&roots_d, filenames::AUTO_ROOTS_FILE_NAME, r#"["/"]"#);

        let merged = load_all(dir.path()).unwrap();
        assert_eq!(
            merged.roots,
            vec![ResolvedRoot {
                path: "/".to_string(),
                watches: vec![]
            }]
        );
    }
}
