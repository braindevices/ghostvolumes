// BTRFS subvolume primitives: detection (inode 256, per §3/§5) and
// creation (`BTRFS_IOC_SUBVOL_CREATE`, per §5/§7).
//
// Dependency-free (plain `std`, plus hand-declared `extern "C"` for
// `open`/`close`/`ioctl` — deliberately NOT the `libc` crate, since
// this file is shared with the LD_PRELOAD shim via `mod`, and bare
// `rustc` can't link crates.io crates). The ioctl request number and
// struct layout aren't in `libc` either way (BTRFS-specific, not
// POSIX) - hand-declared here the same way `<linux/btrfs.h>` defines
// them, confirmed correct against a real BTRFS filesystem in this
// sandbox (`/root`) via a standalone capability probe before writing
// this module (see progress notes for Steps 8-9).
//
// Uses `std::io::Result` (not `anyhow::Result`, an external crate the
// shim can't link) - the main crate's `anyhow`-based call sites absorb
// this automatically via `?`'s `From` conversion. `is_btrfs`
// (`statfs`-based filesystem-type check) stays out of this file: it's
// CLI-only (root validation happens at `reload` time, not on the
// shim's hot path — plan §8.0), so it's free to use the `libc` crate
// instead, in `src/btrfs.rs` directly.

use std::os::unix::fs::MetadataExt;

// Edition 2024 requires `unsafe extern` blocks; this syntax is also
// accepted (not required) on the shim's own `--edition 2021`
// compilation, so one spelling works for both contexts.
unsafe extern "C" {
    fn open(path: *const i8, flags: i32, mode: i32) -> i32;
    fn close(fd: i32) -> i32;
    fn ioctl(fd: i32, request: u64, arg: *mut std::ffi::c_void) -> i32;
}

const O_RDONLY: i32 = 0;
const O_DIRECTORY: i32 = 0o200000;

const BTRFS_PATH_NAME_MAX: usize = 4087;
const BTRFS_IOCTL_MAGIC: u64 = 0x94;

#[repr(C)]
struct BtrfsIoctlVolArgs {
    fd: i64,
    name: [u8; BTRFS_PATH_NAME_MAX + 1],
}

/// Computes an `_IOW(type, nr, size)` request number per
/// `asm-generic/ioctl.h` — the same formula the kernel headers use to
/// define `BTRFS_IOC_SUBVOL_CREATE`.
fn iow(ty: u64, nr: u64, size: usize) -> u64 {
    const DIRSHIFT: u64 = 30;
    const TYPESHIFT: u64 = 8;
    const SIZESHIFT: u64 = 16;
    const IOC_WRITE: u64 = 1;
    (IOC_WRITE << DIRSHIFT) | (ty << TYPESHIFT) | nr | ((size as u64) << SIZESHIFT)
}

/// `true` iff `path` is a directory with inode 256 — BTRFS's
/// structural fingerprint for a subvolume/snapshot root (§3, §5).
pub fn is_subvolume(path: &std::path::Path) -> std::io::Result<bool> {
    let meta = std::fs::metadata(path)?;
    Ok(meta.is_dir() && meta.ino() == 256)
}

/// Creates a new subvolume named `name` directly inside `parent`
/// (which must already exist) via `BTRFS_IOC_SUBVOL_CREATE`.
pub fn create_subvolume(parent: &std::path::Path, name: &str) -> std::io::Result<()> {
    if name.len() > BTRFS_PATH_NAME_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("subvolume name too long: {name}"),
        ));
    }
    let parent_c = std::ffi::CString::new(parent.as_os_str().as_encoded_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;
    let parent_fd = unsafe { open(parent_c.as_ptr(), O_RDONLY | O_DIRECTORY, 0) };
    if parent_fd < 0 {
        return Err(std::io::Error::last_os_error());
    }

    let mut args = BtrfsIoctlVolArgs {
        fd: 0,
        name: [0u8; BTRFS_PATH_NAME_MAX + 1],
    };
    args.name[..name.len()].copy_from_slice(name.as_bytes());

    let request = iow(
        BTRFS_IOCTL_MAGIC,
        14,
        std::mem::size_of::<BtrfsIoctlVolArgs>(),
    );
    let rc = unsafe {
        ioctl(
            parent_fd,
            request,
            &mut args as *mut BtrfsIoctlVolArgs as *mut std::ffi::c_void,
        )
    };
    let ioctl_err = std::io::Error::last_os_error();
    unsafe { close(parent_fd) };

    if rc != 0 {
        return Err(ioctl_err);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::btrfs_scratch_dir;

    #[test]
    fn plain_directory_is_not_a_subvolume() {
        let dir = btrfs_scratch_dir();
        let plain = dir.path().join("plain");
        std::fs::create_dir(&plain).unwrap();
        assert!(!is_subvolume(&plain).unwrap());
    }

    #[test]
    fn created_subvolume_has_inode_256_and_is_detected() {
        let dir = btrfs_scratch_dir();
        create_subvolume(dir.path(), "my-subvol").unwrap();
        let subvol_path = dir.path().join("my-subvol");
        assert!(is_subvolume(&subvol_path).unwrap());
    }

    #[test]
    fn creating_subvolume_with_duplicate_name_fails() {
        let dir = btrfs_scratch_dir();
        create_subvolume(dir.path(), "dup").unwrap();
        assert!(create_subvolume(dir.path(), "dup").is_err());
    }

    #[test]
    fn creating_subvolume_under_nonexistent_parent_fails() {
        let dir = btrfs_scratch_dir();
        let missing_parent = dir.path().join("does-not-exist");
        assert!(create_subvolume(&missing_parent, "x").is_err());
    }
}
