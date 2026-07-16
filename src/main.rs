// BTRFS ioctls, LD_PRELOAD, and /proc/self/mountinfo are all
// Linux-specific (§8.3) - every module here depends on at least one of
// them, so gate the whole implementation rather than have it fail to
// compile confusingly on other platforms.
// Not gated behind `target_os = "linux"` like the modules below -
// `build_version_core.rs`'s logic is plain string/number parsing with
// no OS dependency, and only exists here (rather than only in
// `build.rs`) so its `#[cfg(test)]` unit tests actually run - see that
// file's doc comment for why.
#[cfg(test)]
mod build_version_check {
    include!("../build_version_core.rs");
}

#[cfg(target_os = "linux")]
mod atomic_write;
#[cfg(target_os = "linux")]
mod btrfs;
#[cfg(target_os = "linux")]
mod cache;
#[cfg(target_os = "linux")]
mod config;
#[cfg(target_os = "linux")]
mod convert;
#[cfg(target_os = "linux")]
mod decision;
#[cfg(target_os = "linux")]
mod discover;
#[cfg(target_os = "linux")]
mod filenames;
#[cfg(target_os = "linux")]
mod init;
#[cfg(target_os = "linux")]
mod intercept;
#[cfg(target_os = "linux")]
mod lock;
#[cfg(target_os = "linux")]
mod merge;
#[cfg(target_os = "linux")]
mod mountinfo;
#[cfg(target_os = "linux")]
mod preload_guard;
#[cfg(target_os = "linux")]
mod project_roots;
#[cfg(target_os = "linux")]
mod projects;
#[cfg(target_os = "linux")]
mod reload;
#[cfg(target_os = "linux")]
mod scan;
#[cfg(target_os = "linux")]
mod shellinit;
#[cfg(all(target_os = "linux", test))]
mod test_support;
#[cfg(target_os = "linux")]
mod xdg;

#[cfg(target_os = "linux")]
use std::path::PathBuf;

#[cfg(target_os = "linux")]
use clap::{Parser, Subcommand};

/// `CARGO_PKG_VERSION` alone (e.g. "0.3.2") doesn't say *which* commit
/// was actually built - two installs both claiming "0.3.2" could be
/// meaningfully different if the version wasn't bumped between them.
/// `git describe`'s output (via `build.rs`'s `vergen-gitcl` call) adds
/// that: exactly "v0.3.2" when built right at that tag, or
/// "v0.3.2-3-gabc1234" three commits past it. Needs a tag to actually
/// exist somewhere in history to be meaningful - with none at all,
/// `git describe` falls back to the bare commit hash instead (see
/// `CHANGELOG.md`'s 0.3.1 entry for this project's own tagging).
///
/// `GHOSTVOLUMES_VERSION` (also `build.rs`, its own
/// `current_branch()`/`compute_version()`) is a full SemVer version for
/// this project's GitFlow-shaped branches, not just `CARGO_PKG_VERSION`
/// plus a suffix: `main`/`master`/detached HEAD use `CARGO_PKG_VERSION`
/// unchanged, but `develop`/`feature/*` bump the *minor* version
/// (`-alpha`/`-dev`) and `hotfix/*` bumps the *patch* version (`-rc`),
/// off the latest git tag rather than off `Cargo.toml`. That bump is
/// what keeps SemVer precedence correct after a release: a pre-release
/// suffix left on the *same* base number as the release just tagged
/// (e.g. "0.3.2-alpha" right after tagging v0.3.2) would sort *below*
/// that release, making ongoing development look older than what's
/// already shipped.
#[cfg(target_os = "linux")]
const VERSION: &str = concat!(
    env!("GHOSTVOLUMES_VERSION"),
    " (",
    env!("VERGEN_GIT_DESCRIBE"),
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
    /// Compile and install the LD_PRELOAD shim, write default config
    Init,
    /// Find pre-existing subvolumes and suggest decision-file lines
    Discover {
        path: Option<String>,
        #[arg(long)]
        max_depth: Option<u32>,
        #[arg(long)]
        save: bool,
    },
    /// Recursively find and resolve subvolume candidates under path
    Convert {
        path: String,
        #[arg(long)]
        max_depth: Option<u32>,
    },
    /// Manage the registered project-roots list
    Projects {
        #[command(subcommand)]
        action: ProjectsAction,
    },
    /// Run <cmd> with the shim preloaded into it (and only it)
    Intercept {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Print the shell integration snippet for eval
    ShellInit { shell: String },
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
    Unregister { path: Option<String> },
}

/// Resolves a raw CLI path argument to an absolute path before it's
/// used anywhere — purely lexical (`std::path::absolute`, not
/// `canonicalize`): joins against the current directory and normalizes
/// `.`/`..` components without touching the filesystem or requiring the
/// path to already exist, since `convert`'s whole point is to work with
/// not-yet-existing targets too. A relative argument (e.g. a typo
/// missing a leading `/`) must never silently operate on, or create,
/// state relative to whatever the current directory happens to be —
/// this is the fix for exactly that: a stray subvolume and a
/// confusingly relative-looking lock file, both created under the
/// wrong location, traced back to an unresolved relative `convert`
/// argument.
#[cfg(target_os = "linux")]
fn absolutize(path: &str) -> anyhow::Result<PathBuf> {
    std::path::absolute(path).map_err(|e| anyhow::anyhow!("could not resolve path {path:?}: {e}"))
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
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    preload_guard::refuse_if_shim_preloaded(
        std::env::var("LD_PRELOAD").ok().as_deref(),
        filenames::SHIM_FILE_NAME,
    )?;
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
            save,
        } => {
            let config_dir = xdg::config_dir()?;
            let start = match path {
                Some(p) => absolutize(&p)?,
                None => PathBuf::from(std::env::var("HOME")?),
            };
            let merged = merge::load_all(&config_dir)?;
            let matches = discover::walk(&start, max_depth, &merged.all_watched_names());
            let suggestions = discover::group_by_parent(matches);
            if save {
                for s in &suggestions {
                    let file_path = s.path.join(filenames::DECISION_FILE_NAME);
                    let existing = std::fs::read_to_string(&file_path).unwrap_or_default();
                    let new_lines: Vec<String> = s
                        .names
                        .iter()
                        .map(|name| format!("+ {name}"))
                        .filter(|line| !existing.lines().any(|l| l.trim() == line))
                        .collect();
                    if new_lines.is_empty() {
                        continue;
                    }
                    use std::io::Write;
                    let mut file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&file_path)?;
                    // One write_all call for the whole block of new
                    // lines, not one writeln! per line - see
                    // register.rs's identical fix
                    // (ai-work/tasks/atomic-file-io.plan.md §3); also
                    // makes this whole batch of lines land as one
                    // atomic unit rather than N separate appends.
                    let block: String = new_lines.iter().map(|line| format!("{line}\n")).collect();
                    file.write_all(block.as_bytes())?;
                }
            } else {
                print!("{}", discover::format_decisions(&suggestions));
            }
            Ok(())
        }
        Command::Convert { path, max_depth } => {
            let data_dir = xdg::data_dir()?;
            let cache_path = data_dir.join(filenames::COMPILED_CACHE_FILE_NAME);
            let project_roots_path = data_dir.join(filenames::PROJECT_ROOTS_FILE_NAME);
            convert::convert(
                &absolutize(&path)?,
                max_depth,
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
                    let path = absolutize(&path)?.display().to_string();
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
        Command::Intercept { cmd } => {
            let data_dir = xdg::data_dir()?;
            let cache_path = data_dir.join(filenames::COMPILED_CACHE_FILE_NAME);
            let project_roots_path = data_dir.join(filenames::PROJECT_ROOTS_FILE_NAME);
            let preload_so_path = data_dir.join(filenames::SHIM_FILE_NAME);
            let code =
                intercept::intercept(&cmd, &preload_so_path, &cache_path, &project_roots_path)?;
            std::process::exit(code);
        }
        Command::ShellInit { shell } => {
            let data_dir = xdg::data_dir()?;
            print!("{}", shellinit::shell_init(&shell, &data_dir)?);
            Ok(())
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("GhostVolumes only supports Linux with BTRFS.");
    std::process::exit(1);
}
