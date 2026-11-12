//! Read-only diagnostics: never initialize state, spawn a broker, or touch shells.
use crate::{config, rpc::RpcClient, VERSION};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::{FileTypeExt, PermissionsExt},
    path::Path,
    time::Duration,
};

pub fn report(home: &Path) -> Value {
    let mut checks = Vec::new();
    let mut steps = Vec::new();
    let shell_ok =
        fs::metadata("/bin/bash").is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0);
    checks.push(check(
        "shell",
        shell_ok,
        "/bin/bash must exist and be executable",
    ));
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
                    steps.push("The running broker has a different version. Finish or detach from work before planning a broker restart; restarting terminates live shells.");
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
    let ready = checks.iter().all(|c| c["ok"] == true);
    json!({"ready":ready,"client_version":VERSION,"platform":std::env::consts::OS,"architecture":std::env::consts::ARCH,
        "home":home,"checks":checks,"broker":broker,"next_steps":steps,
        "note":"This diagnosis is read-only. It does not test shell execution or validate an SSH connection."})
}
fn check(name: &str, ok: bool, detail: &str) -> Value {
    json!({"name":name,"ok":ok,"detail":detail})
}

#[cfg(test)]
mod tests {
    use super::*;
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
