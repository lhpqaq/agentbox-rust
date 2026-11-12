//! Shell launch configuration. Environment values are transient, never journaled.
use crate::error::{AbError, Result};
use serde_json::{json, Value};
use std::{
    collections::BTreeMap,
    ffi::OsStr,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};

#[derive(Clone, Debug, Default)]
pub struct Launch {
    pub shell: Option<PathBuf>,
    pub login: bool,
    pub no_rc: bool,
    pub zsh_state: Option<String>,
    pub environment: Option<BTreeMap<String, String>>,
}
impl Launch {
    pub fn native(&self) -> bool {
        self.shell.is_some()
    }
    pub fn parse(p: &Value, cwd: &Path) -> Result<Self> {
        let boolean = |key: &str| match p.get(key) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(v)) => Ok(*v),
            _ => Err(AbError::new(422, format!("{key} must be a boolean"))),
        };
        let login = boolean("login")?;
        let no_rc = boolean("no_rc")?;
        let environment = match p.get("environment") {
            None | Some(Value::Null) => None,
            Some(Value::Object(values)) => {
                if values.len() > 2048 || serde_json::to_vec(values)?.len() > 128 * 1024 {
                    return Err(AbError::new(
                        422,
                        "Environment exceeds 128 KiB or 2048 variables",
                    ));
                }
                let mut out = BTreeMap::new();
                for (key, value) in values {
                    let value = value
                        .as_str()
                        .ok_or_else(|| AbError::new(422, "Environment values must be strings"))?;
                    if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
                        return Err(AbError::new(
                            422,
                            "Invalid environment name or NUL in value",
                        ));
                    }
                    if key != "AGENTBOX_TOKEN" && key != "AGENTBOX_SESSION" {
                        out.insert(key.clone(), value.to_owned());
                    }
                }
                Some(out)
            }
            _ => {
                return Err(AbError::new(
                    422,
                    "environment must be an object of strings",
                ))
            }
        };
        let shell = match p.get("shell") {
            None | Some(Value::Null) => None,
            Some(Value::String(name)) if !name.is_empty() && !name.contains('\0') => {
                let path = environment
                    .as_ref()
                    .map(|e| e.get("PATH").map(OsStr::new))
                    .unwrap_or_else(|| None);
                let broker_path = std::env::var_os("PATH");
                let path = if environment.is_some() {
                    path
                } else {
                    broker_path.as_deref()
                };
                let shell = resolve(OsStr::new(name), path, cwd).ok_or_else(|| {
                    AbError::new(422, "Shell must resolve to an executable Bash or Zsh file")
                })?;
                if !matches!(
                    shell.file_name().and_then(|s| s.to_str()),
                    Some("bash" | "zsh")
                ) {
                    return Err(AbError::new(422, "Native shell must be named bash or zsh"));
                }
                Some(shell)
            }
            _ => {
                return Err(AbError::new(
                    422,
                    "shell must be a nonempty Bash or Zsh path",
                ))
            }
        };
        if shell.is_none() && (login || no_rc) {
            return Err(AbError::new(422, "login and no_rc require a native shell"));
        }
        let zsh_state = match p.get("zsh_state") {
            None | Some(Value::Null) => None,
            Some(Value::String(state)) if state.len() <= 128 * 1024 && !state.contains('\0') => {
                Some(state.clone())
            }
            _ => {
                return Err(AbError::new(
                    422,
                    "zsh_state must be UTF-8 shell source without NUL, at most 128 KiB",
                ))
            }
        };
        if zsh_state.is_some()
            && (shell.as_ref().and_then(|p| p.file_name()) != Some(OsStr::new("zsh")) || no_rc)
        {
            return Err(AbError::new(
                422,
                "State import requires native Zsh with startup files enabled",
            ));
        }
        Ok(Self {
            zsh_state,
            shell,
            login,
            no_rc,
            environment,
        })
    }
    pub fn metadata(&self) -> Value {
        json!({"mode":if self.native(){"native"}else{"managed"},"executable":self.shell,
            "state_imported":false,"state_import_requested":self.zsh_state.is_some(),"login":self.login,"startup_files":self.native() && !self.no_rc,
            "environment_source":if self.environment.is_some(){"client_snapshot"}else{"broker"},
            "structured_commands":!self.native(),"cwd_tracking":if self.native(){"initial_directory_only"}else{"managed_prompt"}})
    }
    pub fn command(&self, rc: &Path) -> Command {
        let mut cmd = if let Some(shell) = &self.shell {
            let mut cmd = Command::new(shell);
            if self.no_rc {
                if shell.file_name() == Some(OsStr::new("bash")) {
                    cmd.args(["--noprofile", "--norc"]);
                } else {
                    cmd.arg("-f");
                }
            }
            if self.login {
                cmd.arg("-l");
            }
            cmd.arg("-i");
            cmd
        } else {
            let mut cmd =
                Command::new(std::env::var_os("AGENTBOX_BASH").unwrap_or_else(|| "bash".into()));
            cmd.args(["--noprofile", "--rcfile"]).arg(rc).arg("-i");
            cmd
        };
        if let Some(environment) = &self.environment {
            cmd.env_clear().envs(environment);
        }
        // Native startup owns prompt, editing mode, plugins and user configuration.
        if !self.native() {
            for key in [
                "BASH_ENV",
                "ENV",
                "PS1",
                "PS2",
                "PROMPT_COMMAND",
                "SHELLOPTS",
                "BASHOPTS",
            ] {
                cmd.env_remove(key);
            }
        }
        if let Some(shell) = &self.shell {
            cmd.env("SHELL", shell);
        }
        cmd.env_remove("AGENTBOX_TOKEN");
        cmd
    }
}
pub fn resolve(command: &OsStr, path: Option<&OsStr>, cwd: &Path) -> Option<PathBuf> {
    let executable = |p: &Path| {
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let command = Path::new(command);
    if command.components().count() > 1 || command.is_absolute() {
        let p = if command.is_absolute() {
            command.to_owned()
        } else {
            cwd.join(command)
        };
        return executable(&p).then_some(p);
    }
    std::env::split_paths(path?)
        .map(|p| if p.is_absolute() { p } else { cwd.join(p) })
        .map(|p| p.join(command))
        .find(|p| executable(p))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_without_leaking_environment_values_into_metadata() {
        let secret = "secret-fixture-do-not-journal";
        let launch = Launch::parse(&json!({"shell":"/bin/bash", "environment":{"PATH":"/bin", "TEST":secret, "AGENTBOX_TOKEN":secret}}), Path::new("/tmp")).unwrap();
        assert!(!launch
            .environment
            .as_ref()
            .unwrap()
            .contains_key("AGENTBOX_TOKEN"));
        assert!(!launch.metadata().to_string().contains(secret));
        assert_eq!(launch.metadata()["structured_commands"], false);
        for p in [
            json!({"login":true}),
            json!({"shell":"/bin/sh"}),
            json!({"environment":{"A=B":"x"}}),
            json!({"environment":{"A":1}}),
            json!({"environment":{"A":"\u{0}"}}),
        ] {
            assert!(Launch::parse(&p, Path::new("/tmp")).is_err());
        }
    }
}
