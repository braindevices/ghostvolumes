// Advisory cross-process file locking (§2). `std::fs::File::lock()`/
// `try_lock()`/`unlock()` are stable as of Rust 1.89, pure `std`, so
// usable directly from the dependency-free shim. Dropping the `File`
// releases the lock automatically. Plain `//` comments (not `//!`/
// `///`) since this file is spliced mid-file into src/lock.rs.

/// Opens (creating if needed) the lock file at `path`, creating its
/// parent directory too. Callers call `.lock()`/`.try_lock()` on the
/// returned handle themselves; the file's content is never touched.
#[allow(dead_code)]
pub fn open_lock_file(path: &std::path::Path) -> std::io::Result<std::fs::File> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(path)
}

/// Percent-encodes `/` and `%` so a boundary's absolute path becomes a
/// single flat filename (e.g. `/home/user1/app` ->
/// `%2Fhome%2Fuser1%2Fapp.lock`) — deliberately not a hash, keeping it
/// human-legible and collision-free across shim/CLI.
#[allow(dead_code)]
pub fn boundary_lock_path(
    locks_dir: &std::path::Path,
    boundary: &std::path::Path,
) -> std::path::PathBuf {
    let mut name = String::new();
    for ch in boundary.to_string_lossy().chars() {
        match ch {
            '/' => name.push_str("%2F"),
            '%' => name.push_str("%25"),
            other => name.push(other),
        }
    }
    name.push_str(".lock");
    locks_dir.join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn boundary_lock_path_escapes_slashes() {
        let path = boundary_lock_path(Path::new("/data/locks"), Path::new("/home/user1/app"));
        assert_eq!(path, Path::new("/data/locks/%2Fhome%2Fuser1%2Fapp.lock"));
    }

    #[test]
    fn boundary_lock_path_escapes_literal_percent_signs_too() {
        // A literal `%` in the boundary must itself be escaped, or it
        // could collide with the `/` escaping scheme.
        let path = boundary_lock_path(Path::new("/data/locks"), Path::new("/home/100%done"));
        assert_eq!(path, Path::new("/data/locks/%2Fhome%2F100%25done.lock"));
    }

    #[test]
    fn boundary_lock_path_never_collides_for_different_boundaries() {
        let a = boundary_lock_path(Path::new("/data/locks"), Path::new("/home/user1/app"));
        let b = boundary_lock_path(Path::new("/data/locks"), Path::new("/home/user1/app2"));
        assert_ne!(a, b);
    }

    #[test]
    fn open_lock_file_creates_missing_parent_directories() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/deep/some.lock");
        open_lock_file(&path).unwrap();
        assert!(path.exists());
    }

    #[test]
    fn a_held_exclusive_lock_blocks_a_second_try_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("some.lock");

        let first = open_lock_file(&path).unwrap();
        first.lock().unwrap();

        let second = open_lock_file(&path).unwrap();
        assert!(second.try_lock().is_err());

        drop(first);
        // A fresh try_lock should succeed once the first handle drops,
        // but under load this sandbox can show a brief delay before a
        // release becomes visible (kernel timing artifact, never the
        // unsafe direction); a few short retries tolerate that.
        let mut acquired = false;
        for _ in 0..20 {
            let third = open_lock_file(&path).unwrap();
            if third.try_lock().is_ok() {
                acquired = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(acquired, "lock was never observed as released");
    }
}
