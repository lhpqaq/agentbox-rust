//! One authenticated request per Unix socket, with no automatic mutation retry.
use crate::{
    config,
    error::{AbError, Result},
};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::{
        fd::AsRawFd,
        unix::{net::UnixStream, process::CommandExt},
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};
#[derive(Clone)]
pub struct RpcClient {
    pub home: PathBuf,
    pub token: String,
}
impl RpcClient {
    pub fn new(home: &Path) -> Result<Self> {
        Ok(Self {
            home: home.to_owned(),
            token: config::read_token(home)?,
        })
    }
    pub fn call(&self, method: &str, params: Value) -> Result<Value> {
        self.call_timeout(method, params, Duration::from_secs(15))
    }
    pub fn call_timeout(&self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let started = Instant::now();
        if method == "create" {
            let requirements = [
                ("shell", "native_shell_v1"),
                ("environment", "client_environment_v1"),
                ("zsh_state", "zsh_state_v1"),
            ];
            if requirements
                .iter()
                .any(|(field, _)| params.get(field).is_some_and(|v| !v.is_null()))
            {
                let status = self.call_timeout("status", json!({}), timeout)?;
                for (field, capability) in requirements {
                    if params.get(field).is_some_and(|v| !v.is_null())
                        && !status["capabilities"]
                            .as_array()
                            .is_some_and(|items| items.iter().any(|v| v == capability))
                    {
                        return Err(AbError::new(409, format!("The running broker does not support {capability}. Use a new --home or finish live work before restarting it. No session was created.")));
                    }
                }
            }
        }
        let timeout = timeout
            .checked_sub(started.elapsed())
            .filter(|t| !t.is_zero())
            .ok_or_else(|| {
                AbError::new(504, "Broker request deadline expired before submission")
            })?;
        let mutating = matches!(
            method,
            "create" | "run" | "input" | "interrupt" | "close" | "rename" | "resize" | "shutdown"
        );
        let mut stream =
            UnixStream::connect(self.home.join("broker.sock")).map_err(|e| connect_error(&e))?;
        let deadline = Instant::now() + timeout;
        let result = (|| -> Result<Value> {
            stream
                .set_write_timeout(Some(timeout))
                .map_err(|e| AbError::new(500, format!("Setting broker write timeout: {e}")))?;
            let mut bytes =
                serde_json::to_vec(&json!({"token":self.token,"method":method,"params":params}))?;
            bytes.push(b'\n');
            if bytes.len() > 400 * 1024 {
                return Err(AbError::new(413, "Broker request too large"));
            }
            stream
                .write_all(&bytes)
                .map_err(|e| AbError::new(500, format!("Sending broker request: {e}")))?;
            // Poll a nonblocking socket against one absolute deadline. On Darwin,
            // changing SO_RCVTIMEO after the peer closes can fail with EINVAL even
            // while a complete response remains buffered in the socket.
            stream.set_nonblocking(true)?;
            let mut reader = BufReader::new(stream);
            let mut data = Vec::new();
            loop {
                let left = deadline
                    .checked_duration_since(Instant::now())
                    .ok_or_else(|| AbError::new(504, "Broker request timed out"))?;
                let mut ready = libc::pollfd {
                    fd: reader.get_ref().as_raw_fd(),
                    events: libc::POLLIN,
                    revents: 0,
                };
                let millis = left.as_millis().saturating_add(1).min(i32::MAX as u128) as i32;
                let polled = unsafe { libc::poll(&mut ready, 1, millis) };
                if polled == 0 {
                    continue;
                }
                if polled < 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() == std::io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(AbError::new(
                        500,
                        format!("Polling broker response: {error}"),
                    ));
                }
                let chunk = match reader.fill_buf() {
                    Ok(chunk) => chunk,
                    Err(e)
                        if matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue
                    }
                    Err(e) => {
                        return Err(AbError::new(500, format!("Reading broker response: {e}")))
                    }
                };
                if chunk.is_empty() {
                    return Err(AbError::new(502, "Broker disconnected before response"));
                }
                let n = chunk
                    .iter()
                    .position(|b| *b == b'\n')
                    .map_or(chunk.len(), |i| i + 1);
                if data.len() + n > 32 * 1024 * 1024 {
                    return Err(AbError::new(502, "Broker response too large"));
                }
                let done = chunk[n - 1] == b'\n';
                data.extend_from_slice(&chunk[..n]);
                reader.consume(n);
                if done {
                    break;
                }
            }
            let response: Value = serde_json::from_slice(&data)
                .map_err(|_| AbError::new(502, "Invalid broker response"))?;
            match response["ok"].as_bool() {
                Some(true) => response
                    .get("result")
                    .cloned()
                    .ok_or_else(|| AbError::new(502, "Missing broker result")),
                Some(false) => {
                    let code = response["code"]
                        .as_u64()
                        .filter(|n| (400..=599).contains(n))
                        .unwrap_or(500) as u16;
                    let mut e =
                        AbError::new(code, response["error"].as_str().unwrap_or("Broker error"));
                    e.from_broker = true;
                    e.uncertain = response["may_have_executed"].as_bool().unwrap_or(false);
                    Err(e)
                }
                None => Err(AbError::new(502, "Invalid broker envelope")),
            }
        })();
        result.map_err(|mut e| {
            if mutating && e.code >= 500 {
                e.uncertain = true;
            }
            e
        })
    }
}
fn connect_error(e: &std::io::Error) -> AbError {
    let code = match e.raw_os_error() {
        Some(libc::ENOENT) | Some(libc::ECONNREFUSED) => 503,
        Some(libc::EPERM) | Some(libc::EACCES) => 403,
        _ => 500,
    };
    AbError::new(code, format!("Cannot connect to local broker: {e}"))
}
pub fn ensure_broker(home: &Path) -> Result<Value> {
    let home = config::init_home(home)?;
    let client = RpcClient::new(&home)?;
    match client.call_timeout("status", json!({}), Duration::from_secs(1)) {
        Ok(v) => return Ok(v),
        Err(e) if e.code == 503 && !e.uncertain && !e.from_broker => {}
        Err(e) => return Err(e),
    }
    let log = config::private_file(&home.join("broker.log"), true)?;
    // Append without exposing token in argv or shell environment.
    unsafe {
        libc::fcntl(
            std::os::fd::AsRawFd::as_raw_fd(&log),
            libc::F_SETFL,
            libc::O_APPEND,
        );
    }
    let mut command = Command::new(std::env::current_exe()?);
    command
        .arg("broker")
        .arg("--home")
        .arg(&home)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log));
    command.env_remove("AGENTBOX_TOKEN");
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn()?;
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
        match client.call_timeout("status", json!({}), Duration::from_millis(400)) {
            Ok(v) => return Ok(v),
            Err(e) if e.code == 503 && !e.from_broker => {}
            Err(e) => return Err(e),
        }
        if let Some(status) = child.try_wait()? {
            return Err(AbError::new(
                503,
                format!("Broker exited ({status}); inspect private broker.log"),
            ));
        }
    }
    Err(AbError::new(
        503,
        "Broker startup timed out; inspect private broker.log before trying again",
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn permission_failure_does_not_qualify_for_spawn() {
        assert_eq!(
            connect_error(&std::io::Error::from_raw_os_error(libc::EPERM)).code,
            403
        );
        assert_eq!(
            connect_error(&std::io::Error::from_raw_os_error(libc::ENOENT)).code,
            503
        );
    }
}
