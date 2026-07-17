//! Test-only helpers shared across modules.

use std::path::PathBuf;

/// Where BTRFS-dependent tests create their scratch subvolumes.
/// Override with `GHOSTVOLUMES_TEST_BTRFS_DIR` if the checkout isn't on
/// BTRFS. Defaults to `<CARGO_MANIFEST_DIR>/target/ghostvolumes-test-scratch`.
fn btrfs_test_root() -> PathBuf {
    if let Ok(dir) = std::env::var("GHOSTVOLUMES_TEST_BTRFS_DIR") {
        return PathBuf::from(dir);
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("target")
        .join("ghostvolumes-test-scratch")
}

/// A tempdir under `btrfs_test_root()` instead of the default `/tmp`,
/// which is often a non-BTRFS overlay/tmpfs.
pub fn btrfs_scratch_dir() -> tempfile::TempDir {
    let parent = btrfs_test_root();
    std::fs::create_dir_all(&parent)
        .unwrap_or_else(|e| panic!("create BTRFS test scratch dir {}: {e}", parent.display()));
    tempfile::tempdir_in(&parent)
        .unwrap_or_else(|e| panic!("create scratch tempdir under {}: {e}", parent.display()))
}
