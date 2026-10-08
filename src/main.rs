// BTRFS ioctls, LD_PRELOAD, and /proc/self/mountinfo are Linux-specific;
// gate the whole implementation rather than fail to compile confusingly
// elsewhere.
#[cfg(target_os = "linux")]
mod atomic_write;
#[cfg(target_os = "linux")]
mod btrfs;
#[cfg(target_os = "linux")]
mod cache;
#[cfg(target_os = "linux")]
mod completions;
#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
mod convert;
#[cfg(target_os = "linux")]
mod debug;
#[cfg(target_os = "linux")]
mod decision;
#[cfg(target_os = "linux")]
mod discover;
#[cfg(target_os = "linux")]
mod filenames;
#[cfg(target_os = "linux")]
mod init;
#[cfg(target_os = "linux")]
mod lock;
#[cfg(target_os = "linux")]
mod merge;
#[cfg(target_os = "linux")]
mod mountinfo;
#[cfg(target_os = "linux")]
mod project_roots;
#[cfg(target_os = "linux")]
mod projects;
#[cfg(target_os = "linux")]
mod prune;
#[cfg(target_os = "linux")]
mod reload;
#[cfg(target_os = "linux")]
mod roots;
#[cfg(target_os = "linux")]
mod scan;
#[cfg(target_os = "linux")]
mod snapper;
#[cfg(all(target_os = "linux", test))]
mod test_support;
#[cfg(target_os = "linux")]
mod vcs;
#[cfg(target_os = "linux")]
mod xdg;

#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use clap::{CommandFactory, Parser, Subcommand};
#[cfg(target_os = "linux")]
use clap_complete::engine::ArgValueCompleter;

/// `CARGO_PKG_VERSION` is trusted verbatim on every branch - real
/// releases only ever bump it on `main`, in lockstep with the release
/// tag (`release.toml`/`.github/workflows/release.yml`), so there's no
/// drift for a branch-conditional scheme to correct for. The
/// parenthesized part is purely informational debug/bug-report
/// metadata, not part of the version number: `VERGEN_GIT_DESCRIBE`
/// (via `build.rs`) pins down exactly which commit was built, and
/// `VERGEN_GIT_BRANCH` says which branch it was built from.
#[cfg(target_os = "linux")]
const VERSION: &str = concat!(
    env!("CARGO_PKG_VERSION"),
    " (",
    env!("VERGEN_GIT_DESCRIBE"),
    ", ",
    env!("VERGEN_GIT_BRANCH"),
    ")"
);

#[cfg(target_os = "linux")]
#[derive(Parser)]
#[command(
    name = "ghostvolumes",
    version = VERSION,
    about = "Isolate volatile build artifacts into unsnapshotted BTRFS subvolumes"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(target_os = "linux")]
#[derive(Subcommand)]
enum Command {
    /// Manage roots.d: detect BTRFS roots, list the effective config
    Roots {
        #[command(subcommand)]
        action: RootsAction,
    },
    /// Rebuild the compiled runtime cache from the TOML config
    Reload,
    /// Write default config; after an upgrade, recompile caches and remove
    /// the retired LD_PRELOAD shim
    Init,
    /// Survey for undecided directories and suggest `decide` commands to run
    Discover {
        /// An arbitrary directory to survey, not necessarily a
        /// registered project - plain filesystem completion, not the
        /// registered-projects list `convert`/`decide` use
        #[arg(value_hint = clap::ValueHint::DirPath)]
        path: Option<String>,
        /// How many directory levels deep to scan (bounded by default,
        /// unlike convert/decide, since `path` is an arbitrary,
        /// unregistered starting point)
        #[arg(long, default_value_t = 3)]
        max_depth: u32,
        /// Let a suggestion nested under `path` fold into it, instead
        /// of treating `path` as a non-project container
        #[arg(long)]
        root_is_project: bool,
        /// A known-not-a-project container path (repeatable); never a
        /// merge target, same as `path` unless --root-is-project is set
        #[arg(long = "no-project")]
        no_project: Vec<String>,
        /// A path (repeatable) to never scan at all - no report, no
        /// descent - unlike --no-project, which still reports its own
        /// finding
        #[arg(long = "ignore")]
        ignore: Vec<String>,
    },
    /// Recursively find and resolve subvolume candidates under a project
    Convert {
        /// The project: a decision-file/project-roots boundary, never
        /// itself converted
        #[arg(add = ArgValueCompleter::new(completions::registered_projects))]
        path: String,
        #[arg(long)]
        max_depth: Option<u32>,
        /// Explicit target (relative to path) to resolve directly,
        /// bypassing the watched-name check - repeatable
        #[arg(long = "create")]
        create: Vec<String>,
        /// Print what would happen without changing anything - never
        /// prompts, never touches the filesystem, the decision file,
        /// or the project-roots list
        #[arg(long)]
        dry_run: bool,
        /// Delete the `.<name>.ghostvolumes-convert-old.<time>` backup after a
        /// successful swap instead of keeping it (config:
        /// `delete-convert-backup = true`)
        #[arg(long)]
        delete_backup: bool,
    },
    /// Print a bundled helper (the snapshot script, systemd units, login
    /// check) — `cargo install` only installs the binary. No name lists them
    Contrib { name: Option<String> },
    /// Print a manifest of every VCS metadata dir under a path (for
    /// comparing a snapshot before and after `prune`)
    VcsManifest { path: String },
    /// Run each repo's configured health command inside a (read-only)
    /// snapshot; with --changes, only repos whose VCS metadata changed
    VcsHealth {
        /// `<subvolume>/.snapshots/<n>/snapshot`
        snapshot: String,
        /// Output of `snapper status -o FILE prev..n`
        #[arg(long)]
        changes: Option<String>,
        /// The live subvolume it was taken from (default: from the path)
        #[arg(long)]
        subvolume: Option<String>,
        /// Per-repo timeout in seconds
        #[arg(long, default_value_t = 300)]
        timeout: u64,
    },
    /// Delete the `+`-decided directories inside a writable Snapper
    /// snapshot (never the live tree): the whole snapshot is managed
    Prune {
        /// `<subvolume>/.snapshots/<n>/snapshot`
        snapshot: String,
        /// List what would be pruned; change nothing
        #[arg(long)]
        dry_run: bool,
        /// Report what the decisions newly prune compared with the oldest
        /// snapshot this tool pruned within this window before the
        /// snapshot's own date (1d, 6h, or a UTC time YYYY-MM-DD[ HH:MM]);
        /// append it to events.log, exit 4. Needs --config
        #[arg(long, requires = "config")]
        since: Option<String>,
        /// The Snapper config of the snapshot's subvolume (for --since)
        #[arg(long)]
        config: Option<String>,
    },
    /// Walk and resolve decisions like convert, but never convert
    /// anything - and hand-author decisions ahead of time
    Decide {
        /// The project: a decision-file/project-roots boundary (same
        /// registration rules as `convert`)
        #[arg(add = ArgValueCompleter::new(completions::registered_projects))]
        path: String,
        #[arg(long)]
        max_depth: Option<u32>,
        /// Pattern to record as `+` (convert) - used verbatim, repeatable
        #[arg(long = "add", add = ArgValueCompleter::new(completions::pending_patterns))]
        add: Vec<String>,
        /// Pattern to record as `-` (never convert) - used verbatim, repeatable
        #[arg(long = "deny", add = ArgValueCompleter::new(completions::pending_patterns))]
        deny: Vec<String>,
    },
    /// Manage the registered project-roots list
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },
}

#[cfg(target_os = "linux")]
#[derive(Subcommand)]
enum RootsAction {
    /// Detect BTRFS snapshot-managed roots (dry run unless --save)
    Scan {
        #[arg(long)]
        save: bool,
    },
    /// List every root.d-configured root and its effective watch list
    List,
    /// Disable a configured root - writes roots.d/10-disable.toml, never
    /// touching 00-auto.toml or any hand-edited file
    Disable {
        #[arg(add = ArgValueCompleter::new(completions::enabled_roots))]
        path: String,
    },
    /// Re-enable a root previously disabled via `roots disable`
    Enable {
        #[arg(add = ArgValueCompleter::new(completions::disabled_roots))]
        path: String,
    },
}

#[cfg(target_os = "linux")]
#[derive(Subcommand)]
enum ProjectsAction {
    /// List every registered project root, flagging any that no longer exist
    List,
    /// Register a project-root path for a narrower decision-file walk-up boundary
    Register { path: String },
    /// Remove a project root. With no path: scan every entry and interactively
    /// offer to prune ones that no longer exist on disk
    Unregister {
        #[arg(add = ArgValueCompleter::new(completions::registered_projects))]
        path: Option<String>,
    },
}

/// Resolves a raw CLI path argument to the absolute, physical path the
/// kernel resolves (as in Snapper snapshots and `prune`): the deepest existing ancestor is
/// canonicalized (symlinks and `..` resolved by the kernel, not
/// lexically), and the not-yet-existing rest appended — it must be plain
/// names, since `..` past a missing directory can't be resolved. Every
/// path argument goes through this, so a relative argument never
/// silently operates relative to whatever the cwd happens to be.
#[cfg(target_os = "linux")]
/// The earlier snapshot `prune --since` compares with: `Ok(None)` when
/// there's none yet (a first run: everything is new), `Err` when it can't
/// be determined — Snapper failed, or `config` belongs to another
/// subvolume (then everything is reported too, but summarized in the log).
#[cfg(target_os = "linux")]
fn baseline_snapshot(
    snapshot: &std::path::Path,
    subvolume: &std::path::Path,
    config: &str,
    since: &snapper::Since,
    now: std::time::SystemTime,
) -> anyhow::Result<Option<PathBuf>> {
    let timeout = std::time::Duration::from_secs(60);
    let configured = snapper::subvolume(config, timeout)?;
    if configured.canonicalize().ok().as_deref() != Some(subvolume) {
        anyhow::bail!(
            "snapper config {config:?} is for {}, not {}",
            configured.display(),
            subvolume.display()
        );
    }
    let number: u64 = snapshot
        .parent()
        .and_then(|n| n.file_name())
        .and_then(|n| n.to_str())
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("no snapshot number in {}", snapshot.display()))?;
    let snaps = snapper::list(config, timeout)?;
    let Some(n) = snapper::baseline(&snaps, number, since, now)? else {
        return Ok(None);
    };
    // Checked like the pruned snapshot (a symlink or a plain directory there
    // would compare the decisions with themselves and hide every change),
    // and it must be locked: the frozen state the listing claims.
    let path = subvolume
        .join(".snapshots")
        .join(n.to_string())
        .join("snapshot");
    if prune::checked_snapshot(&path)? != subvolume {
        anyhow::bail!("baseline snapshot {n} isn't in {}", subvolume.display());
    }
    if !btrfs::is_read_only(&path)? {
        anyhow::bail!("baseline snapshot {n} isn't read-only");
    }
    Ok(Some(path))
}

fn absolutize(path: &str) -> anyhow::Result<PathBuf> {
    use std::path::Component;
    let abs = std::path::absolute(path)
        .map_err(|e| anyhow::anyhow!("could not resolve path {path:?}: {e}"))?;
    let components: Vec<Component> = abs.components().collect();
    for split in (1..=components.len()).rev() {
        let existing: PathBuf = components[..split].iter().collect();
        match existing.canonicalize() {
            Ok(real) => {
                let rest = &components[split..];
                if rest.iter().any(|c| !matches!(c, Component::Normal(_))) {
                    anyhow::bail!(
                        "could not resolve path {path:?}: `..` after a missing directory"
                    );
                }
                return Ok(rest.iter().fold(real, |p, c| p.join(c)));
            }
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::NotFound | std::io::ErrorKind::NotADirectory
                ) => {}
            Err(e) => anyhow::bail!("could not resolve path {path:?}: {e}"),
        }
    }
    Ok(abs)
}

#[cfg(all(target_os = "linux", test))]
mod absolutize_tests {
    use super::absolutize;
    use std::path::PathBuf;

    #[test]
    fn an_already_absolute_path_is_returned_unchanged() {
        assert_eq!(
            absolutize("/already/absolute/path").unwrap(),
            PathBuf::from("/already/absolute/path")
        );
    }

    #[test]
    fn a_relative_path_resolves_against_the_current_directory() {
        // Only ever *reads* the current directory, never sets it - a
        // test that changed it would race every other test running
        // concurrently in this same process.
        let expected = std::env::current_dir().unwrap().join("some-subdir");
        assert_eq!(absolutize("some-subdir").unwrap(), expected);
    }

    #[test]
    fn symlinks_and_dot_dot_resolve_like_the_kernel_not_lexically() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("real/a")).unwrap();
        std::fs::create_dir(root.join("other")).unwrap();
        std::os::unix::fs::symlink("../real", root.join("other/link")).unwrap();
        let abs = |p: &str| absolutize(root.join(p).to_str().unwrap()).unwrap();
        assert_eq!(abs("other/link/a"), root.join("real/a"));
        // Lexically this would be `other/link`; the kernel says `real`.
        assert_eq!(abs("other/link/a/.."), root.join("real"));
        assert_eq!(abs("other/link/new/deeper"), root.join("real/new/deeper"));
    }

    #[test]
    fn dot_dot_after_a_missing_directory_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(absolutize(dir.path().join("missing/../x").to_str().unwrap()).is_err());
    }
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    clap_complete::CompleteEnv::with_factory(Cli::command).complete();

    let cli = Cli::parse();
    match cli.command {
        Command::Roots { action } => match action {
            RootsAction::Scan { save } => {
                let roots = scan::detect_roots()?;
                if save {
                    let config_dir = xdg::config_dir()?;
                    scan::save_roots(&config_dir, &roots)?;
                    let cache_path = xdg::data_dir()?.join(filenames::COMPILED_CACHE_FILE_NAME);
                    reload::reload(&config_dir, &cache_path)?;
                } else {
                    for root in &roots {
                        println!("{root}");
                    }
                }
                Ok(())
            }
            RootsAction::List => {
                let config_dir = xdg::config_dir()?;
                let merged = merge::load_all(&config_dir)?;
                for root in &merged.roots {
                    println!("{}\t{}", root.path, root.watches.join(", "));
                }
                Ok(())
            }
            RootsAction::Disable { path } => {
                let config_dir = xdg::config_dir()?;
                roots::disable(&config_dir, &absolutize(&path)?.to_string_lossy())?;
                let cache_path = xdg::data_dir()?.join(filenames::COMPILED_CACHE_FILE_NAME);
                reload::reload(&config_dir, &cache_path)
            }
            RootsAction::Enable { path } => {
                let config_dir = xdg::config_dir()?;
                roots::enable(&config_dir, &absolutize(&path)?.to_string_lossy())?;
                let cache_path = xdg::data_dir()?.join(filenames::COMPILED_CACHE_FILE_NAME);
                reload::reload(&config_dir, &cache_path)
            }
        },
        Command::Reload => {
            let config_dir = xdg::config_dir()?;
            let cache_path = xdg::data_dir()?.join(filenames::COMPILED_CACHE_FILE_NAME);
            reload::reload(&config_dir, &cache_path)
        }
        Command::Init => {
            let config_dir = xdg::config_dir()?;
            let data_dir = xdg::data_dir()?;
            init::init(&config_dir, &data_dir)
        }
        Command::Discover {
            path,
            max_depth,
            root_is_project,
            no_project,
            ignore: ignore_paths,
        } => {
            let config_dir = xdg::config_dir()?;
            let start = match path {
                Some(p) => absolutize(&p)?,
                None => PathBuf::from(std::env::var("HOME")?),
            };
            let no_project = no_project
                .iter()
                .map(|p| absolutize(p))
                .collect::<anyhow::Result<Vec<PathBuf>>>()?;
            let ignore_paths = ignore_paths
                .iter()
                .map(|p| absolutize(p))
                .collect::<anyhow::Result<Vec<PathBuf>>>()?;
            let merged = merge::load_all(&config_dir)?;
            let matches = discover::walk(
                &start,
                Some(max_depth),
                &merged.all_watched_names(),
                &merged.ignore,
                &ignore_paths,
            );
            let suggestions = discover::merge_nested_suggestions(
                discover::group_by_parent(matches),
                &start,
                root_is_project,
                &no_project,
            );
            print!("{}", discover::format_report(&suggestions));
            Ok(())
        }
        Command::Convert {
            path,
            max_depth,
            create,
            dry_run,
            delete_backup,
        } => {
            let config_dir = xdg::config_dir()?;
            let data_dir = xdg::data_dir()?;
            let cache_path = data_dir.join(filenames::COMPILED_CACHE_FILE_NAME);
            let project_roots_path = data_dir.join(filenames::PROJECT_ROOTS_FILE_NAME);
            let project_path = absolutize(&path)?;
            // Relative to the project, not independently absolutized -
            // `--create` names something *under* the project being
            // pointed at, not an arbitrary unrelated filesystem path.
            let create_paths: Vec<PathBuf> = create.iter().map(|c| project_path.join(c)).collect();
            convert::convert(
                &project_path,
                &create_paths,
                max_depth,
                &config_dir,
                &cache_path,
                &project_roots_path,
                &data_dir,
                dry_run,
                delete_backup,
            )
        }
        Command::Contrib { name } => {
            const FILES: &[(&str, &str)] = &[
                (
                    "ghostvolumes-snapshot",
                    include_str!("../contrib/ghostvolumes-snapshot"),
                ),
                (
                    "ghostvolumes-login-check",
                    include_str!("../contrib/ghostvolumes-login-check"),
                ),
                (
                    "ghostvolumes-snapshot@.service",
                    include_str!("../contrib/systemd/ghostvolumes-snapshot@.service"),
                ),
                (
                    "ghostvolumes-snapshot@.timer",
                    include_str!("../contrib/systemd/ghostvolumes-snapshot@.timer"),
                ),
                (
                    "ghostvolumes-notify@.service",
                    include_str!("../contrib/systemd/ghostvolumes-notify@.service"),
                ),
            ];
            match name {
                None => FILES.iter().for_each(|(n, _)| println!("{n}")),
                Some(name) => match FILES.iter().find(|(n, _)| *n == name) {
                    Some((_, text)) => print!("{text}"),
                    None => anyhow::bail!(
                        "no bundled file {name:?}; run `ghostvolumes contrib` to list them"
                    ),
                },
            }
            Ok(())
        }
        Command::VcsManifest { path } => {
            let vcs = vcs::load(&xdg::config_dir()?)?;
            let text = vcs::manifest(&absolutize(&path)?, &vcs)?;
            // `| head` closing the pipe isn't an error worth a panic.
            match std::io::Write::write_all(&mut std::io::stdout().lock(), text.as_bytes()) {
                Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
                other => Ok(other?),
            }
        }
        Command::VcsHealth {
            snapshot,
            changes,
            subvolume,
            timeout,
        } => {
            let snapshot = absolutize(&snapshot)?;
            let subvolume = match subvolume {
                Some(s) => absolutize(&s)?,
                None => prune::subvolume_of(&snapshot)?,
            };
            let vcs = vcs::load(&xdg::config_dir()?)?;
            // An unreadable or unparseable changes file means "check everything".
            let changed = changes
                .and_then(|path| std::fs::read_to_string(path).ok())
                .and_then(|text| vcs::parse_snapper_status(&text, &subvolume));
            let results = vcs::health(
                &snapshot,
                &subvolume,
                &vcs,
                changed.as_deref(),
                std::time::Duration::from_secs(timeout),
            );
            // Exit codes the timer script relies on: 0 ok, 1 a check failed,
            // 2 only timeouts.
            let (mut failed, mut timed_out) = (false, false);
            for (repo, result) in results {
                let repo = repo.display();
                match result {
                    vcs::Health::Ok => println!("ok: {repo}"),
                    vcs::Health::Unchanged => println!("unchanged: {repo}"),
                    vcs::Health::Unsupported(why) => println!("unsupported: {repo}: {why}"),
                    vcs::Health::TimedOut => {
                        timed_out = true;
                        println!("timeout: {repo}");
                    }
                    vcs::Health::Failed(why) => {
                        failed = true;
                        println!("FAILED: {repo}: {why}");
                    }
                }
            }
            std::process::exit(if failed {
                1
            } else if timed_out {
                2
            } else {
                0
            });
        }
        Command::Prune {
            snapshot,
            dry_run,
            since,
            config,
        } => {
            // Exit codes the timer script relies on: 0 clean, 1 something
            // refused (kept, logged), 2 couldn't prune, 3 already read-only,
            // 4 pruned under changed decisions (with --since; wins over 1).
            let fail = |e: anyhow::Error| -> ! {
                eprintln!("Error: {e:#}");
                std::process::exit(2);
            };
            let snapshot = absolutize(&snapshot).unwrap_or_else(|e| fail(e));
            let subvolume = prune::checked_snapshot(&snapshot).unwrap_or_else(|e| fail(e));
            // Already locked (a run died after locking it): nothing to prune
            // or report, and not a failure.
            if !dry_run && btrfs::is_read_only(&snapshot).unwrap_or_else(|e| fail(e.into())) {
                eprintln!("{}", prune::ReadOnly(snapshot.clone()));
                std::process::exit(3);
            }
            let since = since.map(|s| snapper::parse_since(&s).unwrap_or_else(|e| fail(e)));
            let now = std::time::SystemTime::now();
            match &since {
                Some(snapper::Since::At(at)) if *at > now => {
                    fail(anyhow::anyhow!("--since is in the future"))
                }
                Some(snapper::Since::Delta(d)) if now.checked_sub(*d).is_none() => {
                    fail(anyhow::anyhow!("--since {d:?} is out of range"))
                }
                _ => {}
            }
            if let Some(c) = config.as_deref().filter(|c| !snapper::valid_config(c)) {
                fail(anyhow::anyhow!("--config {c:?}: not a Snapper config name"));
            }
            let vcs = xdg::config_dir()
                .and_then(|c| vcs::load(&c))
                .unwrap_or_else(|e| fail(e));
            let candidates = prune::candidates(&snapshot, &vcs).unwrap_or_else(|e| fail(e));
            if candidates.is_empty() {
                eprintln!(
                    "note: no `+` decision matches anything in {}",
                    snapshot.display()
                );
            }
            // What the decisions now prune that they didn't at the baseline
            // (an earlier snapshot's decision files, applied to this tree).
            let (warnings, logged) = match (since, config) {
                (Some(since), Some(config)) => {
                    let baseline = baseline_snapshot(&snapshot, &subvolume, &config, &since, now);
                    let base_path = baseline.as_ref().ok().cloned().flatten();
                    let new =
                        prune::newly_pruned(&snapshot, base_path.as_deref(), &candidates, &vcs)
                            .unwrap_or_else(|e| fail(e));
                    let lines: Vec<String> = new
                        .iter()
                        .map(|path| {
                            let rel = path.strip_prefix(&snapshot).unwrap_or(path);
                            format!(
                                "will prune (decisions changed): {} {}",
                                rel.to_string_lossy().escape_debug(),
                                prune::summary(path)
                            )
                        })
                        .collect();
                    // A baseline that couldn't be determined (snapper down,
                    // config mismatch) reports everything every run: the
                    // journal gets it all, the events log one line per run.
                    let logged = match &baseline {
                        Err(why) if !lines.is_empty() => vec![format!(
                            "baseline unavailable ({why:#}): {} path(s) pruned unchecked, see the journal",
                            lines.len()
                        )],
                        _ => lines.clone(),
                    };
                    (lines, logged)
                }
                _ => (Vec::new(), Vec::new()),
            };
            for line in &warnings {
                println!("{line}");
            }
            if dry_run {
                for path in &candidates {
                    println!("would prune: {}", path.display());
                }
                return Ok(());
            }
            // Logged before anything is deleted, so a crash can't lose it. A
            // log that can't be written must not cost the snapshot: the
            // journal (stdout above) still has the lines.
            if !logged.is_empty() {
                match xdg::state_dir()
                    .and_then(|dir| prune::append_events(&dir, &snapshot, &logged))
                {
                    Ok(log) => println!("logged to {}", log.display()),
                    Err(e) => eprintln!("warning: couldn't write the events log: {e:#}"),
                }
            }
            let timeout = std::time::Duration::from_secs(60);
            let outcomes = match prune::prune(&snapshot, &candidates, &vcs, timeout) {
                Ok(outcomes) => outcomes,
                Err(e) if e.is::<prune::ReadOnly>() => {
                    eprintln!("{e}");
                    std::process::exit(3);
                }
                Err(e) => fail(e),
            };
            let mut refused = false;
            for outcome in outcomes {
                match outcome {
                    prune::Outcome::Pruned { path, bytes } => {
                        println!("pruned: {} ({bytes} bytes)", path.display())
                    }
                    prune::Outcome::Refused { path, reason } => {
                        refused = true;
                        println!("refused: {}: {reason}", path.display());
                    }
                }
            }
            if !warnings.is_empty() {
                std::process::exit(4);
            }
            std::process::exit(i32::from(refused));
        }
        Command::Decide {
            path,
            max_depth,
            add,
            deny,
        } => {
            let config_dir = xdg::config_dir()?;
            let data_dir = xdg::data_dir()?;
            let cache_path = data_dir.join(filenames::COMPILED_CACHE_FILE_NAME);
            let project_roots_path = data_dir.join(filenames::PROJECT_ROOTS_FILE_NAME);
            let project_path = absolutize(&path)?;
            convert::decide(
                &project_path,
                &add,
                &deny,
                max_depth,
                &config_dir,
                &cache_path,
                &project_roots_path,
                &data_dir,
            )
        }
        Command::Projects { action } => {
            let list_path = xdg::data_dir()?.join(filenames::PROJECT_ROOTS_FILE_NAME);
            match action {
                ProjectsAction::List => {
                    for (path, exists) in projects::list_projects(&list_path) {
                        if exists {
                            println!("{path}");
                        } else {
                            println!("{path} (missing)");
                        }
                    }
                    Ok(())
                }
                ProjectsAction::Register { path } => {
                    let path = absolutize(&path)?;
                    if !path.is_dir() {
                        anyhow::bail!("{} is not an existing directory", path.display());
                    }
                    let path = path.display().to_string();
                    projects::register(&list_path, &path)
                }
                ProjectsAction::Unregister { path } => {
                    let path = path
                        .map(|p| absolutize(&p))
                        .transpose()?
                        .map(|p| p.display().to_string());
                    projects::unregister(&list_path, path.as_deref())
                }
            }
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("GhostVolumes only supports Linux with BTRFS.");
    std::process::exit(1);
}
