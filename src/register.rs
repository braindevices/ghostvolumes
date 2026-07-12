//! `ghostvolumes register <path>` (ai-work/tasks/decision-model.plan.md
//! §3): appends `path` directly to the project-roots file — no TOML,
//! no `reload` involvement, just a plain append. Gives the
//! decision-file walk-up a narrower stopping boundary than the broader
//! `roots.d` entries alone, for someone who wants that benefit from
//! the very first build, before any decision has ever been recorded.
//! CLI-only (unlike `project_roots_core.rs`, which both the CLI and
//! the shim read) — the shim never writes this file, only the
//! deliberate CLI paths that write to it at all (this command, and
//! `convert`'s own side-effect registration).

use std::io::Write;
use std::path::Path;

pub fn register(list_path: &Path, path: &str) -> anyhow::Result<()> {
    let existing = std::fs::read_to_string(list_path).unwrap_or_default();
    if !crate::project_roots::needs_append(&existing, path) {
        return Ok(());
    }
    if let Some(parent) = list_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(list_path)?;
    writeln!(file, "{path}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn appends_a_new_path() {
        let dir = tempdir().unwrap();
        let list_path = dir.path().join("project-roots.txt");
        register(&list_path, "/home/user1/projects/app").unwrap();
        assert_eq!(
            std::fs::read_to_string(&list_path).unwrap(),
            "/home/user1/projects/app\n"
        );
    }

    #[test]
    fn is_idempotent_for_an_already_registered_path() {
        let dir = tempdir().unwrap();
        let list_path = dir.path().join("project-roots.txt");
        register(&list_path, "/a").unwrap();
        register(&list_path, "/a").unwrap();
        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/a\n");
    }

    #[test]
    fn appends_alongside_existing_entries() {
        let dir = tempdir().unwrap();
        let list_path = dir.path().join("project-roots.txt");
        register(&list_path, "/a").unwrap();
        register(&list_path, "/b").unwrap();
        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/a\n/b\n");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempdir().unwrap();
        let list_path = dir.path().join("nested/deep/project-roots.txt");
        register(&list_path, "/a").unwrap();
        assert_eq!(std::fs::read_to_string(&list_path).unwrap(), "/a\n");
    }
}
