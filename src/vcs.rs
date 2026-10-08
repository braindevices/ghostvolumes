//! `[vcs.<name>]` settings: which directory marks a VCS's metadata, and
//! optional commands `prune` uses to refuse a path (`guard`) and
//! `vcs-health` uses to check a repo (`health`). Shipped defaults cover
//! git, hg, svn, jj and dvc; only git's commands ship, since they're the
//! ones verified to leave the metadata untouched and to work on a
//! read-only snapshot. A user entry in `vcs.toml` replaces the default of
//! the same name as a whole.

use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VcsEntry {
    /// The metadata directory's name (`.git`); a file of that name counts too.
    pub dir: String,
    /// argv prefix; the candidate path is appended. Any output → refused.
    pub guard: Option<Vec<String>>,
    /// argv prefix; the candidate path is appended. Must exit 0, or the
    /// path is refused (git: `check-ignore` — a `+` line in a repo only
    /// prunes what the repo also ignores, so committed decisions can't
    /// drop a user's untracked work from history).
    pub allow: Option<Vec<String>>,
    /// argv run in the repo root of a read-only snapshot; non-zero → failed.
    pub health: Option<Vec<String>>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct VcsFile {
    #[serde(default)]
    vcs: BTreeMap<String, VcsEntry>,
}

fn argv(words: &[&str]) -> Option<Vec<String>> {
    Some(words.iter().map(|w| w.to_string()).collect())
}

pub fn defaults() -> BTreeMap<String, VcsEntry> {
    let dir_only = |dir: &str| VcsEntry {
        dir: dir.into(),
        guard: None,
        allow: None,
        health: None,
    };
    // No hooks, no fsmonitor, no lazy fetch, no implicit bare repos: run
    // nothing a repo's own config could bring along.
    let git = |cmd: &[&str]| {
        let mut words = vec![
            "git",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.hooksPath=/dev/null",
            "-c",
            "safe.bareRepository=explicit",
        ];
        words.extend_from_slice(cmd);
        argv(&words)
    };
    BTreeMap::from([
        (
            "git".to_string(),
            VcsEntry {
                dir: ".git".into(),
                // `:(exclude)x` or `*` in a directory name is a name, not a
                // pattern. (Only here: `check-ignore` rejects literal mode,
                // and errors — so refuses — on magic-looking names anyway.)
                guard: git(&["--literal-pathspecs", "ls-files", "-z", "--"]),
                allow: git(&["check-ignore", "-q", "--"]),
                health: git(&["fsck", "--connectivity-only", "--no-dangling"]),
            },
        ),
        ("hg".to_string(), dir_only(".hg")),
        ("svn".to_string(), dir_only(".svn")),
        ("jj".to_string(), dir_only(".jj")),
        ("dvc".to_string(), dir_only(".dvc")),
    ])
}

/// A metadata dir name must be one plain component, and a command a
/// non-empty argv: anything else is a config error, not something to guess at.
fn validate(name: &str, entry: &VcsEntry) -> anyhow::Result<()> {
    let dir = &entry.dir;
    if dir.contains('/') || !crate::decision::representable_path(dir) {
        anyhow::bail!("[vcs.{name}] dir {dir:?} must be a single plain directory name");
    }
    for (key, cmd) in [
        ("guard", &entry.guard),
        ("allow", &entry.allow),
        ("health", &entry.health),
    ] {
        if cmd
            .as_ref()
            .is_some_and(|c| c.is_empty() || c[0].is_empty())
        {
            anyhow::bail!("[vcs.{name}] {key} must be a non-empty command");
        }
    }
    Ok(())
}

/// The shipped defaults, with `<config_dir>/vcs.toml` (if present)
/// replacing or adding entries by name.
pub fn load(config_dir: &Path) -> anyhow::Result<BTreeMap<String, VcsEntry>> {
    let mut entries = defaults();
    let path = config_dir.join(crate::filenames::VCS_CONFIG_FILE_NAME);
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let file: VcsFile =
                toml::from_str(&text).map_err(|e| anyhow::anyhow!("{}: {e}", path.display()))?;
            entries.extend(file.vcs);
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(anyhow::anyhow!("{}: {e}", path.display())),
    }
    for (name, entry) in &entries {
        validate(name, entry)?;
    }
    Ok(entries)
}

/// Output cap for `run`: far beyond any real `ls-files`/`snapper list`.
const MAX_OUTPUT: u64 = 16 << 20;

/// Runs `argv` in `cwd` with git's optional locks off (so nothing writes
/// an index refresh into the snapshot) and a fixed locale, and returns its
/// stdout, or an error if it couldn't start, exited non-zero, ran past
/// `timeout` or printed more than `MAX_OUTPUT`.
pub fn run(
    argv: &[String],
    extra: &[&std::ffi::OsStr],
    cwd: &Path,
    timeout: std::time::Duration,
) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    use wait_timeout::ChildExt;
    let mut command = std::process::Command::new(&argv[0]);
    // An inherited GIT_DIR & co. would point git at another (live) repo.
    for var in [
        "GIT_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_COMMON_DIR",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_CEILING_DIRECTORIES",
    ] {
        command.env_remove(var);
    }
    let mut child = command
        .args(&argv[1..])
        .args(extra)
        .current_dir(cwd)
        .env("LC_ALL", "C")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| anyhow::anyhow!("{}: {e}", argv[0]))?;
    // Drain both pipes while waiting: a command flooding either one (a
    // broken repo's fsck on stderr) must not block and look like a timeout.
    // Keeps at most MAX_OUTPUT bytes (+1, to tell "too much"), discarding
    // the rest without ever blocking the command.
    fn drain(
        pipe: impl Read + Send + 'static,
    ) -> std::thread::JoinHandle<std::io::Result<Vec<u8>>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut pipe = pipe;
            (&mut pipe).take(MAX_OUTPUT + 1).read_to_end(&mut buf)?;
            std::io::copy(&mut pipe, &mut std::io::sink())?;
            Ok(buf)
        })
    }
    let reader = drain(child.stdout.take().expect("piped"));
    let err_reader = drain(child.stderr.take().expect("piped"));
    let Some(status) = child.wait_timeout(timeout)? else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(Timeout.into());
    };
    let out = reader.join().expect("reader thread")?;
    if out.len() as u64 > MAX_OUTPUT {
        anyhow::bail!("{}: more than {MAX_OUTPUT} bytes of output", argv[0]);
    }
    let err = err_reader.join().expect("reader thread")?;
    if !status.success() {
        let err = String::from_utf8_lossy(&err);
        // One short line for the journal, however much the command printed.
        let first: String = err
            .lines()
            .next()
            .unwrap_or("")
            .trim()
            .chars()
            .take(300)
            .collect();
        anyhow::bail!("{} exited with {status}: {first}", argv[0]);
    }
    Ok(out)
}

/// `run` ran past its timeout (distinguished: health reports it apart).
#[derive(Debug)]
pub struct Timeout;

impl std::fmt::Display for Timeout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("timed out")
    }
}

impl std::error::Error for Timeout {}

/// The nearest repository containing `path`, searching from its parent
/// up to and including `top`: the directory holding configured VCS dirs
/// (or files), with **every** entry present there — a DVC or hg-git repo
/// is also a git repo, and all their guards apply.
pub fn repo_of<'a>(
    path: &Path,
    top: &Path,
    entries: &'a BTreeMap<String, VcsEntry>,
) -> Option<(std::path::PathBuf, Vec<&'a VcsEntry>)> {
    let mut dir = path.parent()?;
    loop {
        let present: Vec<&VcsEntry> = entries
            .values()
            .filter(|e| dir.join(&e.dir).symlink_metadata().is_ok())
            .collect();
        if !present.is_empty() {
            return Some((dir.to_path_buf(), present));
        }
        if dir == top {
            return None;
        }
        dir = dir.parent()?;
    }
}

/// Where a repo's metadata really lives: the dir itself, or the target
/// of a `gitdir: …` file (submodules, linked worktrees). `None` if that
/// resolves outside `inside` — e.g. a linked worktree pointing into the
/// live repo — where running a VCS command would read the live tree.
pub fn metadata_inside(repo: &Path, entry: &VcsEntry, inside: &Path) -> Option<std::path::PathBuf> {
    let meta = repo.join(&entry.dir);
    let target = if meta.symlink_metadata().ok()?.is_file() {
        let text = std::fs::read_to_string(&meta).ok()?;
        // Like git: only a first line `gitdir: <path>` counts.
        let gitdir = text.lines().next()?.strip_prefix("gitdir:")?.trim();
        repo.join(gitdir)
    } else {
        meta
    };
    let real = target.canonicalize().ok()?;
    real.starts_with(inside.canonicalize().ok()?)
        .then_some(real)
}

/// One repo's health result.
#[derive(Debug, PartialEq)]
pub enum Health {
    Ok,
    Unchanged,
    Failed(String),
    TimedOut,
    Unsupported(String),
}

/// Every repo in `root`: the directory holding a configured VCS dir or
/// file, with its entry. VCS dirs themselves aren't descended into.
fn repos<'a>(
    root: &Path,
    entries: &'a BTreeMap<String, VcsEntry>,
) -> Vec<(std::path::PathBuf, &'a VcsEntry)> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in read.flatten() {
            let name = entry.file_name();
            if let Some(vcs) = entries
                .values()
                .find(|e| name.to_str() == Some(e.dir.as_str()))
            {
                out.push((dir.clone(), vcs));
            } else if entry.file_type().is_ok_and(|t| t.is_dir()) {
                stack.push(entry.path());
            }
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Live paths `snapper status` reported as changed, or `None` if any line
/// doesn't parse — then every repo gets checked. Lines are
/// `<status> <absolute path>`; a newline inside some filename only splits
/// that entry into more lines, which can add checks but never hide the
/// real line for a change under a VCS dir.
///
/// Every path must lie under `subvolume`: snapper prints its own
/// `SUBVOLUME` spelling, and if that differs from ours (a symlinked
/// `/home`) nothing would ever match — so that, too, means "check all".
pub fn parse_snapper_status(text: &str, subvolume: &Path) -> Option<Vec<std::path::PathBuf>> {
    text.lines()
        .map(|line| {
            let (status, path) = line.split_once(' ')?;
            let status_ok = !status.is_empty() && status.chars().all(|c| "+-ctpugxa.".contains(c));
            let path = std::path::PathBuf::from(path);
            (status_ok && path.is_absolute() && path.starts_with(subvolume)).then_some(path)
        })
        .collect()
}

/// Runs each repo's `health` command inside the (read-only) snapshot.
/// With `changed` (live paths from `snapper status prev..n`), a repo
/// whose metadata has no changed path under it is `Unchanged`; without
/// it, every repo is checked.
pub fn health(
    snapshot: &Path,
    subvolume: &Path,
    entries: &BTreeMap<String, VcsEntry>,
    changed: Option<&[std::path::PathBuf]>,
    timeout: std::time::Duration,
) -> Vec<(std::path::PathBuf, Health)> {
    let mut out = Vec::new();
    for (repo, entry) in repos(snapshot, entries) {
        let Some(cmd) = &entry.health else { continue };
        let result = match metadata_inside(&repo, entry, snapshot) {
            None => Health::Unsupported(format!(
                "{} metadata isn't inside the snapshot (linked worktree?)",
                entry.dir
            )),
            // git's alternates (and anything laid out alike): objects that
            // live outside this repo's metadata — documented as unsupported.
            Some(meta) if meta.join("objects/info/alternates").exists() => {
                Health::Unsupported("uses alternates (clone --shared/--reference)".into())
            }
            Some(meta) => {
                let live = subvolume.join(
                    meta.strip_prefix(snapshot.canonicalize().unwrap_or_default())
                        .unwrap_or(&meta),
                );
                let touched =
                    changed.is_none_or(|paths| paths.iter().any(|p| p.starts_with(&live)));
                if !touched {
                    Health::Unchanged
                } else {
                    match run(cmd, &[], &repo, timeout) {
                        Ok(_) => Health::Ok,
                        Err(e) if e.is::<Timeout>() => Health::TimedOut,
                        Err(e) => Health::Failed(e.to_string()),
                    }
                }
            }
        };
        out.push((repo, result));
    }
    out
}

fn walk_err(path: &Path, e: std::io::Error) -> anyhow::Error {
    anyhow::anyhow!("{}: {e}", path.display())
}

/// One manifest line per entry inside a VCS metadata dir found anywhere
/// under `root`: the path relative to `root` (Rust-escaped, so any name
/// stays on one line), type, size, mtime (ns), inode. Sorted by path, so two
/// runs over an unchanged tree print the same bytes and `cmp` can prove
/// nothing in the metadata changed. No VCS knowledge; symlinks are
/// recorded (with their target), never followed.
pub fn manifest(root: &Path, entries: &BTreeMap<String, VcsEntry>) -> anyhow::Result<String> {
    let dirs: Vec<&str> = entries.values().map(|e| e.dir.as_str()).collect();
    let mut lines = Vec::new();
    find_vcs_dirs(root, root, &dirs, &mut lines)?;
    lines.sort();
    Ok(lines.concat())
}

/// Walks the work tree looking for VCS dirs. An unreadable directory
/// outside any VCS dir is skipped with a warning (prune can't touch it
/// either); inside one it's an error, since nothing could be proven.
fn find_vcs_dirs(
    root: &Path,
    dir: &Path,
    dirs: &[&str],
    out: &mut Vec<String>,
) -> anyhow::Result<()> {
    let read = match std::fs::read_dir(dir) {
        Ok(read) => read,
        Err(e) => {
            eprintln!("warning: skipping unreadable {}: {e}", dir.display());
            return Ok(());
        }
    };
    for entry in read {
        let entry = entry.map_err(|e| walk_err(dir, e))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|e| walk_err(&path, e))?;
        if entry
            .file_name()
            .to_str()
            .is_some_and(|n| dirs.contains(&n))
        {
            record(root, &path, out)?;
        } else if file_type.is_dir() {
            find_vcs_dirs(root, &path, dirs, out)?;
        }
    }
    Ok(())
}

/// Records `path` and, for a real directory, everything below it.
fn record(root: &Path, path: &Path, out: &mut Vec<String>) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let meta = std::fs::symlink_metadata(path).map_err(|e| walk_err(path, e))?;
    let kind = if meta.is_dir() {
        "dir".to_string()
    } else if meta.file_type().is_symlink() {
        let target = std::fs::read_link(path).map_err(|e| walk_err(path, e))?;
        format!("link:{target:?}")
    } else if meta.is_file() {
        "file".to_string()
    } else {
        "other".to_string()
    };
    let rel = path.strip_prefix(root).unwrap_or(path);
    out.push(format!(
        "{rel:?}\t{kind}\t{}\t{}.{:09}\t{}\n",
        meta.size(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ino()
    ));
    if meta.is_dir() {
        for entry in std::fs::read_dir(path).map_err(|e| walk_err(path, e))? {
            record(root, &entry.map_err(|e| walk_err(path, e))?.path(), out)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load_text(text: &str) -> anyhow::Result<BTreeMap<String, VcsEntry>> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("vcs.toml"), text).unwrap();
        load(dir.path())
    }

    #[test]
    fn defaults_cover_common_vcs_but_only_git_ships_commands() {
        let entries = load(tempfile::tempdir().unwrap().path()).unwrap();
        let dirs: Vec<&str> = entries.values().map(|e| e.dir.as_str()).collect();
        assert_eq!(dirs, [".dvc", ".git", ".hg", ".jj", ".svn"]);
        assert!(entries["git"].guard.is_some() && entries["git"].health.is_some());
        for name in ["hg", "svn", "jj", "dvc"] {
            assert!(
                entries[name].guard.is_none() && entries[name].health.is_none(),
                "{name}"
            );
        }
    }

    #[test]
    fn a_user_entry_replaces_its_default_and_new_ones_are_added() {
        let entries = load_text(
            "[vcs.git]\ndir = \".git\"\n\n[vcs.pijul]\ndir = \".pijul\"\nhealth = [\"pijul\", \"check\"]\n",
        )
        .unwrap();
        assert_eq!(entries["git"].guard, None, "replaced as a whole");
        assert_eq!(
            entries["pijul"].health.as_deref(),
            Some(&["pijul".to_string(), "check".to_string()][..])
        );
        assert!(entries.contains_key("hg"));
    }

    /// A project with a `.git` dir, a submodule-style `.git` file, and
    /// a work tree, inside a BTRFS subvolume.
    fn repo_tree(base: &Path) -> std::path::PathBuf {
        crate::btrfs::create_subvolume(base, "src").unwrap();
        let src = base.join("src");
        std::fs::create_dir_all(src.join("p/.git/refs/heads")).unwrap();
        std::fs::write(src.join("p/.git/HEAD"), "ref: refs/heads/main\n").unwrap();
        std::fs::write(src.join("p/.git/refs/heads/main"), "abc\n").unwrap();
        std::fs::create_dir_all(src.join("p/sub")).unwrap();
        std::fs::write(src.join("p/sub/.git"), "gitdir: ../.git/modules/sub\n").unwrap();
        std::fs::create_dir_all(src.join("p/node_modules/x")).unwrap();
        std::fs::write(src.join("p/main.rs"), "fn main() {}").unwrap();
        src
    }

    #[test]
    fn manifest_covers_only_vcs_metadata_and_is_deterministic() {
        let scratch = crate::test_support::btrfs_scratch_dir();
        let src = repo_tree(scratch.path());
        let vcs = defaults();
        let m = manifest(&src, &vcs).unwrap();
        assert_eq!(m, manifest(&src, &vcs).unwrap());
        assert!(m.contains("\"p/.git/refs/heads/main\""), "{m}");
        assert!(
            m.contains("file\t") && m.contains("\"p/sub/.git\""),
            "a .git file is metadata too:\n{m}"
        );
        assert!(!m.contains("main.rs") && !m.contains("node_modules"), "{m}");

        // Work-tree changes and deletions don't show up...
        std::fs::write(src.join("p/main.rs"), "changed").unwrap();
        std::fs::remove_dir_all(src.join("p/node_modules")).unwrap();
        assert_eq!(m, manifest(&src, &vcs).unwrap());
    }

    #[test]
    fn manifest_changes_when_metadata_changes() {
        let scratch = crate::test_support::btrfs_scratch_dir();
        let src = repo_tree(scratch.path());
        let vcs = defaults();
        let changes: [&dyn Fn(&Path); 3] = [
            &|s| std::fs::write(s.join("p/.git/HEAD"), "ref: refs/heads/other\n").unwrap(),
            // a ref rewritten via rename (as git does): same content, new inode
            &|s| {
                let main = s.join("p/.git/refs/heads/main");
                std::fs::write(main.with_extension("lock"), "abc\n").unwrap();
                std::fs::rename(main.with_extension("lock"), &main).unwrap();
            },
            &|s| std::fs::remove_dir_all(s.join("p/.git/refs/heads")).unwrap(),
        ];
        for change in changes {
            let before = manifest(&src, &vcs).unwrap();
            change(&src);
            assert_ne!(before, manifest(&src, &vcs).unwrap());
        }
    }

    #[test]
    fn manifest_records_symlinks_without_following_them_and_survives_snapshots() {
        let scratch = crate::test_support::btrfs_scratch_dir();
        let src = repo_tree(scratch.path());
        std::os::unix::fs::symlink("/etc", src.join("p/.git/evil")).unwrap();
        let vcs = defaults();
        let live = manifest(&src, &vcs).unwrap();
        assert!(live.contains("link:\"/etc\""), "{live}");
        assert!(!live.contains("passwd"), "not followed");

        // A snapshot keeps inode numbers, sizes and mtimes: same manifest.
        crate::test_support::snapshot(&src, scratch.path(), "snap", true).unwrap();
        let snap = scratch.path().join("snap");
        assert_eq!(manifest(&snap, &vcs).unwrap(), live);
        crate::test_support::set_read_only(&snap, false).unwrap();
    }

    #[test]
    fn snapper_status_parsing_is_fail_safe() {
        let ok = parse_snapper_status(
            "c...... /src/p/.git/HEAD\n+..... /src/p/a b.txt\n",
            Path::new("/src"),
        )
        .unwrap();
        assert_eq!(
            ok,
            [
                std::path::PathBuf::from("/src/p/.git/HEAD"),
                "/src/p/a b.txt".into()
            ]
        );
        assert_eq!(
            parse_snapper_status("", Path::new("/src")).unwrap(),
            Vec::<std::path::PathBuf>::new()
        );
        // Anything unexpected → None → every repo is checked.
        for bad in ["garbage\n", "c...... relative\n", "Q...... /x\n"] {
            assert_eq!(
                parse_snapper_status(bad, Path::new("/src")),
                None,
                "{bad:?}"
            );
        }
        // Snapper spells the subvolume differently (symlinked /home): check all.
        assert_eq!(
            parse_snapper_status(
                "c...... /var/home/u/src/p/.git/HEAD\n",
                Path::new("/home/u/src")
            ),
            None
        );
        // A filename with an embedded newline splits into a fragment that
        // doesn't parse → check everything, never fewer.
        assert_eq!(
            parse_snapper_status(
                "+..... /src/p/evil\nname\nc...... /src/q/.git/HEAD\n",
                Path::new("/src")
            ),
            None
        );
    }

    /// `src` subvolume with a committed git repo `p`, snapshotted read-only
    /// at `src/.snapshots/1/snapshot`.
    fn repo_snapshot(scratch: &Path) -> (std::path::PathBuf, std::path::PathBuf) {
        crate::btrfs::create_subvolume(scratch, "src").unwrap();
        let src = scratch.join("src");
        let p = src.join("p");
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(p.join("src/main.rs"), "fn main() {}").unwrap();
        for args in [
            &["init", "-q"][..],
            &["add", "."],
            &["commit", "-qm", "init"],
        ] {
            let ok = std::process::Command::new("git")
                .args([
                    "-c",
                    "user.name=t",
                    "-c",
                    "user.email=t@t",
                    "-c",
                    "init.defaultBranch=main",
                ])
                .args(args)
                .current_dir(&p)
                .status()
                .unwrap()
                .success();
            assert!(ok);
        }
        std::fs::create_dir_all(src.join(".snapshots/1")).unwrap();
        crate::test_support::snapshot(&src, &src.join(".snapshots/1"), "snapshot", true).unwrap();
        (src.clone(), src.join(".snapshots/1/snapshot"))
    }

    fn check(
        snap: &Path,
        src: &Path,
        vcs: &BTreeMap<String, VcsEntry>,
        changed: Option<&[std::path::PathBuf]>,
    ) -> Vec<Health> {
        health(snap, src, vcs, changed, std::time::Duration::from_secs(30))
            .into_iter()
            .map(|(_, h)| h)
            .collect()
    }

    #[test]
    fn health_runs_only_for_repos_whose_metadata_changed() {
        let scratch = crate::test_support::btrfs_scratch_dir();
        let (src, snap) = repo_snapshot(scratch.path());
        let vcs = defaults();
        let before = manifest(&snap, &vcs).unwrap();

        assert_eq!(
            check(&snap, &src, &vcs, None),
            [Health::Ok],
            "no change list → checked"
        );
        assert_eq!(check(&snap, &src, &vcs, Some(&[])), [Health::Unchanged]);
        let worktree_only = [src.join("p/src/main.rs")];
        assert_eq!(
            check(&snap, &src, &vcs, Some(&worktree_only)),
            [Health::Unchanged]
        );
        let ref_changed = [src.join("p/.git/refs/heads/main")];
        assert_eq!(check(&snap, &src, &vcs, Some(&ref_changed)), [Health::Ok]);

        assert_eq!(
            manifest(&snap, &vcs).unwrap(),
            before,
            "health wrote into the snapshot"
        );
        crate::test_support::set_read_only(&snap, false).unwrap();
    }

    #[test]
    fn a_missing_object_fails_and_a_slow_check_times_out() {
        let scratch = crate::test_support::btrfs_scratch_dir();
        let (src, snap) = repo_snapshot(scratch.path());
        crate::test_support::set_read_only(&snap, false).unwrap();
        let head = std::fs::read_to_string(snap.join("p/.git/refs/heads/main")).unwrap();
        let (dir, file) = head.trim().split_at(2);
        std::fs::remove_file(snap.join("p/.git/objects").join(dir).join(file)).unwrap();
        crate::test_support::set_read_only(&snap, true).unwrap();
        assert!(matches!(
            &check(&snap, &src, &defaults(), None)[..],
            [Health::Failed(_)]
        ));

        let mut slow = defaults();
        slow.get_mut("git").unwrap().health = Some(vec!["sleep".into(), "5".into()]);
        let timed = health(
            &snap,
            &src,
            &slow,
            None,
            std::time::Duration::from_millis(200),
        );
        assert!(matches!(&timed[..], [(_, Health::TimedOut)]), "{timed:?}");
        crate::test_support::set_read_only(&snap, false).unwrap();
    }

    #[test]
    fn alternates_and_out_of_snapshot_metadata_are_unsupported() {
        let scratch = crate::test_support::btrfs_scratch_dir();
        let (src, snap) = repo_snapshot(scratch.path());
        crate::test_support::set_read_only(&snap, false).unwrap();
        std::fs::write(
            snap.join("p/.git/objects/info/alternates"),
            "/elsewhere/objects\n",
        )
        .unwrap();
        std::fs::create_dir_all(snap.join("wt")).unwrap();
        std::fs::write(
            snap.join("wt/.git"),
            "gitdir: /live/repo/.git/worktrees/wt\n",
        )
        .unwrap();
        let results = check(&snap, &src, &defaults(), None);
        assert!(
            results.iter().all(|h| matches!(h, Health::Unsupported(_))),
            "{results:?}"
        );
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn a_command_flooding_stderr_fails_promptly_instead_of_timing_out() {
        let flood = vec![
            "sh".to_string(),
            "-c".into(),
            "head -c 300000 /dev/zero >&2; exit 3".into(),
        ];
        let started = std::time::Instant::now();
        let err = run(
            &flood,
            &[],
            Path::new("/"),
            std::time::Duration::from_secs(5),
        )
        .unwrap_err();
        assert!(!err.is::<Timeout>(), "{err}");
        assert!(started.elapsed() < std::time::Duration::from_secs(4));
    }

    #[test]
    fn invalid_entries_are_errors() {
        for bad in [
            "[vcs.x]\ndir = \"a/b\"\n",
            "[vcs.x]\ndir = \"\"\n",
            "[vcs.x]\ndir = \"..\"\n",
            "[vcs.x]\ndir = \".x\"\nguard = []\n",
            "[vcs.x]\ndir = \".x\"\nhealth = [\"\"]\n",
            "[vcs.x]\ndir = \".x\"\ntypo = 1\n",
            "[other]\n",
        ] {
            assert!(load_text(bad).is_err(), "{bad:?}");
        }
    }
}
