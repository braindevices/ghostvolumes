//! XDG base directory resolution (the pure `*_from` logic is in
//! `xdg_core.rs`, pulled in below).

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

pub fn state_dir() -> anyhow::Result<PathBuf> {
    let home = std::env::var("HOME")?;
    Ok(state_dir_from(
        &home,
        std::env::var("XDG_STATE_HOME").ok().as_deref(),
    ))
}

// Kept last so the shared file's own #[cfg(test)] mod stays the final
// item in this file (avoids clippy::items_after_test_module).
include!("xdg_core.rs");
