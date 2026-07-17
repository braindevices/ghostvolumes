//! BTRFS primitives: filesystem-type detection (used to validate
//! configured roots at config-compile time, not on the interception hot
//! path), plus subvolume detection/creation shared with the shim.

use std::ffi::CString;
use std::path::Path;

include!("../shim/btrfs_core.rs");

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
