//! BTRFS primitives: filesystem-type detection (used to validate
//! configured roots at config-compile time, not on the interception
//! hot path — plan §8.0), plus subvolume detection/creation (shared
//! with the LD_PRELOAD shim — see `shim/btrfs_core.rs`'s doc comment).

use std::ffi::CString;
use std::path::Path;

include!("../shim/btrfs_core.rs");

/// `true` iff the filesystem containing `path` is BTRFS. Uses
/// `statfs`'s filesystem-type magic number, which reports the type of
/// whatever filesystem actually contains `path` regardless of mount
/// boundaries — no need to know the exact mountpoint. CLI-only (not
/// needed by the shim, since root validation happens at `reload` time,
/// not on the hot path), so it's free to use the `libc` crate.
pub fn is_btrfs(path: &Path) -> anyhow::Result<bool> {
    let c_path = CString::new(path.as_os_str().as_encoded_bytes())?;
    let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statfs(c_path.as_ptr(), &mut stat) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    // `f_type`'s and `BTRFS_SUPER_MAGIC`'s exact integer type both vary
    // by target (e.g. c_long vs c_uint) - widen through `i64::from`
    // rather than `as` for portability across targets, even though
    // it's a same-type (thus "useless") conversion on this one.
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
