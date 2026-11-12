//! Read-only diagnostics: never initialize state, spawn a broker, or touch shells.
use crate::{config, rpc::RpcClient, VERSION};
use serde_json::{json, Value};
use std::{
    ffi::OsStr,
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

pub fn report(home: &Path) -> Value {
    let mut checks = Vec::new();
    let mut steps = Vec::new();
    let configured_shell = std::env::var_os("AGENTBOX_BASH").unwrap_or_else(|| "bash".into());
    let resolved_shell = resolve_shell(&configured_shell, std::env::var_os("PATH").as_deref());
    checks.push(check("shell", resolved_shell.is_some(),
        "Configured AGENTBOX_BASH (default bash) must resolve to an executable file in the current environment"));
    if resolved_shell.is_none() {
        steps.push("Check AGENTBOX_BASH and PATH. Prefer an absolute Bash path when sessions use different working directories.");
    }
    let home_ok = config::private_dir_existing(home).is_ok();
    checks.push(check(
        "state_directory",
        home_ok,
        "State directory must exist, be owned by this user, private, and not a symlink",
    ));
    if !home_ok {
        steps.push("Check --home. For a new dedicated directory, run agentbox start --home PATH. For an existing directory, inspect its ownership and permissions.");
    }
    let client = if home_ok {
        RpcClient::new(home).ok()
    } else {
        None
    };
    checks.push(check("token", client.is_some(), "Token must be a private owned regular file containing 64 hexadecimal characters; its value is never displayed"));
    let socket_ok = home_ok
        && fs::symlink_metadata(home.join("broker.sock")).is_ok_and(|m| m.file_type().is_socket());
    checks.push(check(
        "socket",
        socket_ok,
        "broker.sock must be a Unix socket in the selected state directory",
    ));
    let mut broker = Value::Null;
    if let Some(client) = client.filter(|_| socket_ok) {
        match client.call_timeout("status", json!({}), Duration::from_secs(2)) {
            Ok(status) => {
                checks.push(check(
                    "broker",
                    true,
                    "Authenticated status request succeeded",
                ));
                let version_matches = status["version"] == VERSION;
                checks.push(check(
                    "version",
                    version_matches,
                    "Client and broker versions should match",
                ));
                let healthy = status["journal_healthy"] == true;
                checks.push(check(
                    "journal",
                    healthy,
                    "Broker reports whether the journal is healthy",
                ));
                if !version_matches {
                    steps.push("The running broker has a different version. Finish live work before planning a broker restart; detaching does not protect shells from a broker restart.");
                }
                // Whitelist fields: diagnostics must never echo arbitrary broker content.
                broker = json!({"version":status["version"],"pid":status["broker_pid"],"uptime_seconds":status["uptime"],"live_sessions":status["session_count"],"journal_healthy":status["journal_healthy"]});
            }
            Err(error) => {
                checks.push(check(
                    "broker",
                    false,
                    &format!("Status request failed (code {})", error.code),
                ));
                steps.push("Inspect the private broker.log and verify --home. A stale socket or stopped broker may require agentbox start; do not delete state or restart active shells blindly.");
            }
        }
    } else {
        checks.push(check(
            "broker",
            false,
            "Not contacted because the directory, token, or socket check failed",
        ));
        if home_ok && !socket_ok {
            steps.push(
                "The broker may be stopped. Run agentbox start with this --home to start it.",
            );
        }
    }
    let native_name = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/bash".into());
    let native_resolved = resolve_shell(&native_name, std::env::var_os("PATH").as_deref());
    let native_supported = native_resolved
        .as_ref()
        .is_some_and(|p| matches!(p.file_name().and_then(|s| s.to_str()), Some("bash" | "zsh")));
    let ready = checks.iter().all(|c| c["ok"] == true);
    json!({"ready":ready,"client_version":VERSION,"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
        "home":home.to_string_lossy(),"shell":{"configured":configured_shell.to_string_lossy(),"resolved":resolved_shell.map(|p|p.to_string_lossy().into_owned()),"environment":"current client environment; an already-running broker may have inherited different settings"},"native_shell":{"configured":native_name.to_string_lossy(),"resolved":native_resolved,"supported":native_supported,"environment":"new native CLI sessions use the creating client environment by default"},"checks":checks,"broker":broker,"next_steps":steps,
        "note":"This diagnosis is read-only. It does not test shell execution or validate an SSH connection."})
}
fn resolve_shell(command: &OsStr, path: Option<&OsStr>) -> Option<PathBuf> {
    let executable = |p: &Path| {
        fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    };
    let command = Path::new(command);
    if command.as_os_str().is_empty() {
        return None;
    }
    if command.components().count() > 1 || command.is_absolute() {
        return executable(command).then(|| command.to_owned());
    }
    std::env::split_paths(path?)
        .map(|dir| dir.join(command))
        .find(|candidate| executable(candidate))
}

fn check(name: &str, ok: bool, detail: &str) -> Value {
    json!({"name":name,"ok":ok,"detail":detail})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn configured_shell_resolution_checks_path_and_executable_permissions() {
        let temp = tempfile::tempdir().unwrap();
        let shell = temp.path().join("custom-bash");
        fs::write(&shell, "fixture").unwrap();
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(resolve_shell(shell.as_os_str(), None).is_none());
        fs::set_permissions(&shell, fs::Permissions::from_mode(0o700)).unwrap();
        assert_eq!(resolve_shell(shell.as_os_str(), None).unwrap(), shell);
        assert_eq!(
            resolve_shell(OsStr::new("custom-bash"), Some(temp.path().as_os_str())).unwrap(),
            shell
        );
        assert!(resolve_shell(OsStr::new("missing-bash"), Some(temp.path().as_os_str())).is_none());
    }
    #[test]
    fn missing_home_is_not_created() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("missing");
        let result = report(&home);
        assert_eq!(result["ready"], false);
        assert!(!home.exists());
    }
    #[test]
    fn token_is_never_reported_and_stopped_broker_is_not_started() {
        let temp = tempfile::tempdir().unwrap();
        let home = config::init_home(&temp.path().join("state")).unwrap();
        let token = config::read_token(&home).unwrap();
        let result = report(&home);
        assert_eq!(result["ready"], false);
        assert!(!result.to_string().contains(&token));
        assert!(!home.join("broker.sock").exists());
        assert!(!home.join("broker.log").exists());
    }
}
