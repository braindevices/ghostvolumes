//! `ghostvolumes shell-init <shell>` (§8.2): prints the `LD_PRELOAD`
//! export line, following the same output-a-snippet pattern as
//! `starship`/`zoxide`/`direnv` — never edits rc files directly. Only
//! the export remains — no `cd` wrapper/`chpwd` hook: `ensure`
//! (cd-hook) was removed entirely
//! (ai-work/tasks/decision-model.plan.md §5/§7).
//!
//! **Not recommended to `eval` into an rc file, despite the pattern
//! this follows.** Doing so makes `LD_PRELOAD` inherited by every
//! process the shell spawns — including every `ghostvolumes`
//! subcommand itself (`intercept`, `convert`, `register`, ...), which
//! silently breaks `intercept`'s own documented invariant ("the parent
//! never has `LD_PRELOAD` set on itself, only the child does") since
//! `ld.so` processes `LD_PRELOAD` at `exec()` time, before any of this
//! crate's own code ever runs — there's no way for a running process to
//! un-preload a library that's already mapped into itself. It also
//! makes `intercept` redundant for its main job (every command already
//! gets shim coverage regardless of wrapping), leaving only its
//! post-run notice as unique value. This function/subcommand exists
//! mainly to show, precisely, what `LD_PRELOAD` value `intercept` sets
//! internally — a diagnostic/reference tool, not a setup step. For
//! whole-session shim coverage without that downside, prefer
//! `ghostvolumes intercept -- bash` (or `zsh`) — an explicit,
//! deliberate wrapped subshell, not a permanent export on the parent
//! shell.

use std::path::Path;

pub fn shell_init(shell: &str, data_dir: &Path) -> anyhow::Result<String> {
    let so_path = data_dir.join(crate::filenames::SHIM_FILE_NAME);
    match shell {
        "bash" => Ok(ld_preload_export(&so_path)),
        "zsh" => Ok(ld_preload_export(&so_path)),
        other => anyhow::bail!("unsupported shell: {other} (supported: bash, zsh)"),
    }
}

fn ld_preload_export(so_path: &Path) -> String {
    // Appends to an existing LD_PRELOAD rather than clobbering it, in
    // case the user (or another tool) already has one set.
    format!(
        "export LD_PRELOAD=\"${{LD_PRELOAD:+$LD_PRELOAD:}}{}\"\n",
        so_path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn data_dir() -> PathBuf {
        PathBuf::from("/home/user1/.local/share/ghostvolumes")
    }

    #[test]
    fn bash_snippet_exports_ld_preload_pointing_at_preload_so() {
        let text = shell_init("bash", &data_dir()).unwrap();
        assert!(text.contains(&format!(
            "/home/user1/.local/share/ghostvolumes/{}",
            crate::filenames::SHIM_FILE_NAME
        )));
        assert!(text.contains("export LD_PRELOAD"));
    }

    #[test]
    fn bash_snippet_appends_to_existing_ld_preload_rather_than_clobbering() {
        let text = shell_init("bash", &data_dir()).unwrap();
        assert!(text.contains("${LD_PRELOAD:+$LD_PRELOAD:}"));
    }

    #[test]
    fn zsh_snippet_also_exports_ld_preload() {
        let text = shell_init("zsh", &data_dir()).unwrap();
        assert!(text.contains(&format!(
            "/home/user1/.local/share/ghostvolumes/{}",
            crate::filenames::SHIM_FILE_NAME
        )));
    }

    #[test]
    fn unsupported_shell_is_rejected() {
        let err = shell_init("fish", &data_dir()).unwrap_err();
        assert!(err.to_string().contains("fish"));
    }

    #[test]
    fn bash_snippet_is_syntactically_valid_bash() {
        let text = shell_init("bash", &data_dir()).unwrap();
        let status = std::process::Command::new("bash")
            .arg("-n") // parse only, don't execute
            .arg("-c")
            .arg(&text)
            .status()
            .expect("bash must be available to run this test");
        assert!(
            status.success(),
            "generated bash snippet has a syntax error"
        );
    }
}
