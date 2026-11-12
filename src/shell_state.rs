//! Opt-in Zsh state import. Private temporary startup shims preserve user rc order.
//! Imported text is trusted user shell code, never inferred from terminal output.
use anyhow::Result;
use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}
fn write(path: &Path, text: &str) -> Result<()> {
    fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?
        .write_all(text.as_bytes())?;
    Ok(())
}
pub fn prepare(runtime: &Path, launch: &crate::shell::Launch) -> Result<Option<PathBuf>> {
    let Some(state) = launch.zsh_state.as_ref() else {
        return Ok(None);
    };
    let directory = runtime.join(format!(".zsh-state-{}", uuid::Uuid::new_v4().simple()));
    fs::DirBuilder::new().mode(0o700).create(&directory)?;
    let result = (|| -> Result<()> {
        write(&directory.join("state.zsh"), state)?;
        let original = match &launch.environment {
            Some(env) => env.get("ZDOTDIR").cloned(),
            None => std::env::var("ZDOTDIR").ok(),
        };
        let dir = quote(
            directory
                .to_str()
                .ok_or_else(|| anyhow::anyhow!("State directory must be UTF-8"))?,
        );
        let initialize = format!(
            "typeset -g __agentbox_state_zdot_set={}\ntypeset -g __agentbox_state_zdot={}\n",
            u8::from(original.is_some()),
            quote(original.as_deref().unwrap_or(""))
        );
        let restore = "if (( __agentbox_state_zdot_set )); then\n  ZDOTDIR=$__agentbox_state_zdot\nelse\n  builtin unset ZDOTDIR\nfi\n";
        let capture = format!("__agentbox_state_zdot_set=${{+ZDOTDIR}}\n__agentbox_state_zdot=${{ZDOTDIR-}}\nZDOTDIR={dir}\n");
        // The import is sourced at global scope so typeset declarations remain
        // visible. State/options are applied only after ordinary startup files.
        let finish = format!(
            r#"builtin source {dir}/state.zsh
__agentbox_state_result=$?
command /bin/rm -rf -- {dir}
if (( __agentbox_state_result == 0 )); then
  builtin printf 'I\0\0%d\0%s\0%s\0' 0 "$PWD" "$$" >&198
else
  builtin printf 'AgentBox: importing Zsh state failed\n' >&2
fi
__agentbox_state_fd=198
exec {{__agentbox_state_fd}}>&-
builtin unset __agentbox_state_zdot_set __agentbox_state_zdot __agentbox_state_result __agentbox_state_fd
"#
        );
        for file in [".zshenv", ".zprofile", ".zshrc", ".zlogin"] {
            let mut script = String::new();
            if file == ".zshenv" {
                script.push_str(&initialize);
            }
            script.push_str(restore);
            script.push_str(&format!("if [[ -r ${{ZDOTDIR-$HOME}}/{file} ]]; then\n  builtin source \"${{ZDOTDIR-$HOME}}/{file}\"\nfi\n"));
            // Respect RCS being disabled by user configuration: there will be no
            // later startup file in which to finish the import.
            if file == ".zlogin" {
                script.push_str(&finish);
            } else {
                let terminal_stage = if file == ".zshrc" {
                    " || ! -o login"
                } else {
                    ""
                };
                script.push_str(&format!(
                    "if [[ ! -o rcs{terminal_stage} ]]; then\n{finish}else\n{capture}fi\n"
                ));
            }
            write(&directory.join(file), &script)?;
        }
        Ok(())
    })();
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&directory);
        return Err(error);
    }
    Ok(Some(directory))
}
