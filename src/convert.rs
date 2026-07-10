//! `ghostvolumes convert <path>` (§7): one-time migration of a
//! pre-existing, populated plain directory into a BTRFS subvolume.

use std::path::Path;
use std::process::Command;

use crate::{btrfs, git};

/// Refuses outright if `path` is git-tracked (no override — see §1's
/// scope boundary), creates a new subvolume at a temp sibling path,
/// `cp -a --reflink=always`s the existing contents in (cheap on BTRFS:
/// extent-sharing metadata, not a real copy, though still a full tree
/// walk so cost scales with file count not size), then atomically
/// swaps it into place and removes the old plain directory.
pub fn convert(path: &Path) -> anyhow::Result<()> {
    if git::is_git_tracked(path) {
        anyhow::bail!(
            "{} is git-tracked; refusing to convert (no override — see plan §1)",
            path.display()
        );
    }
    if !path.is_dir() {
        anyhow::bail!("{} is not a directory", path.display());
    }
    if btrfs::is_subvolume(path).unwrap_or(false) {
        anyhow::bail!("{} is already a subvolume", path.display());
    }

    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("{} has no parent directory", path.display()))?;
    let name = path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("{} has no file name", path.display()))?
        .to_string_lossy()
        .into_owned();

    let tmp_name = format!(".{name}.ghostvolumes-convert-tmp");
    let tmp_path = parent.join(&tmp_name);
    if tmp_path.exists() {
        anyhow::bail!(
            "temp path {} already exists; a previous convert may have failed partway — \
             remove it manually and retry",
            tmp_path.display()
        );
    }
    btrfs::create_subvolume(parent, &tmp_name)?;

    let status = Command::new("cp")
        .arg("-a")
        .arg("--reflink=always")
        .arg("--")
        .arg(format!("{}/.", path.display()))
        .arg(&tmp_path)
        .status()?;
    if !status.success() {
        anyhow::bail!(
            "cp -a --reflink=always into {} failed: {status}",
            tmp_path.display()
        );
    }

    // Atomic swap: move the old plain dir out of the way, move the new
    // subvolume into place, then clean up the old dir. `path` is never
    // missing or half-written in between the two renames.
    let backup_name = format!(".{name}.ghostvolumes-convert-old");
    let backup_path = parent.join(&backup_name);
    std::fs::rename(path, &backup_path)?;
    std::fs::rename(&tmp_path, path)?;
    std::fs::remove_dir_all(&backup_path)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::btrfs_scratch_dir;
    use std::os::unix::fs::MetadataExt;
    use std::process::Command;

    fn git_init(repo: &Path) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .arg("init")
            .arg("-q")
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn converts_plain_directory_preserving_contents() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("node_modules");
        std::fs::create_dir_all(target.join("pkg")).unwrap();
        std::fs::write(target.join("pkg/index.js"), b"module.exports = {}").unwrap();
        std::fs::write(target.join("top-level.txt"), b"hello").unwrap();

        convert(&target).unwrap();

        assert!(btrfs::is_subvolume(&target).unwrap());
        assert_eq!(
            std::fs::read(target.join("pkg/index.js")).unwrap(),
            b"module.exports = {}"
        );
        assert_eq!(
            std::fs::read(target.join("top-level.txt")).unwrap(),
            b"hello"
        );
    }

    #[test]
    fn no_leftover_backup_or_tmp_directories_after_success() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("f"), b"x").unwrap();

        convert(&target).unwrap();

        let entries: Vec<_> = std::fs::read_dir(scratch.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("target")]);
    }

    #[test]
    fn empty_directory_converts_fine() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("build");
        std::fs::create_dir_all(&target).unwrap();

        convert(&target).unwrap();
        assert!(btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn refuses_git_tracked_path() {
        let scratch = btrfs_scratch_dir();
        git_init(scratch.path());
        let target = scratch.path().join("vendor");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("f"), b"x").unwrap();
        Command::new("git")
            .arg("-C")
            .arg(scratch.path())
            .arg("add")
            .arg("vendor/f")
            .status()
            .unwrap();

        let err = convert(&target).unwrap_err();
        assert!(err.to_string().contains("git-tracked"));
        assert!(!btrfs::is_subvolume(&target).unwrap());
    }

    #[test]
    fn refuses_nonexistent_path() {
        let scratch = btrfs_scratch_dir();
        let err = convert(&scratch.path().join("does-not-exist")).unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }

    #[test]
    fn refuses_path_that_is_already_a_subvolume() {
        let scratch = btrfs_scratch_dir();
        btrfs::create_subvolume(scratch.path(), "already").unwrap();
        let target = scratch.path().join("already");

        let err = convert(&target).unwrap_err();
        assert!(err.to_string().contains("already a subvolume"));
    }

    #[test]
    fn refuses_plain_file_not_directory() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("not-a-dir");
        std::fs::write(&target, b"x").unwrap();

        let err = convert(&target).unwrap_err();
        assert!(err.to_string().contains("not a directory"));
    }

    #[test]
    fn preserves_permissions_via_cp_a() {
        use std::os::unix::fs::PermissionsExt;
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join(".venv");
        std::fs::create_dir_all(&target).unwrap();
        let script = target.join("run.sh");
        std::fs::write(&script, b"#!/bin/sh\necho hi").unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();

        convert(&target).unwrap();

        let mode = std::fs::metadata(target.join("run.sh")).unwrap().mode();
        assert_eq!(mode & 0o777, 0o755);
    }

    #[test]
    fn converted_subvolume_is_a_real_new_inode_not_the_old_directory() {
        let scratch = btrfs_scratch_dir();
        let target = scratch.path().join("app");
        std::fs::create_dir_all(&target).unwrap();
        let original_ino = std::fs::metadata(&target).unwrap().ino();

        convert(&target).unwrap();

        let new_ino = std::fs::metadata(&target).unwrap().ino();
        assert_ne!(original_ino, new_ino);
        assert_eq!(new_ino, 256);
    }
}
