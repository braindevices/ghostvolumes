// XDG base directory resolution (§2): `~/.config/ghostvolumes` and
// `~/.local/share/ghostvolumes` by default, honoring `XDG_CONFIG_HOME`/
// `XDG_DATA_HOME`. Spliced into src/xdg.rs via `include!`.

pub fn config_dir_from(home: &str, xdg_config_home: Option<&str>) -> std::path::PathBuf {
    match xdg_config_home {
        Some(dir) if !dir.is_empty() => std::path::Path::new(dir).join("ghostvolumes"),
        _ => std::path::Path::new(home)
            .join(".config")
            .join("ghostvolumes"),
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

pub fn state_dir_from(home: &str, xdg_state_home: Option<&str>) -> std::path::PathBuf {
    match xdg_state_home {
        Some(dir) if !dir.is_empty() => std::path::Path::new(dir).join("ghostvolumes"),
        _ => std::path::Path::new(home)
            .join(".local")
            .join("state")
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
    fn state_dir_defaults_to_dot_local_state_and_honors_the_override() {
        assert_eq!(
            state_dir_from("/home/user1", None),
            std::path::PathBuf::from("/home/user1/.local/state/ghostvolumes")
        );
        assert_eq!(
            state_dir_from("/home/user1", Some("/s")),
            std::path::PathBuf::from("/s/ghostvolumes")
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
