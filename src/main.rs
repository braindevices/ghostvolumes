// BTRFS ioctls, LD_PRELOAD, and /proc/self/mountinfo are all
// Linux-specific (§8.3) - every module here depends on at least one of
// them, so gate the whole implementation rather than have it fail to
// compile confusingly on other platforms.
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
mod init;
#[cfg(target_os = "linux")]
mod intercept;
#[cfg(target_os = "linux")]
mod merge;
#[cfg(target_os = "linux")]
mod mountinfo;
#[cfg(target_os = "linux")]
mod project_roots;
#[cfg(target_os = "linux")]
mod register;
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

#[cfg(target_os = "linux")]
#[derive(Parser)]
#[command(
    name = "ghostvolumes",
    version,
    about = "Isolate volatile build artifacts into unsnapshotted BTRFS subvolumes"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[cfg(target_os = "linux")]
#[derive(Subcommand)]
enum Command {
    /// Detect BTRFS snapshot-managed roots (dry run unless --save)
    Scan {
        #[arg(long)]
        save: bool,
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
    /// Register a project-root path for a narrower decision-file walk-up boundary
    Register { path: String },
    /// Run <cmd> with the shim preloaded into it (and only it)
    Intercept {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        cmd: Vec<String>,
    },
    /// Print the shell integration snippet for eval
    ShellInit { shell: String },
}

#[cfg(target_os = "linux")]
fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Scan { save } => {
            let roots = scan::detect_roots()?;
            if save {
                let config_dir = xdg::config_dir()?;
                scan::save_roots(&config_dir, &roots)?;
                let cache_path = xdg::data_dir()?.join("compiled.tsv");
                reload::reload(&config_dir, &cache_path)?;
            } else {
                for root in &roots {
                    println!("{root}");
                }
            }
            Ok(())
        }
        Command::Reload => {
            let config_dir = xdg::config_dir()?;
            let cache_path = xdg::data_dir()?.join("compiled.tsv");
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
                Some(p) => PathBuf::from(p),
                None => PathBuf::from(std::env::var("HOME")?),
            };
            let merged = merge::load_all(&config_dir)?;
            let matches = discover::walk(&start, max_depth, &merged.global_defaults);
            let suggestions = discover::group_by_parent(matches);
            if save {
                for s in &suggestions {
                    let file_path = s.path.join(decision::DECISION_FILE_NAME);
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
                    for line in new_lines {
                        writeln!(file, "{line}")?;
                    }
                }
            } else {
                print!("{}", discover::format_decisions(&suggestions));
            }
            Ok(())
        }
        Command::Convert { path, max_depth } => {
            let data_dir = xdg::data_dir()?;
            let cache_path = data_dir.join("compiled.tsv");
            let project_roots_path = project_roots::path_in(&data_dir);
            convert::convert(
                &PathBuf::from(path),
                max_depth,
                &cache_path,
                &project_roots_path,
            )
        }
        Command::Register { path } => {
            let list_path = project_roots::path_in(&xdg::data_dir()?);
            register::register(&list_path, &path)
        }
        Command::Intercept { cmd } => {
            let data_dir = xdg::data_dir()?;
            let cache_path = data_dir.join("compiled.tsv");
            let project_roots_path = project_roots::path_in(&data_dir);
            let preload_so_path = data_dir.join("preload.so");
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
