//! Explicitly opt-in socket tests. Never run on an environment that denied sockets.
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Read, Write},
    net::{TcpListener, TcpStream},
    path::PathBuf,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};
const BIN: &str = env!("CARGO_BIN_EXE_agentbox");
struct Instance {
    home: PathBuf,
    port: u16,
    http: Option<Child>,
}
impl Instance {
    fn cli(&self, args: &[&str]) -> std::process::Output {
        Command::new(BIN)
            .arg("--home")
            .arg(&self.home)
            .arg("--port")
            .arg(self.port.to_string())
            .args(args)
            .output()
            .unwrap()
    }
    fn start(&mut self) {
        self.http = Some(
            Command::new(BIN)
                .args(["serve", "--home"])
                .arg(&self.home)
                .arg("--port")
                .arg(self.port.to_string())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = Instant::now() + Duration::from_secs(12);
        loop {
            if self.home.join("token").exists()
                && self
                    .request("GET", "/api/status", None, true)
                    .is_ok_and(|(s, _)| s == 200)
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "HTTP did not start; check local socket policy"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
    fn stop_http(&mut self) {
        if let Some(mut child) = self.http.take() {
            child.kill().unwrap();
            child.wait().unwrap();
        }
    }
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        auth: bool,
    ) -> std::io::Result<(u16, Value)> {
        let mut s = TcpStream::connect_timeout(
            &([127, 0, 0, 1], self.port).into(),
            Duration::from_secs(1),
        )?;
        s.set_read_timeout(Some(Duration::from_secs(5)))?;
        let body = body.map(|v| v.to_string()).unwrap_or_default();
        let auth = if auth {
            format!(
                "Authorization: Bearer {}\r\n",
                fs::read_to_string(self.home.join("token"))?
            )
        } else {
            String::new()
        };
        write!(s,"{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\n{auth}Content-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",self.port,body.len())?;
        let mut bytes = Vec::new();
        s.take(2 * 1024 * 1024).read_to_end(&mut bytes)?;
        let split = bytes
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("HTTP headers");
        let header = String::from_utf8_lossy(&bytes[..split]);
        let status = header.split_whitespace().nth(1).unwrap().parse().unwrap();
        let raw = &bytes[split + 4..];
        let body = if header.to_lowercase().contains("transfer-encoding: chunked") {
            let mut result = Vec::new();
            let mut left = raw;
            loop {
                let n = left
                    .windows(2)
                    .position(|w| w == b"\r\n")
                    .expect("chunk size");
                let size = usize::from_str_radix(
                    std::str::from_utf8(&left[..n])
                        .unwrap()
                        .split(';')
                        .next()
                        .unwrap(),
                    16,
                )
                .unwrap();
                left = &left[n + 2..];
                if size == 0 {
                    break;
                }
                result.extend_from_slice(&left[..size]);
                left = &left[size + 2..];
            }
            result
        } else {
            raw.to_vec()
        };
        Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
    }
}
impl Drop for Instance {
    fn drop(&mut self) {
        let _ = self.cli(&["stop", "--shutdown-sessions"]);
        if let Some(mut c) = self.http.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}
#[test]
#[ignore = "requires explicit AGENTBOX_ALLOW_SOCKET_TESTS=1 on a permitted Linux or macOS machine"]
fn http_restart_mcp_and_live_state() {
    assert_eq!(
        std::env::var("AGENTBOX_ALLOW_SOCKET_TESTS").as_deref(),
        Ok("1"),
        "Read docs/VERIFICATION.md and explicitly enable socket tests"
    );
    let d = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut i = Instance {
        home: d.path().join("state"),
        port,
        http: None,
    };
    i.start();
    assert_eq!(i.request("GET", "/api/status", None, false).unwrap().0, 401);
    let status = i.request("GET", "/api/status", None, true).unwrap().1;
    let pid = status["broker_pid"].clone();
    let (status, s) = i
        .request(
            "POST",
            "/api/sessions",
            Some(json!({"name":"e2e","cwd":"/tmp"})),
            true,
        )
        .unwrap();
    assert_eq!(status, 201);
    let id = s["id"].as_str().unwrap().to_owned();
    let output = i.cli(&[
        "run",
        &id,
        "export AB_E2E=persistent; cd /",
        "--request-id",
        "state",
    ]);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    i.stop_http();
    let output = i.cli(&[
        "run",
        &id,
        "printf '%s' \"$AB_E2E\"",
        "--request-id",
        "during-restart",
    ]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["output"]
            .as_str()
            .unwrap()
            .contains("persistent")
    );
    i.start();
    assert_eq!(
        i.request("GET", "/api/status", None, true).unwrap().1["broker_pid"],
        pid
    );
    let s = i
        .request("GET", &format!("/api/sessions/{id}"), None, true)
        .unwrap()
        .1;
    assert_eq!(s["cwd"], "/");
    let large = i.cli(&[
        "run",
        &id,
        "printf '%65536s' '' | tr ' ' x",
        "--request-id",
        "large-output",
    ]);
    assert!(
        large.status.success(),
        "Large RPC stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&large.stdout),
        String::from_utf8_lossy(&large.stderr)
    );
    let large: Value = serde_json::from_slice(&large.stdout).unwrap();
    assert_eq!(
        large["output"].as_str().unwrap().matches('x').count(),
        65536
    );
    let output = i.cli(&[
        "run",
        &id,
        "read -r x; printf 'interactive:%s' \"$x\"",
        "--no-wait",
        "--request-id",
        "interactive",
    ]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let command: Value = serde_json::from_slice(&output.stdout).unwrap();
    i.stop_http();
    i.start();
    assert!(i
        .cli(&["input", &id, "reply", "--newline"])
        .status
        .success());
    let log = i.cli(&["logs", &id]);
    assert!(
        log.status.success(),
        "Event RPC failed: {}\nBroker log: {}",
        String::from_utf8_lossy(&log.stderr),
        fs::read_to_string(i.home.join("broker.log")).unwrap_or_default()
    );
    let output = i.cli(&["wait", &id, command["id"].as_str().unwrap()]);
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        serde_json::from_slice::<Value>(&output.stdout).unwrap()["output"]
            .as_str()
            .unwrap()
            .contains("interactive:reply")
    );
    let mut mcp = Command::new(BIN)
        .arg("mcp")
        .arg("--home")
        .arg(&i.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = mcp.stdin.take().unwrap();
    let mut output = BufReader::new(mcp.stdout.take().unwrap());
    writeln!(input,"{}",json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"e2e","version":"1"}}})).unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert!(serde_json::from_str::<Value>(&line).unwrap()["result"].is_object());
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","id":2,"method":"tools/list"})
    )
    .unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["result"]["tools"]
            .as_array()
            .unwrap()
            .len(),
        9
    );
    writeln!(input,"{}",json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"agentbox_sessions","arguments":{}}})).unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&line).unwrap()["result"]["isError"],
        false
    );
    let mut cursor = 0;
    let mut transcript = String::new();
    for request_id in 4..24 {
        writeln!(input,"{}",json!({"jsonrpc":"2.0","id":request_id,"method":"tools/call","params":{"name":"agentbox_read","arguments":{"session_id":id,"after":cursor,"max_output_bytes":65536}}})).unwrap();
        line.clear();
        output.read_line(&mut line).unwrap();
        let response: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(response["result"]["isError"], false);
        let payload: Value =
            serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap())
                .unwrap();
        assert_eq!(payload["output_truncated"], false);
        let page = &payload["result"];
        transcript.push_str(page["output"].as_str().unwrap());
        cursor = page["after"].as_i64().unwrap();
        if page["has_more"] == false {
            break;
        }
    }
    assert!(transcript.contains("interactive:reply"));
    drop(input);
    assert!(mcp.wait().unwrap().success());
    assert_eq!(
        i.request("DELETE", &format!("/api/sessions/{id}"), None, true)
            .unwrap()
            .0,
        200
    );
}

#[test]
#[ignore = "requires explicit AGENTBOX_ALLOW_SOCKET_TESTS=1 on a permitted Linux or macOS machine"]
fn rpc_drains_closed_peer_and_enforces_deadline() {
    use std::os::unix::net::UnixListener;
    assert_eq!(
        std::env::var("AGENTBOX_ALLOW_SOCKET_TESTS").as_deref(),
        Ok("1")
    );
    let temp = tempfile::tempdir().unwrap();
    let socket = UnixListener::bind(temp.path().join("broker.sock")).unwrap();
    let payload = "x".repeat(32768);
    let expected = payload.clone();
    let server = std::thread::spawn(move || {
        for _ in 0..24 {
            let (mut stream, _) = socket.accept().unwrap();
            let mut request = String::new();
            BufReader::new(&stream).read_line(&mut request).unwrap();
            writeln!(stream, "{}", json!({"ok":true,"result":payload})).unwrap();
            // Drop immediately, leaving several client buffer-sized chunks unread.
        }
        let (mut stream, _) = socket.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        stream.write_all(b"{\"ok\":true").unwrap();
        drop(stream);
        let (stream, _) = socket.accept().unwrap();
        BufReader::new(&stream)
            .read_line(&mut String::new())
            .unwrap();
        std::thread::sleep(Duration::from_millis(150));
    });
    let client = agentbox::rpc::RpcClient {
        home: temp.path().to_owned(),
        token: "test-token".into(),
    };
    for _ in 0..24 {
        assert_eq!(client.call("status", json!({})).unwrap(), expected);
    }
    let incomplete = client.call("status", json!({})).unwrap_err();
    assert_eq!(incomplete.code, 502);
    assert!(incomplete.message.contains("disconnected"));
    let start = Instant::now();
    let timeout = client
        .call_timeout("status", json!({}), Duration::from_millis(30))
        .unwrap_err();
    assert_eq!(timeout.code, 504);
    assert!(start.elapsed() < Duration::from_secs(1));
    server.join().unwrap();
}

#[test]
#[ignore = "requires explicit AGENTBOX_ALLOW_SOCKET_TESTS=1 on a permitted Linux or macOS machine"]
fn old_broker_never_receives_native_create_mutation() {
    use std::os::unix::net::UnixListener;
    assert_eq!(
        std::env::var("AGENTBOX_ALLOW_SOCKET_TESTS").as_deref(),
        Ok("1")
    );
    let temp = tempfile::tempdir().unwrap();
    let socket = UnixListener::bind(temp.path().join("broker.sock")).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = socket.accept().unwrap();
        let mut request = String::new();
        BufReader::new(&stream).read_line(&mut request).unwrap();
        let request: Value = serde_json::from_str(&request).unwrap();
        assert_eq!(request["method"], "status");
        assert_eq!(request["params"], json!({}));
        writeln!(stream, "{}", json!({"ok":true,"result":{"version":"old"}})).unwrap();
        socket
    });
    let client = agentbox::rpc::RpcClient {
        home: temp.path().to_owned(),
        token: "fixture".into(),
    };
    let error = client
        .call(
            "create",
            json!({"shell":"/bin/zsh","environment":{"PRIVATE":"not-in-status"}}),
        )
        .unwrap_err();
    assert_eq!(error.code, 409);
    assert!(error.message.contains("No session was created"));
    let socket = server.join().unwrap();
    socket.set_nonblocking(true).unwrap();
    assert_eq!(
        socket.accept().unwrap_err().kind(),
        std::io::ErrorKind::WouldBlock
    );
}

#[test]
#[ignore = "requires explicit AGENTBOX_ALLOW_SOCKET_TESTS=1 on a permitted Linux or macOS machine"]
fn native_shell_creation_through_http_and_mcp() {
    assert_eq!(
        std::env::var("AGENTBOX_ALLOW_SOCKET_TESTS").as_deref(),
        Ok("1")
    );
    let temp = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut instance = Instance {
        home: temp.path().join("state"),
        port,
        http: None,
    };
    instance.start();
    let request = json!({"name":"native-api","shell":"/bin/bash","no_rc":true,"environment":{"HOME":temp.path(),"PATH":"/usr/bin:/bin","API_ENV":"shared value"}});
    let (status, session) = instance
        .request("POST", "/api/sessions", Some(request.clone()), true)
        .unwrap();
    assert_eq!(status, 201, "{session}");
    assert_eq!(session["shell"]["mode"], "native");
    assert_eq!(session["status"], "interactive");
    let mut child = Command::new(BIN)
        .args(["mcp", "--home"])
        .arg(&instance.home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    let mut line = String::new();
    writeln!(input,"{}",json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"native-api-test","version":"1"}}})).unwrap();
    output.read_line(&mut line).unwrap();
    assert!(serde_json::from_str::<Value>(&line).unwrap()["result"].is_object());
    writeln!(
        input,
        "{}",
        json!({"jsonrpc":"2.0","method":"notifications/initialized"})
    )
    .unwrap();
    writeln!(input,"{}",json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"agentbox_create","arguments":request}})).unwrap();
    line.clear();
    output.read_line(&mut line).unwrap();
    let response: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(response["result"]["isError"], false, "{response}");
    let content: Value =
        serde_json::from_str(response["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    let mcp_session = &content["result"];
    assert_eq!(mcp_session["shell"]["mode"], "native");
    for session in [&session, mcp_session] {
        let id = session["id"].as_str().unwrap();
        let (status, _) = instance
            .request(
                "POST",
                &format!("/api/sessions/{id}/input"),
                Some(json!({"data":"printf 'API:%s\\n' \"$API_ENV\"\n"})),
                true,
            )
            .unwrap();
        assert_eq!(status, 200);
        let until = Instant::now() + Duration::from_secs(5);
        loop {
            let result = instance.cli(&["read", id]);
            assert!(result.status.success());
            let result: Value = serde_json::from_slice(&result.stdout).unwrap();
            if result["output"]
                .as_str()
                .unwrap()
                .contains("API:shared value")
            {
                break;
            }
            assert!(Instant::now() < until, "native API output missing");
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(!instance.cli(&["run", id, "true"]).status.success());
        let (_, detail) = instance
            .request("GET", &format!("/api/sessions/{id}"), None, true)
            .unwrap();
        assert!(detail.get("environment").is_none());
        assert_eq!(
            instance
                .request("DELETE", &format!("/api/sessions/{id}"), None, true)
                .unwrap()
                .0,
            200
        );
    }
    drop(input);
    assert!(child.wait().unwrap().success());
}
