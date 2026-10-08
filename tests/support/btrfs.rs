//! BTRFS helpers for integration tests (the crate's own `test_support` is
//! private to the binary). Everything lives under the test scratch dir.
#![allow(dead_code)] // shared by several test binaries; each uses a subset

use std::path::{Path, PathBuf};

/// A temp dir on BTRFS: `$GHOSTVOLUMES_TEST_BTRFS_DIR` or the crate's
/// `target/ghostvolumes-test-scratch`.
pub fn scratch() -> tempfile::TempDir {
    let root = std::env::var("GHOSTVOLUMES_TEST_BTRFS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            Path::new(env!("CARGO_MANIFEST_DIR")).join("target/ghostvolumes-test-scratch")
        });
    std::fs::create_dir_all(&root).unwrap();
    tempfile::tempdir_in(root).unwrap()
}

/// `BTRFS_IOC_SUBVOL_CREATE` of `parent/name`.
pub fn create_subvolume(parent: &Path, name: &str) {
    use std::os::fd::AsRawFd;
    #[repr(C)]
    struct Args {
        fd: i64,
        name: [u8; 4088],
    }
    let dir = std::fs::File::open(parent).unwrap();
    let mut args = Args {
        fd: 0,
        name: [0; 4088],
    };
    args.name[..name.len()].copy_from_slice(name.as_bytes());
    let request = ((1u64 << 30) | (4096 << 16) | (0x94 << 8) | 14) as libc::c_ulong;
    assert_eq!(
        unsafe { libc::ioctl(dir.as_raw_fd(), request, &mut args) },
        0
    );
}

/// Clears the read-only flag (`BTRFS_IOC_SUBVOL_SETFLAGS`) of the subvolume
/// at `path`, never following a symlink there. Best effort: for cleanup.
pub fn make_writable(path: &Path) {
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    if !std::fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && m.ino() == 256) {
        return;
    }
    let Ok(dir) = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY)
        .open(path)
    else {
        return;
    };
    let flags: u64 = 0;
    let request = ((1u64 << 30) | (8 << 16) | (0x94 << 8) | 26) as libc::c_ulong;
    unsafe { libc::ioctl(dir.as_raw_fd(), request, &flags) };
}
