# AgentBox Rust — Linux and macOS

This is a Linux and macOS adaptation of `AgentBox-Rust-source-unverified.zip`.
See [Linux verification](docs/LINUX.md) for the Linux checks and setup.
See [macOS verification](docs/MACOS.md) for actual checks and limitations.
The [original package README](docs/UPSTREAM-README.md) is retained for provenance;
its Linux-only and unverified statements describe the original source package.

## Use the CLI on Linux or macOS

```sh
cd agentbox-rust
cargo build --locked
AB=./target/debug/agentbox
AB_HOME="$PWD/.macos-demo"
$AB start --home "$AB_HOME"
SESSION_ID=$($AB create work --home "$AB_HOME" --cwd "$PWD")
$AB run "$SESSION_ID" 'export DEMO_VALUE=hello; pwd' --home "$AB_HOME"
$AB run "$SESSION_ID" 'printf "%s\n" "$DEMO_VALUE"' --home "$AB_HOME"
$AB history "$SESSION_ID" --home "$AB_HOME"
$AB close "$SESSION_ID" --home "$AB_HOME"
```

CLI and MCP connect directly to the broker's Unix socket; no browser, HTTP
server or manual token entry is required. To shut down the broker and sessions
without targeting a real HTTP port:

```sh
$AB stop --home "$AB_HOME" --port 0 --shutdown-sessions
```

In broker-only mode, this reports that HTTP stop was not confirmed, because no
HTTP server is running; the broker shutdown request is still sent successfully.

To rerun the isolated CLI scenarios:

```sh
python3 scripts/test-macos-cli.py
```

The application runs with your account permissions; it does not provide isolation.
macOS process cleanup uses session ID plus process start time to reduce PID-reuse
risk; unlike Linux pidfds, it cannot atomically bind a signal to an identity.
Deliberately daemonized processes can escape the session on both platforms.

## Features and validation

- Persistent Bash/PTY sessions with independent working directories, variables and functions.
- Detached broker, SQLite command history, bounded output replay and request ID deduplication.
- CLI and stdio MCP interfaces; an optional embedded web UI.
- On macOS arm64: 50 Rust tests and seven pure CLI scenarios passed, along with fmt, clippy and six frontend test files. A live socket/MCP integration test also passed.
- On Ubuntu 22.04 x86_64: 50 Rust tests, seven CLI scenarios, the live HTTP/MCP integration test, fmt, clippy and six frontend test files passed. See [Linux verification](docs/LINUX.md). Browser UI acceptance testing is incomplete.
- Nonempty raw input permanently disables later structured `run` in that session; create a new session to resume structured execution.

## Documentation

- [CLI quick start](开始使用.md)
- [macOS changes and verification](docs/MACOS.md)
- [MCP tools](docs/MCP.md)
- [HTTP API](docs/API.md)
- [Security boundaries](docs/SECURITY.md)
- [Original verification record](docs/VERIFICATION.md)

## License

MIT; original copyright and bundled dependency notices are preserved.
