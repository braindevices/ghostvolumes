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

/// `_IOW(0x94, nr, size)` — the btrfs ioctl request encoding.
fn btrfs_iow(nr: u64, size: usize) -> libc::c_ulong {
    ((1u64 << 30) | ((size as u64) << 16) | (0x94 << 8) | nr) as libc::c_ulong
}

/// `struct btrfs_ioctl_vol_args_v2` (4096 bytes): fd, transid, flags,
/// a 32-byte union we leave zeroed, then the name.
#[repr(C)]
struct VolArgsV2 {
    fd: i64,
    transid: u64,
    flags: u64,
    unused: [u64; 4],
    name: [u8; 4040],
}

const BTRFS_SUBVOL_RDONLY: u64 = 1 << 1;

fn ioctl_result(rc: libc::c_int) -> std::io::Result<()> {
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

/// Snapshots subvolume `source` as `dest_parent/name` — what Snapper does
/// for `create`, without Snapper (tests run unprivileged on the BTRFS
/// scratch dir). `read_only` mirrors `create --read-only`.
pub fn snapshot(
    source: &std::path::Path,
    dest_parent: &std::path::Path,
    name: &str,
    read_only: bool,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let src = std::fs::File::open(source)?;
    let dst = std::fs::File::open(dest_parent)?;
    let mut args = VolArgsV2 {
        fd: src.as_raw_fd() as i64,
        transid: 0,
        flags: if read_only { BTRFS_SUBVOL_RDONLY } else { 0 },
        unused: [0; 4],
        name: [0; 4040],
    };
    args.name[..name.len()].copy_from_slice(name.as_bytes());
    let request = btrfs_iow(23, std::mem::size_of::<VolArgsV2>()); // BTRFS_IOC_SNAP_CREATE_V2
    ioctl_result(unsafe { libc::ioctl(dst.as_raw_fd(), request, &mut args) })
}

/// Sets or clears a subvolume's read-only flag — `snapper modify
/// --read-only` / `--read-write`, without Snapper.
pub fn set_read_only(subvolume: &std::path::Path, read_only: bool) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;
    let dir = std::fs::File::open(subvolume)?;
    let mut flags: u64 = if read_only { BTRFS_SUBVOL_RDONLY } else { 0 };
    let request = btrfs_iow(26, std::mem::size_of::<u64>()); // BTRFS_IOC_SUBVOL_SETFLAGS
    ioctl_result(unsafe { libc::ioctl(dir.as_raw_fd(), request, &mut flags) })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::MetadataExt;

    #[test]
    fn vol_args_v2_matches_the_kernel_struct_size() {
        assert_eq!(std::mem::size_of::<VolArgsV2>(), 4096);
    }

    #[test]
    fn a_snapshot_shares_content_and_is_independent_of_its_source() {
        let scratch = btrfs_scratch_dir();
        let src = scratch.path().join("src");
        crate::btrfs::create_subvolume(scratch.path(), "src").unwrap();
        std::fs::write(src.join("a"), "one").unwrap();

        snapshot(&src, scratch.path(), "snap", false).unwrap();
        let snap = scratch.path().join("snap");
        assert_eq!(std::fs::metadata(&snap).unwrap().ino(), 256);
        assert_eq!(std::fs::read_to_string(snap.join("a")).unwrap(), "one");

        std::fs::write(snap.join("a"), "changed").unwrap();
        std::fs::remove_file(snap.join("a")).unwrap();
        assert_eq!(std::fs::read_to_string(src.join("a")).unwrap(), "one");
    }

    #[test]
    fn read_only_snapshots_refuse_writes_until_made_writable_again() {
        let scratch = btrfs_scratch_dir();
        let src = scratch.path().join("src");
        crate::btrfs::create_subvolume(scratch.path(), "src").unwrap();
        std::fs::write(src.join("a"), "one").unwrap();

        snapshot(&src, scratch.path(), "ro", true).unwrap();
        let ro = scratch.path().join("ro");
        let err = std::fs::write(ro.join("b"), "x").unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EROFS));

        set_read_only(&ro, false).unwrap();
        std::fs::write(ro.join("b"), "x").unwrap();
        set_read_only(&ro, true).unwrap();
        assert_eq!(
            std::fs::remove_file(ro.join("b"))
                .unwrap_err()
                .raw_os_error(),
            Some(libc::EROFS)
        );
        set_read_only(&ro, false).unwrap(); // let the scratch dir clean up
    }
}
