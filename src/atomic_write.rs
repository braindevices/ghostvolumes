//! Shared atomic-write helper: temp file in the same directory, `rename`
//! over the destination — never leaves the destination half-written.
//!
//! The temp filename includes the PID and a per-process counter: two
//! concurrent writers must never share one temp path, or one's write
//! could corrupt the other's temp file before either renames.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A temp path in `dir` no other call can produce at the same instant:
/// PID plus a monotonic per-process counter.
fn unique_tmp_path(dir: &Path, file_name: &str) -> PathBuf {
    let pid = std::process::id();
    let counter = TMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    dir.join(format!(".{file_name}.{pid}.{counter}.tmp"))
}

pub fn write_atomically(path: &Path, contents: &str) -> anyhow::Result<()> {
    let dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("path has no parent directory"))?;
    std::fs::create_dir_all(dir)?;
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("ghostvolumes.tmp");
    let tmp_path = unique_tmp_path(dir, file_name);
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

    #[test]
    fn unique_tmp_path_never_repeats_within_the_same_process() {
        let dir = Path::new("/tmp");
        let a = unique_tmp_path(dir, "out.txt");
        let b = unique_tmp_path(dir, "out.txt");
        assert_ne!(a, b, "two calls must never produce the same temp path");
    }

    #[test]
    fn concurrent_writes_to_the_same_destination_never_corrupt_it() {
        // Each writer's temp path is isolated, so final content must
        // always be exactly one writer's complete content, never a mix.
        let dir = tempdir().unwrap();
        let path = std::sync::Arc::new(dir.path().join("shared.txt"));
        let contents: Vec<String> = (0..8)
            .map(|i| format!("writer-{i}-{}", "x".repeat(10_000)))
            .collect();

        let handles: Vec<_> = contents
            .iter()
            .map(|content| {
                let path = std::sync::Arc::clone(&path);
                let content = content.clone();
                std::thread::spawn(move || write_atomically(&path, &content).unwrap())
            })
            .collect();
        for h in handles {
            h.join().unwrap();
        }

        let final_content = std::fs::read_to_string(&*path).unwrap();
        assert!(
            contents.contains(&final_content),
            "final content ({} bytes) must exactly match one writer's full content, not a mix or truncation",
            final_content.len()
        );
    }
}
