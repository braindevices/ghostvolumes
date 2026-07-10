// Git-tracked gate (§4): never touch a path git already tracks.
// Applied, per the plan, at all three call sites (cd-hook, LD_PRELOAD,
// `convert`) - this module only implements the check itself.
//
// Dependency-free (plain `std` only - just std::path/std::process),
// so this exact file is shared between the main CLI (via `include!`,
// from `src/git.rs`) and the LD_PRELOAD shim (via `mod`, from
// `shim/preload.rs`). Plain `//` comments, not `//!`/`///`: keeps this
// file splice-safe wherever it's included, not just at file start.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Walks up from `path` looking for a `.git` entry (dir for a normal
/// repo, file for a worktree/submodule — either counts). Tolerates
/// "not a repo" by returning `None` rather than an error.
fn find_repo_root(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|ancestor| ancestor.join(".git").exists())
        .map(Path::to_path_buf)
}

/// `true` iff `path` is tracked by git. `false` (not an error) when
/// `path` isn't inside a repo, or the `git` binary can't be found —
/// both are "obviously not git-tracked", not failures.
pub fn is_git_tracked(path: &Path) -> bool {
    is_git_tracked_with_bin("git", path)
}

fn is_git_tracked_with_bin(git_bin: &str, path: &Path) -> bool {
    let Some(repo_root) = find_repo_root(path) else {
        return false;
    };
    let relative = path.strip_prefix(&repo_root).unwrap_or(path);

    let output = Command::new(git_bin)
        .arg("-C")
        .arg(&repo_root)
        .arg("ls-files")
        .arg("--")
        .arg(relative)
        .output();

    match output {
        Ok(output) => output.status.success() && !output.stdout.is_empty(),
        Err(_) => false, // git binary missing, or otherwise unrunnable
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::tempdir;

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "test@example.com")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "test@example.com")
            .status()
            .expect("git must be available to run this test");
        assert!(status.success(), "git {:?} failed", args);
    }

    #[test]
    fn tracked_file_is_detected() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        fs::write(repo.join("src.rs"), "fn main() {}").unwrap();
        git(repo, &["add", "src.rs"]);
        git(repo, &["commit", "-q", "-m", "init"]);

        assert!(is_git_tracked(&repo.join("src.rs")));
    }

    #[test]
    fn untracked_file_in_repo_is_not_tracked() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        fs::write(repo.join("node_modules_marker"), "x").unwrap();
        // never `git add`ed

        assert!(!is_git_tracked(&repo.join("node_modules_marker")));
    }

    #[test]
    fn path_outside_any_repo_is_not_tracked() {
        let dir = tempdir().unwrap();
        assert!(!is_git_tracked(&dir.path().join("whatever")));
    }

    #[test]
    fn nonexistent_untracked_path_is_not_tracked() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        // node_modules doesn't exist yet and was never tracked
        assert!(!is_git_tracked(&repo.join("node_modules")));
    }

    #[test]
    fn nested_directory_repo_root_is_found_by_walking_up() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        fs::create_dir_all(repo.join("crates/app/src")).unwrap();
        fs::write(repo.join("crates/app/src/main.rs"), "fn main() {}").unwrap();
        git(repo, &["add", "crates/app/src/main.rs"]);
        git(repo, &["commit", "-q", "-m", "init"]);

        assert!(is_git_tracked(&repo.join("crates/app/src/main.rs")));
        assert!(!is_git_tracked(&repo.join("crates/app/node_modules")));
    }

    #[test]
    fn missing_git_binary_is_tolerated_as_not_tracked() {
        let dir = tempdir().unwrap();
        let repo = dir.path();
        git(repo, &["init", "-q"]);
        fs::write(repo.join("src.rs"), "fn main() {}").unwrap();
        git(repo, &["add", "src.rs"]);
        git(repo, &["commit", "-q", "-m", "init"]);

        // Even for an actually-tracked file, a nonexistent git binary
        // must resolve to "not tracked", never an error/panic.
        assert!(!is_git_tracked_with_bin(
            "definitely-not-a-real-git-binary-xyz",
            &repo.join("src.rs")
        ));
    }
}
