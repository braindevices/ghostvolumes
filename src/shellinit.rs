//! `ghostvolumes shell-init <shell>` (§8.2): prints a shell snippet for
//! the user to `eval`, following the same pattern as `starship`,
//! `zoxide`, `direnv` — never edits rc files directly. Only the
//! `LD_PRELOAD` export remains — no `cd` wrapper/`chpwd` hook: `ensure`
//! (cd-hook) was removed entirely
//! (ai-work/tasks/decision-model.plan.md §5/§7). Its old
//! responsibilities (proactive pre-creation, repo-local-config
//! registration, plain-directory warnings) all have homes elsewhere now
//! — see `convert` (§6) and `intercept` (§5).

use std::path::Path;

pub fn shell_init(shell: &str, data_dir: &Path) -> anyhow::Result<String> {
    let so_path = data_dir.join("preload.so");
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
        assert!(text.contains("/home/user1/.local/share/ghostvolumes/preload.so"));
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
        assert!(text.contains("/home/user1/.local/share/ghostvolumes/preload.so"));
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
