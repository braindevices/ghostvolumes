// XDG base directory resolution (§2): `~/.config/ghostvolumes` and
// `~/.local/share/ghostvolumes` by default, honoring
// `XDG_CONFIG_HOME`/`XDG_DATA_HOME` overrides. Pure logic - takes
// `home`/override as plain arguments rather than reading the
// environment itself, so it's shareable as-is between the main crate
// (`src/xdg.rs`, via `include!`) and the shim (`shim/preload.rs`, via
// `mod`) - the shim MUST resolve `compiled.tsv`'s path exactly the way
// `reload`/`init` do, or a user with `XDG_DATA_HOME` set would have a
// shim silently reading from the wrong (or no) file.

pub fn config_dir_from(home: &str, xdg_config_home: Option<&str>) -> std::path::PathBuf {
    match xdg_config_home {
        Some(dir) if !dir.is_empty() => std::path::Path::new(dir).join("ghostvolumes"),
        _ => std::path::Path::new(home).join(".config").join("ghostvolumes"),
    }
}

pub fn data_dir_from(home: &str, xdg_data_home: Option<&str>) -> std::path::PathBuf {
    match xdg_data_home {
        Some(dir) if !dir.is_empty() => std::path::Path::new(dir).join("ghostvolumes"),
        _ => std::path::Path::new(home)
            .join(".local")
            .join("share")
            .join("ghostvolumes"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_to_dot_config_under_home() {
        assert_eq!(
            config_dir_from("/home/user1", None),
            std::path::PathBuf::from("/home/user1/.config/ghostvolumes")
        );
    }

    #[test]
    fn defaults_to_dot_local_share_under_home() {
        assert_eq!(
            data_dir_from("/home/user1", None),
            std::path::PathBuf::from("/home/user1/.local/share/ghostvolumes")
        );
    }

    #[test]
    fn xdg_config_home_override_takes_precedence() {
        assert_eq!(
            config_dir_from("/home/user1", Some("/custom/config")),
            std::path::PathBuf::from("/custom/config/ghostvolumes")
        );
    }

    #[test]
    fn empty_xdg_override_falls_back_to_default() {
        assert_eq!(
            config_dir_from("/home/user1", Some("")),
            std::path::PathBuf::from("/home/user1/.config/ghostvolumes")
        );
    }
}
