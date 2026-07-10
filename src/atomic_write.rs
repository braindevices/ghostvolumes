//! Shared atomic-write helper: temp file in the same directory,
//! `rename` over the destination. Used by `reload` (compiled.tsv) and
//! `scan --save` (roots.d/00-auto.toml) — never leaves the destination
//! half-written even if the process is killed mid-write.

use std::path::Path;

pub fn write_atomically(path: &Path, contents: &str) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let tmp_path = dir.join(format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("ghostvolumes.tmp")
    ));
    std::fs::write(&tmp_path, contents)?;
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn writes_file_with_given_contents() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        write_atomically(&path, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn creates_missing_parent_directories() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nested/deep/out.txt");
        write_atomically(&path, "hello").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "hello");
    }

    #[test]
    fn overwrites_existing_file_atomically() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        std::fs::write(&path, "old").unwrap();
        write_atomically(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn does_not_leave_tmp_file_behind() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("out.txt");
        write_atomically(&path, "hello").unwrap();
        let entries: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("out.txt")]);
    }
}
