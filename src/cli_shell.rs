use agentbox::shell;
use anyhow::{bail, Context, Result};
use clap::Args;
use serde_json::{json, Value};
use std::path::Path;

#[derive(Args, Debug, Default)]
pub struct ShellOptions {
    /// Native Bash/Zsh executable (uses normal startup files). Default for attach: $SHELL.
    #[arg(long, conflicts_with = "managed")]
    pub shell: Option<String>,
    /// Use the isolated managed Bash required by structured `run`.
    #[arg(long, conflicts_with_all = ["login", "no_rc"])]
    pub managed: bool,
    /// Start the native shell as a login shell.
    #[arg(long)]
    pub login: bool,
    /// Skip user startup files (Zsh still reads system zshenv).
    #[arg(long)]
    pub no_rc: bool,
    /// Copy exported variables from this CLI process, even for managed Bash.
    #[arg(long, conflicts_with = "broker_env")]
    pub inherit_env: bool,
    /// Use the broker's environment instead of the creating terminal's snapshot.
    #[arg(long, conflicts_with = "env")]
    pub broker_env: bool,
    /// Override an exported variable; repeatable. Values are not journaled.
    #[arg(long, value_name = "NAME=VALUE")]
    pub env: Vec<String>,
    /// Import trusted Zsh source from an owned private file (normally shell-init).
    #[arg(long, conflicts_with_all = ["managed", "no_rc"])]
    pub state_file: Option<std::path::PathBuf>,
    /// Remove the private import file after reading it (used by shell integration).
    #[arg(long, hide = true, requires = "state_file")]
    pub consume_state_file: bool,
}
impl ShellOptions {
    pub fn specified(&self) -> bool {
        self.shell.is_some()
            || self.managed
            || self.login
            || self.no_rc
            || self.inherit_env
            || self.consume_state_file
            || self.state_file.is_some()
            || self.broker_env
            || !self.env.is_empty()
    }
    pub fn apply(&self, request: &mut Value, native_default: bool, cwd: &Path) -> Result<()> {
        let native =
            self.shell.is_some() || (!self.managed && (native_default || self.login || self.no_rc));
        if (native && !self.broker_env) || self.inherit_env || !self.env.is_empty() {
            let mut env = serde_json::Map::new();
            for (key, value) in std::env::vars_os() {
                if key == "AGENTBOX_TOKEN" || key == "AGENTBOX_SESSION" {
                    continue;
                }
                let key = key.into_string().map_err(|_| {
                    anyhow::anyhow!("Cannot copy a non-UTF-8 environment name; use --broker-env")
                })?;
                let value = value.into_string().map_err(|_| {
                    anyhow::anyhow!("Cannot copy a non-UTF-8 environment value; use --broker-env")
                })?;
                env.insert(key, json!(value));
            }
            for item in &self.env {
                let (key, value) = item.split_once('=').context("--env requires NAME=VALUE")?;
                if key == "AGENTBOX_TOKEN" || key == "AGENTBOX_SESSION" {
                    bail!("AgentBox control variables cannot be supplied through --env");
                }
                env.insert(key.to_owned(), json!(value));
            }
            request["environment"] = Value::Object(env);
        }
        if native {
            let name = self
                .shell
                .clone()
                .or_else(|| std::env::var("SHELL").ok())
                .unwrap_or_else(|| "/bin/bash".into());
            // Resolve in the creating terminal, never in a stale broker PATH.
            let path = std::env::var_os("PATH");
            let resolved = shell::resolve(name.as_ref(), path.as_deref(), cwd)
                .context("Cannot resolve the selected native shell in the creating terminal")?;
            request["shell"] = json!(resolved);
            request["login"] = json!(self.login);
            request["no_rc"] = json!(self.no_rc);
        }
        if let Some(path) = &self.state_file {
            use std::io::Read;
            let mut state = String::new();
            agentbox::config::private_file(path, false)?
                .take(128 * 1024 + 1)
                .read_to_string(&mut state)
                .context("Read private UTF-8 Zsh state file")?;
            if state.len() > 128 * 1024 {
                bail!("Zsh state exceeds 128 KiB");
            }
            if self.consume_state_file {
                std::fs::remove_file(path).context("Remove consumed private state file")?;
            }
            request["zsh_state"] = json!(state);
        }
        shell::Launch::parse(request, cwd)?;
        Ok(())
    }
}
