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
mod discover;
#[cfg(target_os = "linux")]
mod ensure;
#[cfg(target_os = "linux")]
mod git;
#[cfg(target_os = "linux")]
mod init;
#[cfg(target_os = "linux")]
mod merge;
#[cfg(target_os = "linux")]
mod mountinfo;
#[cfg(target_os = "linux")]
mod pathmatch;
#[cfg(target_os = "linux")]
mod registration;
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
    /// Find pre-existing subvolumes and suggest projects.d entries
    Discover {
        path: Option<String>,
        #[arg(long)]
        max_depth: Option<u32>,
        #[arg(long)]
        save: bool,
    },
    /// Migrate a pre-existing populated directory into a subvolume
    Convert { path: String },
    /// Print the shell integration snippet for eval
    ShellInit { shell: String },
    /// Invoked by the cd-hook on every directory change
    Ensure { path: String },
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
            let suggestions = discover::group_and_gate(matches, git::is_git_tracked);
            let text = discover::format_toml(&suggestions);
            if save {
                let projects_local = config_dir.join("projects.d").join("local.toml");
                std::fs::create_dir_all(projects_local.parent().unwrap())?;
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&projects_local)?
                    .write_all(text.as_bytes())?;
                let cache_path = xdg::data_dir()?.join("compiled.tsv");
                reload::reload(&config_dir, &cache_path)?;
            } else {
                print!("{text}");
            }
            Ok(())
        }
        Command::Convert { path } => convert::convert(&PathBuf::from(path)),
        Command::ShellInit { shell } => {
            let data_dir = xdg::data_dir()?;
            print!("{}", shellinit::shell_init(&shell, &data_dir)?);
            Ok(())
        }
        Command::Ensure { path } => {
            let config_dir = xdg::config_dir()?;
            let data_dir = xdg::data_dir()?;
            let cache_path = data_dir.join("compiled.tsv");
            let session_id = unsafe { libc::getppid() };
            ensure::ensure(
                &PathBuf::from(path),
                &config_dir,
                &data_dir,
                &cache_path,
                &ensure::runtime_dir(),
                session_id,
            )
        }
    }
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("GhostVolumes only supports Linux with BTRFS.");
    std::process::exit(1);
}
