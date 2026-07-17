//! Refuses to run if `LD_PRELOAD` already contains this installation's
//! own shim (implying it leaked in via a shell rc file), which risks
//! redirecting internal directory creation into a subvolume. Matches by
//! filename only, so it's robust to how `$HOME` resolves.

use std::path::Path;

/// `true` iff any colon-separated entry in `ld_preload` has
/// `shim_file_name` as its filename, ignoring its directory.
fn already_preloaded(ld_preload: Option<&str>, shim_file_name: &str) -> bool {
    let Some(ld_preload) = ld_preload else {
        return false;
    };
    ld_preload.split(':').any(|entry| {
        !entry.is_empty()
            && Path::new(entry).file_name().and_then(|n| n.to_str()) == Some(shim_file_name)
    })
}

/// `ld_preload` is injectable for testability; the real caller passes
/// `std::env::var("LD_PRELOAD").ok().as_deref()`.
pub fn refuse_if_shim_preloaded(
    ld_preload: Option<&str>,
    shim_file_name: &str,
) -> anyhow::Result<()> {
    if already_preloaded(ld_preload, shim_file_name) {
        anyhow::bail!(
            "LD_PRELOAD already contains {shim_file_name} - refusing to run.\n\
             Current LD_PRELOAD: {}\n\
             This almost always means `ghostvolumes shell-init`'s export line was added to a \
             shell rc file, which is not recommended (see FAQ.md's \"Why not just export \
             LD_PRELOAD globally?\"). Remove it from your rc file, then use \
             `ghostvolumes intercept -- <cmd>` per build instead (or `intercept -- bash`/`zsh` \
             for a whole wrapped session) - `ghostvolumes` itself should never run with its own \
             shim preloaded into it.",
            ld_preload.unwrap_or_default()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filenames::SHIM_FILE_NAME;

    #[test]
    fn ok_when_ld_preload_is_unset() {
        assert!(refuse_if_shim_preloaded(None, SHIM_FILE_NAME).is_ok());
    }

    #[test]
    fn ok_when_ld_preload_is_unrelated() {
        assert!(refuse_if_shim_preloaded(Some("/usr/lib/libsomething.so"), SHIM_FILE_NAME).is_ok());
    }

    #[test]
    fn refuses_on_an_exact_single_entry_match() {
        let ld_preload = format!("/home/user1/.local/share/ghostvolumes/{SHIM_FILE_NAME}");
        let err = refuse_if_shim_preloaded(Some(&ld_preload), SHIM_FILE_NAME).unwrap_err();
        assert!(err.to_string().contains("refusing to run"));
        assert!(err.to_string().contains("shell-init"));
    }

    #[test]
    fn refuses_regardless_of_directory_since_only_the_basename_is_compared() {
        // A different install location, a symlinked $HOME, sudo -E with a
        // different effective home, ... - still the same shim by name.
        let ld_preload = format!("/some/other/path/{SHIM_FILE_NAME}");
        let err = refuse_if_shim_preloaded(Some(&ld_preload), SHIM_FILE_NAME).unwrap_err();
        assert!(err.to_string().contains("refusing to run"));
    }

    #[test]
    fn refuses_on_a_match_among_several_colon_separated_entries() {
        let ld_preload = format!(
            "/usr/lib/libsomething.so:/home/user1/.local/share/ghostvolumes/{SHIM_FILE_NAME}:/other.so"
        );
        assert!(refuse_if_shim_preloaded(Some(&ld_preload), SHIM_FILE_NAME).is_err());
    }

    #[test]
    fn ok_for_empty_string() {
        assert!(refuse_if_shim_preloaded(Some(""), SHIM_FILE_NAME).is_ok());
    }

    #[test]
    fn does_not_match_a_similarly_named_but_different_file() {
        let with_suffix = format!("/usr/lib/{SHIM_FILE_NAME}.bak");
        assert!(refuse_if_shim_preloaded(Some(&with_suffix), SHIM_FILE_NAME).is_ok());
        let with_prefix = format!("/usr/lib/not{SHIM_FILE_NAME}");
        assert!(refuse_if_shim_preloaded(Some(&with_prefix), SHIM_FILE_NAME).is_ok());
    }

    #[test]
    fn error_message_includes_the_raw_ld_preload_value_for_diagnosis() {
        let ld_preload = format!("/a/{SHIM_FILE_NAME}:/b/other.so");
        let err = refuse_if_shim_preloaded(Some(&ld_preload), SHIM_FILE_NAME).unwrap_err();
        assert!(err.to_string().contains(&ld_preload));
    }
}
