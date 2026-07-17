//! XDG base directory resolution. The pure `*_from` logic is shared with
//! the LD_PRELOAD shim, which must resolve `compiled.tsv`'s path exactly
//! the same way or a custom `XDG_DATA_HOME` would silently break it.

use std::path::PathBuf;

pub fn config_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(config_dir_from(
        &home,
        std::env::var("XDG_CONFIG_HOME").ok().as_deref(),
    ))
}

pub fn data_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(data_dir_from(
        &home,
        std::env::var("XDG_DATA_HOME").ok().as_deref(),
    ))
}

// Kept last so the shared file's own #[cfg(test)] mod stays the final
// item in this file (avoids clippy::items_after_test_module).
include!("../shim/xdg_core.rs");
