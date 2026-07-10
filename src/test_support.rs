//! Test-only helpers shared across modules.

use std::path::PathBuf;

/// Where BTRFS-dependent tests create their scratch subvolumes.
/// Override with `GHOSTVOLUMES_TEST_BTRFS_DIR` if your checkout isn't
/// on a BTRFS filesystem but some other mounted location is. Defaults
/// to `<CARGO_MANIFEST_DIR>/target/ghostvolumes-test-scratch` — the
/// project's own build directory, which is BTRFS-backed whenever the
/// checkout itself is (a reasonable default for a project whose whole
/// point is BTRFS subvolumes, and avoids hardcoding any particular
/// machine's layout, e.g. this sandbox's `/root`).
fn btrfs_test_root() -> PathBuf {
    if let Ok(dir) = std::env::var("GHOSTVOLUMES_TEST_BTRFS_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ghostvolumes-test-scratch")
}

/// A tempdir under `btrfs_test_root()` instead of the default `/tmp`
/// (`tempfile::tempdir()`), which is very often a non-BTRFS overlay or
/// tmpfs and would silently make BTRFS-dependent tests exercise the
/// wrong filesystem.
pub fn btrfs_scratch_dir() -> tempfile::TempDir {
    let parent = btrfs_test_root();
    std::fs::create_dir_all(&parent)
        .unwrap_or_else(|e| panic!("create BTRFS test scratch dir {}: {e}", parent.display()));
    tempfile::tempdir_in(&parent)
        .unwrap_or_else(|e| panic!("create scratch tempdir under {}: {e}", parent.display()))
}
