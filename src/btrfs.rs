//! BTRFS primitives: filesystem-type detection (used to validate
//! configured roots at config-compile time) plus subvolume detection and creation.

use std::ffi::CString;
use std::path::Path;

include!("btrfs_core.rs");

/// `true` iff the filesystem containing `path` is BTRFS, via `statfs`'s
/// filesystem-type magic number. CLI-only, so free to use `libc`.
pub fn is_btrfs(path: &Path) -> anyhow::Result<bool> {
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // `f_type`'s integer type varies by target (c_long vs c_uint) -
    // widen via `i64::from` rather than `as` for portability.
    #[allow(clippy::useless_conversion)]
    Ok(i64::from(stat.f_type) == i64::from(libc::BTRFS_SUPER_MAGIC))
}

/// `true` iff the subvolume at `path` has BTRFS's read-only flag
/// (`BTRFS_IOC_SUBVOL_GETFLAGS`), e.g. a Snapper snapshot not created
/// `--read-write`.
pub fn is_read_only(path: &Path) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    const BTRFS_SUBVOL_RDONLY: u64 = 1 << 1;
    // _IOR(0x94, 25, __u64)
    let request = ((2u64 << 30) | (8 << 16) | (0x94 << 8) | 25) as libc::c_ulong;
    let dir = std::fs::File::open(path)?;
    let mut flags: u64 = 0;
    if unsafe { libc::ioctl(dir.as_raw_fd(), request, &mut flags) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(flags & BTRFS_SUBVOL_RDONLY != 0)
}

#[cfg(test)]
mod is_btrfs_tests {
    use super::*;
    use crate::test_support::btrfs_scratch_dir;
    use tempfile::tempdir;

    #[test]
    fn overlay_tempdir_is_not_btrfs() {
        // /tmp on this sandbox is container overlayfs, not BTRFS.
        let dir = tempdir().unwrap();
        assert!(!is_btrfs(dir.path()).unwrap());
    }

    #[test]
    fn root_scratch_dir_is_really_btrfs() {
        // /root on this sandbox genuinely is BTRFS-backed.
        let dir = btrfs_scratch_dir();
        assert!(is_btrfs(dir.path()).unwrap());
    }

    #[test]
    fn nonexistent_path_errors_rather_than_panicking() {
        assert!(is_btrfs(Path::new("/definitely/does/not/exist")).is_err());
    }
}
