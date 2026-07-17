// BTRFS ioctls, LD_PRELOAD, and /proc/self/mountinfo are Linux-specific;
// gate the whole implementation rather than fail to compile confusingly
// elsewhere. `build_version_core.rs` has no OS dependency and is
// included here (not gated) so its `#[cfg(test)]` tests actually run.
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

/// `git describe` (via `build.rs`) pins down exactly which commit was
/// built, since `CARGO_PKG_VERSION` alone can't. `GHOSTVOLUMES_VERSION`
/// (also `build.rs`) is a full SemVer version off the latest git tag:
/// `develop`/`feature/*` bump the minor version, `hotfix/*` bumps the
/// patch version, so a pre-release suffix still sorts above the last
/// tagged release rather than looking older than what's shipped.
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
    /// Survey for undecided directories and suggest `decide` commands to run
    Discover {
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
    },
    /// Walk and resolve decisions like convert, but never convert
    /// anything - and hand-author decisions ahead of time
    Decide {
        /// The project: a decision-file/project-roots boundary (same
        /// registration rules as `convert`)
        path: String,
        #[arg(long)]
        max_depth: Option<u32>,
        /// Pattern to record as `+` (convert) - used verbatim, repeatable
        #[arg(long = "add")]
        add: Vec<String>,
        /// Pattern to record as `-` (never convert) - used verbatim, repeatable
        #[arg(long = "deny")]
        deny: Vec<String>,
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

/// Resolves a raw CLI path argument to an absolute path before use —
/// purely lexical (`std::path::absolute`, not `canonicalize`), so it
/// works without the path existing yet. Every path argument must go
/// through this so a relative argument never silently operates
/// relative to whatever the current directory happens to be.
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
            )
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
