// BTRFS subvolume primitives: detection (inode 256, per §3/§5) and
// creation (`BTRFS_IOC_SUBVOL_CREATE`, per §5/§7).
//
// Dependency-free (plain `std`, plus a hand-declared `extern "C"` for
// `ioctl`, not `libc`, since bare `rustc` can't link crates.io crates). Ioctl request number and struct layout match
// `<linux/btrfs.h>`, verified against a real BTRFS filesystem.
//
// Uses `std::io::Result`, not `anyhow::Result`; `is_btrfs` stays
// CLI-only in `src/btrfs.rs` and is free to use `libc` instead.

use std::os::fd::AsRawFd;
use std::os::unix::fs::MetadataExt;

// Edition 2024 requires `unsafe extern`.
unsafe extern "C" {
    // Variadic like libc's; rustc rejects mismatched runtime symbols.
    fn ioctl(fd: std::ffi::c_int, request: std::ffi::c_ulong, ...) -> std::ffi::c_int;
}

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
#[allow(dead_code)]
pub fn create_subvolume(parent: &std::path::Path, name: &str) -> std::io::Result<()> {
    // Stands in for O_DIRECTORY: a read-only open of a FIFO parent would
    // block the host process. `std` opens with O_CLOEXEC and closes on
    // drop; no hand-declared `open` (rustc rejects one that isn't
    // variadic like libc's).
    if !std::fs::metadata(parent)?.is_dir() {
        return Err(std::io::ErrorKind::NotADirectory.into());
    }
    create_subvolume_in(&std::fs::File::open(parent)?, name)
}

/// `create_subvolume` on an already-open parent directory, so the
/// subvolume lands in exactly that directory even if a path component is
/// swapped in between.
pub fn create_subvolume_in(parent_dir: &std::fs::File, name: &str) -> std::io::Result<()> {
    if name.len() > BTRFS_PATH_NAME_MAX {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("subvolume name too long: {name}"),
        ));
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
            parent_dir.as_raw_fd(),
            request as std::ffi::c_ulong,
            &mut args as *mut BtrfsIoctlVolArgs as *mut std::ffi::c_void,
        )
    };
    if rc != 0 {
        return Err(std::io::Error::last_os_error());
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

    #[test]
    fn fifo_parent_fails_fast_instead_of_blocking() {
        let dir = btrfs_scratch_dir();
        let fifo = dir.path().join("fifo");
        let fifo_c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(fifo_c.as_ptr(), 0o600) }, 0);
        // A regression would block in open(); fail via timeout, not a hung test run.
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || tx.send(create_subvolume(&fifo, "x")).unwrap());
        let err = rx
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("create_subvolume blocked on a FIFO parent")
            .unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::NotADirectory);
    }
}
