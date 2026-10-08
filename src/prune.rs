//! `ghostvolumes prune <snapshot>`: delete the explicitly decided (`+`)
//! volatile directories inside a writable Snapper snapshot, so
//! long-retention history holds only source. The whole snapshot is the
//! managed set: every decision file in it applies to everything below it
//! (up to the snapshot root), and nothing outside it is read except the
//! `[vcs.*]` config — no project list, no `roots.d`/`compiled.tsv`, no
//! `.ghostvolumes-ignore` (that's `convert`'s).

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};

use crate::vcs::VcsEntry;
use crate::{decision, filenames};

/// `<subvolume>/.snapshots/<n>/snapshot` → `<subvolume>`: Snapper's
/// layout, so the snapshot alone says which live tree it came from.
pub fn subvolume_of(snapshot: &Path) -> anyhow::Result<PathBuf> {
    let n = snapshot.parent().filter(|_| snapshot.ends_with("snapshot"));
    let snapshots = n
        .and_then(Path::parent)
        .filter(|p| p.ends_with(".snapshots"));
    let num_ok = n
        .and_then(Path::file_name)
        .and_then(|s| s.to_str())
        .is_some_and(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()));
    match snapshots.and_then(Path::parent) {
        Some(subvolume) if num_ok => Ok(subvolume.to_path_buf()),
        _ => anyhow::bail!(
            "{} isn't a Snapper snapshot (<subvolume>/.snapshots/<n>/snapshot)",
            snapshot.display()
        ),
    }
}

/// `snapshot` must be a real Snapper snapshot: canonically
/// `<subvolume>/.snapshots/<n>/snapshot`, and a subvolume root itself.
/// Returns the subvolume. A live subvolume, any other directory, or a
/// path through a symlink is refused: prune deletes, and must only ever
/// do so in a snapshot.
pub fn checked_snapshot(snapshot: &Path) -> anyhow::Result<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let real = snapshot
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("{}: {e}", snapshot.display()))?;
    if real != snapshot {
        anyhow::bail!(
            "{} resolves to {}: pass the real snapshot path",
            snapshot.display(),
            real.display()
        );
    }
    let subvolume = subvolume_of(snapshot)?;
    if std::fs::symlink_metadata(snapshot)?.ino() != 256 {
        anyhow::bail!("{} isn't a subvolume: not a snapshot", snapshot.display());
    }
    Ok(subvolume)
}

/// Decision files are read many times during a walk (every directory
/// resolves against its ancestors'), so each is read once. A baseline
/// reader reads each one at the same relative path in another snapshot
/// instead (or nothing, with no baseline): the old decisions applied to
/// this tree.
#[derive(Default)]
struct Files {
    cache: RefCell<HashMap<PathBuf, Option<String>>>,
    baseline: Option<(PathBuf, Option<PathBuf>)>,
}

impl Files {
    fn baseline(snapshot: &Path, baseline: Option<&Path>) -> Self {
        Files {
            cache: RefCell::default(),
            baseline: Some((snapshot.to_path_buf(), baseline.map(Path::to_path_buf))),
        }
    }

    fn read(&self, path: &Path) -> Option<String> {
        self.cache
            .borrow_mut()
            .entry(path.to_path_buf())
            .or_insert_with(|| match &self.baseline {
                None => decision::read_regular_file(path),
                Some((snapshot, baseline)) => read_in_baseline(path, snapshot, baseline.as_deref()),
            })
            .clone()
    }
}

/// `path` (inside `snapshot`) read at the same place in `baseline`. A
/// missing file, or one under a symlinked or non-directory ancestor
/// there, counts as absent: that can only add warnings, never hide one.
fn read_in_baseline(path: &Path, snapshot: &Path, baseline: Option<&Path>) -> Option<String> {
    let baseline = baseline?;
    let file = baseline.join(path.strip_prefix(snapshot).ok()?);
    let dir = file.parent()?;
    if dir != baseline {
        crate::convert::check_contained(dir, baseline).ok()?;
    }
    decision::read_regular_file(&file)
}

/// Every directory in the snapshot whose decision resolves to `+`, in
/// walk order. Not descended into: VCS metadata dirs and a directory
/// already selected (its whole subtree goes). Nested subvolumes appear in
/// a snapshot only as empty placeholders (inode 2), which are skipped; a
/// real subvolume or any other filesystem below the root means this
/// isn't a plain snapshot, and nothing is pruned (an error).
pub fn candidates(
    snapshot: &Path,
    vcs: &BTreeMap<String, VcsEntry>,
) -> anyhow::Result<Vec<PathBuf>> {
    survey(snapshot, vcs, &Files::default())
}

/// The paths this snapshot's decisions prune that `baseline`'s decision
/// files (an earlier snapshot's, only read) wouldn't have pruned in this
/// same tree. Both walks see the same directories, so new build output
/// never shows up here; only a decision change can: a new `+`, a removed
/// `-`, reordered lines, a removed override, a new repo with decision
/// files. `None`: no baseline, so everything is new.
pub fn newly_pruned(
    snapshot: &Path,
    baseline: Option<&Path>,
    now: &[PathBuf],
    vcs: &BTreeMap<String, VcsEntry>,
) -> anyhow::Result<Vec<PathBuf>> {
    let before = survey(snapshot, vcs, &Files::baseline(snapshot, baseline))?;
    Ok(now
        .iter()
        .filter(|path| !before.iter().any(|old| path.starts_with(old)))
        .cloned()
        .collect())
}

fn survey(
    snapshot: &Path,
    vcs: &BTreeMap<String, VcsEntry>,
    files: &Files,
) -> anyhow::Result<Vec<PathBuf>> {
    use std::os::unix::fs::MetadataExt;
    let vcs_dirs: Vec<&str> = vcs.values().map(|e| e.dir.as_str()).collect();
    let dev = std::fs::symlink_metadata(snapshot)?.dev();
    let mut out = Vec::new();
    walk(snapshot, snapshot, dev, &vcs_dirs, files, &mut out)?;
    Ok(out)
}

/// An empty inode-2 directory: how BTRFS shows a nested subvolume inside
/// a snapshot (it also reports its own device number).
fn placeholder(path: &Path, meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    meta.ino() == 2 && std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_none())
}

fn walk(
    snapshot: &Path,
    dir: &Path,
    dev: u64,
    vcs_dirs: &[&str],
    files: &Files,
    out: &mut Vec<PathBuf>,
) -> anyhow::Result<()> {
    use std::os::unix::fs::MetadataExt;
    let Ok(entries) = std::fs::read_dir(dir) else {
        eprintln!("warning: skipping unreadable {}", dir.display());
        return Ok(());
    };
    for entry in entries.flatten() {
        let path = entry.path();
        // `DirEntry::file_type`/`metadata` never follow a symlink.
        if !entry.file_type().is_ok_and(|t| t.is_dir())
            || entry
                .file_name()
                .to_str()
                .is_some_and(|n| vcs_dirs.contains(&n))
        {
            continue;
        }
        let meta = entry.metadata()?;
        if placeholder(&path, &meta) {
            continue;
        }
        if meta.dev() != dev {
            anyhow::bail!(
                "{} is another subvolume or filesystem: {} isn't a plain snapshot",
                path.display(),
                snapshot.display()
            );
        }
        let decided = decision::resolve(&path, snapshot, filenames::DECISION_FILE_NAME, |p| {
            files.read(p)
        });
        if decided == Some(true) {
            out.push(path);
        } else {
            walk(snapshot, &path, dev, vcs_dirs, files, out)?;
        }
    }
    Ok(())
}

/// `(37 file(s), newest 2026-10-08)` for a directory about to be pruned.
pub fn summary(dir: &Path) -> String {
    let (mut files, mut newest) = (0, None);
    tally(dir, &mut files, &mut newest);
    let newest = newest
        .map(|t| humantime::format_rfc3339_seconds(t).to_string()[..10].to_string())
        .unwrap_or_else(|| "-".into());
    format!("({files} file(s), newest {newest})")
}

/// File count and newest mtime under `dir`, without following symlinks;
/// unreadable parts are skipped (this is only a report).
fn tally(dir: &Path, files: &mut u64, newest: &mut Option<std::time::SystemTime>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let Ok(meta) = entry.metadata() else { continue };
        if meta.is_dir() {
            tally(&entry.path(), files, newest);
        } else {
            *files += 1;
        }
        if let Ok(t) = meta.modified() {
            *newest = (*newest).max(Some(t));
        }
    }
}

/// Appends `<time>\t<snapshot or project>\t<line>` per warning to
/// `<state dir>/events.log`, the file the user reads and clears (the login
/// check reports it while non-empty). Never read back or trimmed here.
pub fn append_events(
    state_dir: &Path,
    snapshot: &Path,
    lines: &[String],
) -> anyhow::Result<PathBuf> {
    use std::io::Write;
    std::fs::create_dir_all(state_dir)?;
    let log = state_dir.join(filenames::EVENTS_LOG_FILE_NAME);
    let time = humantime::format_rfc3339_seconds(std::time::SystemTime::now());
    let snapshot = snapshot.to_string_lossy();
    let text: String = lines
        .iter()
        .map(|line| format!("{time}\t{}\t{line}\n", snapshot.escape_debug()))
        .collect();
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| anyhow::anyhow!("{}: {e}", log.display()))?;
    Ok(log)
}

/// The snapshot is already read-only: nothing can be (or needs to be)
/// pruned. Distinguished so the timer script can tell "a run died after
/// locking it" (keep it) from a real failure (exit 3, not 2).
#[derive(Debug)]
pub struct ReadOnly(pub PathBuf);

impl std::fmt::Display for ReadOnly {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} is read-only; prune needs a writable snapshot",
            self.0.display()
        )
    }
}

impl std::error::Error for ReadOnly {}

/// What happened to one `+` directory.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Pruned { path: PathBuf, bytes: u64 },
    Refused { path: PathBuf, reason: String },
}

/// Deletes every candidate (from `candidates`) that passes the guards
/// (see `check`). A refused or undeletable directory is kept and
/// reported, never fatal: the snapshot just keeps it. Only a read-only
/// snapshot is an error — nothing could be pruned, and the caller must
/// not tag it as done.
pub fn prune(
    snapshot: &Path,
    candidates: &[PathBuf],
    vcs: &BTreeMap<String, VcsEntry>,
    timeout: std::time::Duration,
) -> anyhow::Result<Vec<Outcome>> {
    if crate::btrfs::is_read_only(snapshot)? {
        return Err(ReadOnly(snapshot.to_path_buf()).into());
    }
    let mut outcomes = Vec::new();
    for path in candidates {
        let path = path.clone();
        let outcome = match check(snapshot, &path, vcs, timeout) {
            Err(reason) => Outcome::Refused { path, reason },
            // A partial failure (root-owned files left by a container build)
            // leaves the rest for next time.
            // Last check before deleting: the real path is inside the snapshot.
            Ok(_) if !inside(&path, snapshot) => Outcome::Refused {
                reason: "resolves outside the snapshot".into(),
                path,
            },
            Ok(bytes) => match std::fs::remove_dir_all(&path) {
                Ok(()) => Outcome::Pruned { path, bytes },
                Err(e) => Outcome::Refused {
                    path,
                    reason: format!("delete failed: {e}"),
                },
            },
        };
        outcomes.push(outcome);
    }
    Ok(outcomes)
}

fn inside(path: &Path, snapshot: &Path) -> bool {
    match (path.canonicalize(), snapshot.canonicalize()) {
        (Ok(p), Ok(s)) => p.starts_with(&s) && p != s,
        _ => false,
    }
}

/// The guards, before anything is deleted. Returns the subtree's size,
/// or why it must be kept:
/// 1. strictly inside the snapshot by plain components, no symlinked
///    component, a real directory;
/// 2. no VCS metadata dir/file, no decision file and no other filesystem
///    or subvolume anywhere inside (empty nested-subvolume placeholders
///    are fine: nothing in them);
/// 3. the enclosing repo's `guard` command (git: tracked files) prints
///    nothing for it, and its `allow` command (git: ignored) succeeds —
///    run against the snapshot's own metadata.
fn check(
    snapshot: &Path,
    path: &Path,
    vcs: &BTreeMap<String, VcsEntry>,
    timeout: std::time::Duration,
) -> Result<u64, String> {
    crate::convert::check_contained(path, snapshot).map_err(|e| e.to_string())?;
    use std::os::unix::fs::MetadataExt;
    let vcs_dirs: Vec<&str> = vcs.values().map(|e| e.dir.as_str()).collect();
    let dev = std::fs::symlink_metadata(snapshot)
        .map_err(|e| format!("can't inspect {}: {e}", snapshot.display()))?
        .dev();
    let bytes = scan(path, &vcs_dirs, dev)?;
    if let Some((repo, present)) = crate::vcs::repo_of(path, snapshot, vcs) {
        let rel = path.strip_prefix(&repo).unwrap_or(path);
        for entry in present {
            if entry.guard.is_none() && entry.allow.is_none() {
                continue;
            }
            let Some(meta) = crate::vcs::metadata_inside(&repo, entry, snapshot) else {
                return Err(format!(
                    "{} metadata of {} isn't inside the snapshot (linked worktree?): unsupported",
                    entry.dir,
                    repo.display()
                ));
            };
            if meta.join("objects/info/alternates").exists() || meta.join("commondir").exists() {
                return Err(format!(
                    "{} of {} borrows objects from elsewhere: unsupported",
                    entry.dir,
                    repo.display()
                ));
            }
            if let Some(guard) = &entry.guard {
                match crate::vcs::run(guard, &[rel.as_os_str()], &repo, timeout) {
                    Ok(out) if out.is_empty() => {}
                    Ok(out) => {
                        let first = out.split(|b| *b == 0 || *b == b'\n').next().unwrap_or(&[]);
                        return Err(format!(
                            "guard reports versioned content, e.g. {:?}",
                            String::from_utf8_lossy(first)
                        ));
                    }
                    Err(e) => return Err(format!("guard couldn't run: {e}")),
                }
            }
            if let Some(allow) = &entry.allow
                && let Err(e) = crate::vcs::run(allow, &[rel.as_os_str()], &repo, timeout)
            {
                return Err(format!(
                    "not ignored by {} ({e}); `+` only prunes what the repo ignores",
                    entry.dir
                ));
            }
        }
    }
    Ok(bytes)
}

/// Size of the subtree, or the reason it must be kept (guard 2). Doesn't
/// follow symlinks; an unreadable directory can't be vouched for.
fn scan(dir: &Path, vcs_dirs: &[&str], dev: u64) -> Result<u64, String> {
    use std::os::unix::fs::MetadataExt;
    let inspect = |p: &Path, e: std::io::Error| format!("can't inspect {}: {e}", p.display());
    let mut bytes = 0;
    for entry in std::fs::read_dir(dir).map_err(|e| inspect(dir, e))? {
        let entry = entry.map_err(|e| inspect(dir, e))?;
        let name = entry.file_name();
        let path = entry.path();
        if name.to_str().is_some_and(|n| vcs_dirs.contains(&n)) {
            return Err(format!("contains VCS metadata {}", path.display()));
        }
        if name == filenames::DECISION_FILE_NAME {
            return Err(format!("contains a decision file {}", path.display()));
        }
        let meta = entry.metadata().map_err(|e| inspect(&path, e))?;
        if meta.is_dir() && placeholder(&path, &meta) {
            continue;
        }
        if meta.dev() != dev {
            return Err(format!(
                "contains another subvolume or filesystem {}",
                path.display()
            ));
        }
        bytes += if meta.is_dir() {
            scan(&path, vcs_dirs, dev)?
        } else {
            meta.len()
        };
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{btrfs_scratch_dir, set_read_only, snapshot};

    #[test]
    fn subvolume_of_follows_snappers_layout_only() {
        assert_eq!(
            subvolume_of(Path::new("/home/u/src/.snapshots/42/snapshot")).unwrap(),
            Path::new("/home/u/src")
        );
        for bad in [
            "/home/u/src",
            "/x/.snapshots/abc/snapshot",
            "/x/other/42/snapshot",
            "/x/.snapshots/42/snap",
        ] {
            assert!(subvolume_of(Path::new(bad)).is_err(), "{bad}");
        }
    }

    /// `src` subvolume holding project `p`, snapshotted writable as
    /// `src/.snapshots/1/snapshot` (Snapper's layout).
    struct Fixture {
        _scratch: tempfile::TempDir,
        src: PathBuf,
        snap: PathBuf,
    }

    fn fixture(build: impl Fn(&Path)) -> Fixture {
        let scratch = btrfs_scratch_dir();
        crate::btrfs::create_subvolume(scratch.path(), "src").unwrap();
        let src = scratch.path().join("src");
        build(&src.join("p"));
        std::fs::create_dir_all(src.join(".snapshots/1")).unwrap();
        snapshot(&src, &src.join(".snapshots/1"), "snapshot", false).unwrap();
        let snap = src.join(".snapshots/1/snapshot");
        Fixture {
            _scratch: scratch,
            src,
            snap,
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn planned(f: &Fixture) -> Vec<String> {
        let mut rel: Vec<String> = candidates(&f.snap, &crate::vcs::defaults())
            .unwrap()
            .iter()
            .map(|p| p.strip_prefix(&f.snap).unwrap().display().to_string())
            .collect();
        rel.sort();
        rel
    }

    #[test]
    fn plus_decisions_select_dirs_at_any_depth_without_descending() {
        let f = fixture(|p| {
            write(
                &p.join(".ghostvolumes-decisions"),
                "+ node_modules\n+ /build\n",
            );
            write(&p.join("node_modules/a/node_modules/b/x.js"), "");
            write(&p.join("pkg/web/node_modules/y.js"), "");
            write(&p.join("build/out.o"), "");
            write(&p.join("pkg/build/keep.txt"), ""); // `/build` is anchored
            write(&p.join("src/main.rs"), "");
            write(&p.join("undecided/z"), "");
        });
        assert_eq!(
            planned(&f),
            ["p/build", "p/node_modules", "p/pkg/web/node_modules"]
        );
    }

    #[test]
    fn a_closer_minus_decision_overrides() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ node_modules\n");
            write(
                &p.join("vendored/.ghostvolumes-decisions"),
                "- node_modules\n",
            );
            write(&p.join("node_modules/x"), "");
            write(&p.join("vendored/node_modules/y"), "");
        });
        assert_eq!(planned(&f), ["p/node_modules"]);
    }

    #[test]
    fn vcs_dirs_and_symlinks_are_never_entered() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ refs\n+ cache\n");
            write(&p.join(".git/refs/heads/main"), "x");
            std::fs::create_dir_all(p.join("real/cache")).unwrap();
            std::os::unix::fs::symlink("real", p.join("link")).unwrap();
        });
        // `.git/refs` (VCS dir) and `link/cache` (through a symlink) are
        // never candidates.
        assert_eq!(planned(&f), ["p/real/cache"]);
    }

    #[test]
    fn ignore_files_are_convert_s_not_prune_s_use_minus_to_keep() {
        let f = fixture(|p| {
            write(
                &p.join(".ghostvolumes-decisions"),
                "+ target\n- /vendored/**/target\n",
            );
            write(&p.join(".ghostvolumes-ignore"), "third_party\n");
            write(&p.join("third_party/target/z"), "");
            write(&p.join("vendored/x/target/z"), "");
        });
        assert_eq!(planned(&f), ["p/third_party/target"]);
    }

    #[test]
    fn decisions_come_from_the_snapshot_not_the_live_tree() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ target\n");
            write(&p.join("target/x"), "");
            write(&p.join("src/y"), "");
        });
        // A later live edit must not affect what's pruned from the snapshot.
        write(&f.src.join("p/.ghostvolumes-decisions"), "+ src\n");
        assert_eq!(planned(&f), ["p/target"]);
    }

    #[test]
    fn rules_apply_to_everything_below_their_file_up_to_the_snapshot_root() {
        // No project roots: a decision file above a repo (the user's own
        // `~/src/.ghostvolumes-decisions`) reaches into it, and so does an
        // enclosing repo's; a nested repo's own closer `-` still wins.
        let scratch = btrfs_scratch_dir();
        crate::btrfs::create_subvolume(scratch.path(), "src").unwrap();
        let src = scratch.path().join("src");
        write(&src.join(".ghostvolumes-decisions"), "+ node_modules\n");
        write(&src.join("a/node_modules/x"), "");
        write(&src.join("outer/.ghostvolumes-decisions"), "+ target\n");
        write(&src.join("outer/target/x"), "");
        write(&src.join("outer/inner/target/y"), "");
        write(
            &src.join("outer/kept/.ghostvolumes-decisions"),
            "- target\n",
        );
        write(&src.join("outer/kept/target/z"), "");
        write(&src.join("plain/build/w"), "");
        std::fs::create_dir_all(src.join(".snapshots/1")).unwrap();
        snapshot(&src, &src.join(".snapshots/1"), "snapshot", false).unwrap();
        let snap = src.join(".snapshots/1/snapshot");
        let mut found: Vec<String> = candidates(&snap, &crate::vcs::defaults())
            .unwrap()
            .iter()
            .map(|p| p.strip_prefix(&snap).unwrap().display().to_string())
            .collect();
        found.sort();
        assert_eq!(
            found,
            ["a/node_modules", "outer/inner/target", "outer/target"]
        );
    }

    #[test]
    fn nested_subvolume_placeholders_are_skipped_and_deletable() {
        // In a snapshot a nested subvolume is an empty inode-2 directory
        // (with its own device number): never a reason to refuse.
        let f = fixture(|p| {
            write(
                &p.join(".ghostvolumes-decisions"),
                "+ node_modules\n+ target\n",
            );
            std::fs::create_dir_all(p).unwrap();
            crate::btrfs::create_subvolume(p, "node_modules").unwrap();
            write(&p.join("node_modules/x.js"), "x");
            std::fs::create_dir_all(p.join("target")).unwrap();
            crate::btrfs::create_subvolume(&p.join("target"), "inner").unwrap();
        });
        // `node_modules` is itself a placeholder: skipped, not a candidate.
        assert_eq!(planned(&f), ["p/target"]);
        // `target` holds one: pruned anyway.
        assert_eq!(rel(&f, &run_prune(&f)), ["pruned p/target"]);
    }

    #[test]
    fn another_filesystem_or_live_subvolume_below_the_root_is_not_a_snapshot() {
        let f = fixture(|p| write(&p.join(".ghostvolumes-decisions"), "+ target\n"));
        // A real subvolume made inside the (writable) snapshot afterwards.
        crate::btrfs::create_subvolume(&f.snap.join("p"), "sneaky").unwrap();
        let err = candidates(&f.snap, &crate::vcs::defaults()).unwrap_err();
        assert!(err.to_string().contains("isn't a plain snapshot"), "{err}");
    }

    #[test]
    fn only_a_real_snapshot_path_is_accepted() {
        let f = fixture(|p| write(&p.join(".keep"), ""));
        assert_eq!(checked_snapshot(&f.snap).unwrap(), f.src);
        // The live subvolume itself, a plain dir in the layout, a symlink.
        assert!(checked_snapshot(&f.src).is_err());
        std::fs::create_dir_all(f.src.join(".snapshots/2/snapshot")).unwrap();
        assert!(checked_snapshot(&f.src.join(".snapshots/2/snapshot")).is_err());
        std::fs::create_dir_all(f.src.join(".snapshots/3")).unwrap();
        std::os::unix::fs::symlink(&f.src, f.src.join(".snapshots/3/snapshot")).unwrap();
        assert!(checked_snapshot(&f.src.join(".snapshots/3/snapshot")).is_err());
    }

    fn run_prune(f: &Fixture) -> Vec<Outcome> {
        let vcs = crate::vcs::defaults();
        prune(
            &f.snap,
            &candidates(&f.snap, &vcs).unwrap(),
            &crate::vcs::defaults(),
            std::time::Duration::from_secs(30),
        )
        .unwrap()
    }

    fn rel(f: &Fixture, outcomes: &[Outcome]) -> Vec<String> {
        let mut out: Vec<String> = outcomes
            .iter()
            .map(|o| match o {
                Outcome::Pruned { path, .. } => {
                    format!("pruned {}", path.strip_prefix(&f.snap).unwrap().display())
                }
                Outcome::Refused { path, .. } => {
                    format!("refused {}", path.strip_prefix(&f.snap).unwrap().display())
                }
            })
            .collect();
        out.sort();
        out
    }

    fn git(dir: &Path, args: &[&str]) {
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
            .current_dir(dir)
            .output()
            .unwrap()
            .status
            .success();
        assert!(ok, "git {args:?}");
    }

    #[test]
    fn prune_deletes_in_the_snapshot_only_and_reports_sizes() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ target\n");
            write(&p.join("target/big.o"), "0123456789");
            write(&p.join("src/main.rs"), "fn main() {}");
        });
        let outcomes = run_prune(&f);
        assert_eq!(
            outcomes,
            [Outcome::Pruned {
                path: f.snap.join("p/target"),
                bytes: 10
            }]
        );
        assert!(!f.snap.join("p/target").exists());
        assert!(f.snap.join("p/src/main.rs").exists());
        assert!(f.src.join("p/target/big.o").exists(), "live tree untouched");
    }

    #[test]
    fn dirs_holding_vcs_metadata_or_decision_files_are_refused() {
        let f = fixture(|p| {
            write(
                &p.join(".ghostvolumes-decisions"),
                "+ vendor\n+ deps\n+ tools\n",
            );
            write(&p.join("vendor/lib/.git/HEAD"), "x");
            write(&p.join("deps/x/.hg/store"), "x");
            write(&p.join("tools/.ghostvolumes-decisions"), "- bin\n");
        });
        assert_eq!(
            rel(&f, &run_prune(&f)),
            ["refused p/deps", "refused p/tools", "refused p/vendor"]
        );
        for kept in [
            "vendor/lib/.git/HEAD",
            "deps/x/.hg/store",
            "tools/.ghostvolumes-decisions",
        ] {
            assert!(f.snap.join("p").join(kept).exists(), "{kept}");
        }
    }

    #[test]
    fn the_git_guard_refuses_tracked_content_and_leaves_git_metadata_untouched() {
        let f = fixture(|p| {
            write(
                &p.join(".ghostvolumes-decisions"),
                "+ src\n+ target\n+ notes\n",
            );
            write(&p.join(".gitignore"), "target/\n");
            write(&p.join("src/main.rs"), "fn main() {}");
            write(&p.join("target/out"), "x");
            write(
                &p.join("notes/todo.md"),
                "untracked, not ignored: the user's work",
            );
            git(p, &["init", "-q"]);
            git(p, &["add", "src", ".gitignore", ".ghostvolumes-decisions"]);
            git(p, &["commit", "-qm", "init"]);
        });
        let vcs = crate::vcs::defaults();
        let before = crate::vcs::manifest(&f.snap, &vcs).unwrap();
        let outcomes = run_prune(&f);
        // tracked → refused; untracked but not ignored → refused (a committed
        // `+` can't drop the user's own work); ignored → pruned.
        assert_eq!(
            rel(&f, &outcomes),
            ["pruned p/target", "refused p/notes", "refused p/src"]
        );
        assert!(f.snap.join("p/src/main.rs").exists());
        assert!(f.snap.join("p/notes/todo.md").exists());
        assert_eq!(
            crate::vcs::manifest(&f.snap, &vcs).unwrap(),
            before,
            "guard wrote into .git"
        );
    }

    #[test]
    fn a_symlinked_dir_under_the_snapshot_is_never_walked_or_deleted_through() {
        // QR-1/SR-1: `a` is a symlink (absolute, as an untrusted repo could
        // commit) to a live directory outside the snapshot.
        let f = fixture(|p| write(&p.join(".ghostvolumes-decisions"), "+ a\n+ target\n"));
        let outside = f.src.parent().unwrap().join("outside");
        write(&outside.join("a/p/.ghostvolumes-decisions"), "+ target\n");
        write(&outside.join("a/p/target/precious"), "live data");
        std::os::unix::fs::symlink(outside.join("a"), f.snap.join("a")).unwrap();
        std::os::unix::fs::symlink(outside.join("a"), f.snap.join("p/a")).unwrap();

        assert!(planned(&f).is_empty(), "{:?}", planned(&f));
        assert!(run_prune(&f).is_empty());
        assert!(outside.join("a/p/target/precious").exists());
    }

    #[test]
    fn every_vcs_present_at_a_repo_is_guarded() {
        // QR-2: `.dvc` sorts before `.git`, but git's guard still applies.
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ data\n");
            write(&p.join("data/tracked.csv"), "a,b");
            write(&p.join(".dvc/config"), "");
            git(p, &["init", "-q"]);
            git(p, &["add", "data"]);
            git(p, &["commit", "-qm", "init"]);
        });
        assert_eq!(rel(&f, &run_prune(&f)), ["refused p/data"]);
    }

    #[test]
    fn pathspec_magic_in_a_dir_name_is_taken_literally() {
        // SR-3: `:(attr:zz)x` would be a pathspec matching nothing.
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ /:(attr:zz)x\n");
            write(&p.join(":(attr:zz)x/tracked"), "x");
            git(p, &["init", "-q"]);
            git(p, &["--literal-pathspecs", "add", ":(attr:zz)x"]);
            git(p, &["commit", "-qm", "init"]);
        });
        assert_eq!(rel(&f, &run_prune(&f)), ["refused p/:(attr:zz)x"]);
    }

    #[test]
    fn a_linked_worktree_pointing_at_the_live_repo_is_refused() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ target\n");
            write(&p.join("target/out"), "x");
            write(
                &p.join(".git"),
                "gitdir: /somewhere/live/.git/worktrees/p\n",
            );
        });
        let outcomes = run_prune(&f);
        assert!(
            matches!(&outcomes[..], [Outcome::Refused { reason, .. }] if reason.contains("unsupported")),
            "{outcomes:?}"
        );
        assert!(f.snap.join("p/target/out").exists());
    }

    #[test]
    fn a_live_subvolume_inside_a_candidate_is_refused_and_a_later_run_finishes_it() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ target\n");
            write(&p.join("target/a"), "x");
        });
        // A real subvolume created inside the writable snapshot after the
        // fact (not a placeholder): never deleted across.
        crate::btrfs::create_subvolume(&f.snap.join("p/target"), "locked").unwrap();
        write(&f.snap.join("p/target/locked/f"), "x");
        // The walk doesn't enter `target` (a candidate), so the guard sees it.
        let outcomes = run_prune(&f);
        assert!(
            matches!(&outcomes[..], [Outcome::Refused { reason, .. }] if reason.contains("another subvolume")),
            "{outcomes:?}"
        );
        assert!(f.snap.join("p/target/locked/f").exists());

        std::fs::remove_dir_all(f.snap.join("p/target/locked")).unwrap();
        assert_eq!(rel(&f, &run_prune(&f)), ["pruned p/target"]);
        assert!(!f.snap.join("p/target").exists());
    }

    #[test]
    fn a_read_only_snapshot_is_an_error_not_a_refusal() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ target\n");
            write(&p.join("target/a"), "x");
        });
        let found = candidates(&f.snap, &crate::vcs::defaults()).unwrap();
        set_read_only(&f.snap, true).unwrap();
        let err = prune(
            &f.snap,
            &found,
            &crate::vcs::defaults(),
            std::time::Duration::from_secs(30),
        )
        .unwrap_err();
        assert!(err.is::<ReadOnly>(), "{err}");
        set_read_only(&f.snap, false).unwrap();
    }

    /// `f.snap` (snapshot 1) as the read-only baseline, and snapshot 2
    /// taken after `change` edits the live tree.
    fn second(f: &Fixture, change: impl Fn(&Path)) -> PathBuf {
        change(&f.src.join("p"));
        std::fs::create_dir_all(f.src.join(".snapshots/2")).unwrap();
        snapshot(&f.src, &f.src.join(".snapshots/2"), "snapshot", false).unwrap();
        set_read_only(&f.snap, true).unwrap();
        f.src.join(".snapshots/2/snapshot")
    }

    fn new_since(_f: &Fixture, snap: &Path, baseline: Option<&Path>) -> Vec<String> {
        let vcs = crate::vcs::defaults();
        let now = candidates(snap, &vcs).unwrap();
        let mut out: Vec<String> = newly_pruned(snap, baseline, &now, &vcs)
            .unwrap()
            .iter()
            .map(|p| p.strip_prefix(snap).unwrap().display().to_string())
            .collect();
        out.sort();
        out
    }

    /// Decisions before and after; returns what's newly pruned.
    fn changed(before: &str, after: &str, tree: &[&str]) -> Vec<String> {
        let tree: Vec<String> = tree.iter().map(|s| s.to_string()).collect();
        let before = before.to_string();
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), &before);
            for dir in &tree {
                write(&p.join(dir).join("f"), "x");
            }
        });
        let after = after.to_string();
        let snap2 = second(&f, |p| write(&p.join(".ghostvolumes-decisions"), &after));
        let out = new_since(&f, &snap2, Some(&f.snap));
        set_read_only(&f.snap, false).unwrap();
        out
    }

    #[test]
    fn every_decision_change_that_widens_pruning_is_reported() {
        let tree = ["notes", "target", "a/target"];
        // A new `+`, an edited one, a removed `-`, reordered lines.
        assert_eq!(
            changed("+ target\n", "+ target\n+ /notes\n", &tree),
            ["p/notes"]
        );
        assert_eq!(changed("+ /target\n", "+ target\n", &tree), ["p/a/target"]);
        assert_eq!(
            changed("+ notes\n- notes\n", "+ notes\n", &tree),
            ["p/notes"]
        );
        assert_eq!(
            changed("+ notes\n- notes\n", "- notes\n+ notes\n", &tree),
            ["p/notes"]
        );
    }

    #[test]
    fn narrowing_or_cosmetic_changes_are_silent() {
        let tree = ["notes", "target"];
        for after in [
            "+ target\n",                       // a removed `+`
            "# a comment\n+ target\n+ notes\n", // comments, order of `+`
            "+ notes\n+ target\n? /maybe\n",    // a pending marker
            "+ notes\n+ target\n- /notes\n",    // a new `-`
        ] {
            let out = changed("+ notes\n+ target\n", after, &tree);
            assert!(out.is_empty(), "{after:?}: {out:?}");
        }
    }

    #[test]
    fn a_removed_nested_override_is_reported() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ cache\n");
            write(&p.join("sub/.ghostvolumes-decisions"), "- cache\n");
            write(&p.join("sub/cache/x"), "x");
        });
        let snap2 = second(&f, |p| {
            std::fs::remove_file(p.join("sub/.ghostvolumes-decisions")).unwrap()
        });
        assert_eq!(new_since(&f, &snap2, Some(&f.snap)), ["p/sub/cache"]);
        set_read_only(&f.snap, false).unwrap();
    }

    #[test]
    fn new_build_output_under_unchanged_decisions_is_silent() {
        let f = fixture(|p| write(&p.join(".ghostvolumes-decisions"), "+ node_modules\n"));
        let snap2 = second(&f, |p| write(&p.join("web/node_modules/x/y.js"), ""));
        assert!(new_since(&f, &snap2, Some(&f.snap)).is_empty());
        set_read_only(&f.snap, false).unwrap();
    }

    #[test]
    fn no_baseline_reports_everything_pruned() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ target\n");
            write(&p.join("target/x"), "x");
        });
        assert_eq!(new_since(&f, &f.snap, None), ["p/target"]);
    }

    #[test]
    fn a_baseline_file_behind_a_symlink_counts_as_absent() {
        // Snapshot 1 has `sub` as a symlink to a dir whose decisions
        // already say `+ notes`; that must not hide snapshot 2's change.
        let f = fixture(|p| {
            write(&p.join("other/.ghostvolumes-decisions"), "+ notes\n");
            std::os::unix::fs::symlink("other", p.join("sub")).unwrap();
        });
        let snap2 = second(&f, |p| {
            std::fs::remove_file(p.join("sub")).unwrap();
            write(&p.join("sub/.ghostvolumes-decisions"), "+ notes\n");
            write(&p.join("sub/notes/n.md"), "mine");
        });
        assert_eq!(new_since(&f, &snap2, Some(&f.snap)), ["p/sub/notes"]);
        set_read_only(&f.snap, false).unwrap();
    }

    #[test]
    fn the_baseline_is_only_read() {
        let f = fixture(|p| {
            write(&p.join(".ghostvolumes-decisions"), "+ target\n");
            write(&p.join("target/x"), "x");
        });
        let before = crate::vcs::manifest(&f.snap, &crate::vcs::defaults()).unwrap();
        let snap2 = second(&f, |_| {});
        assert!(new_since(&f, &snap2, Some(&f.snap)).is_empty());
        assert!(f.snap.join("p/target/x").exists());
        assert_eq!(
            crate::vcs::manifest(&f.snap, &crate::vcs::defaults()).unwrap(),
            before
        );
        set_read_only(&f.snap, false).unwrap();
    }

    #[test]
    fn summary_counts_files_and_the_newest_date() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("a.md"), "a");
        write(&dir.path().join("deep/b.md"), "b");
        let today = humantime::format_rfc3339_seconds(std::time::SystemTime::now()).to_string();
        assert_eq!(
            summary(dir.path()),
            format!("(2 file(s), newest {})", &today[..10])
        );
        assert_eq!(
            summary(&dir.path().join("missing")),
            "(0 file(s), newest -)"
        );
    }

    #[test]
    fn events_are_appended_never_truncated() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("state/ghostvolumes");
        let snap = Path::new("/s/.snapshots/7/snapshot");
        let log = append_events(&state, snap, &["one".into(), "two".into()]).unwrap();
        append_events(&state, snap, &["three".into()]).unwrap();
        let text = std::fs::read_to_string(&log).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines[0].ends_with("\t/s/.snapshots/7/snapshot\tone"),
            "{}",
            lines[0]
        );
        assert!(lines[2].ends_with("\tthree"));
    }
}
