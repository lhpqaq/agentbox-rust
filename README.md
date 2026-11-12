# AgentBox Rust — Linux and macOS

AgentBox keeps Bash terminals running in a local broker so a person and an agent
can view and operate the same shell through a native terminal, CLI or MCP.
This repository adapts `AgentBox-Rust-source-unverified.zip` for Linux and macOS.
See [Linux verification](docs/LINUX.md) for the Linux checks and setup.
See [macOS verification](docs/MACOS.md) for actual checks and limitations.
The [original package README](docs/UPSTREAM-README.md) is retained for provenance;
its Linux-only and unverified statements describe the original source package.

## Shared interactive terminal

```sh
./target/debug/agentbox attach --home "$PWD/.macos-demo"
```

This creates a shell and prints its session ID. Type normally in your terminal;
another operator or agent can inspect `agentbox logs SESSION_ID --follow` and
send keystrokes with `agentbox input SESSION_ID 'command text' --newline`, using
the same `--home`. Both operators control the same shell and must take turns typing.

Session arguments accept a full ID, an exact session name, or a unique ID prefix
of at least six characters. Ambiguous selectors fail without running the command;
closed sessions also count when checking ambiguity. Use the full ID to disambiguate.

For a bounded JSON snapshot of an interactive shell (including SSH output), use:

```sh
./target/debug/agentbox read SESSION_ID --home "$PWD/.macos-demo" --max-bytes 65536
./target/debug/agentbox read SESSION_ID --home "$PWD/.macos-demo" --after CURSOR
```

The returned `after` is the next call's cursor. Continue while `has_more` is true.
`output` contains terminal text, including ANSI escapes encoded safely in JSON;
it is a transcript, not a reconstructed terminal screen. Reads never send input.
Pages preserve complete output events; if `next_event_bytes` exceeds your limit,
increase `--max-bytes` (maximum 1 MiB) to make progress. Check
`output_may_be_truncated` for possible retention loss. Cursors belong to one session
and state directory; keep those alongside the cursor. A snapshot can be followed
by another read to observe output that arrived later.

For observation without controlling the shell, use
`agentbox attach SESSION_ID --read-only --home "$PWD/.macos-demo"`.
The observer receives output but cannot send keystrokes or resize the shared PTY.
This is a client mode for avoiding accidental input, not a separate access permission.

`Ctrl-]` detaches without closing the shell. Reconnect with
`agentbox attach SESSION_ID --home "$PWD/.macos-demo"`; type `exit` in the shell
to close it. Attaching to an existing session does not inject setup commands.
Omit the ID to create a shell with readline editing enabled. Existing shells
retain their own editing mode. Window size follows the attached terminal.

This streams retained terminal output, not a saved screen image; use `--no-replay`
for live output only. Terminal device queries, OSC/DCS and unknown CSI operations
are filtered to prevent automatic terminal replies or clipboard operations.
Programs needing those features may not work fully. Raw keyboard input uses the
broker's Unicode API; NUL is omitted and invalid UTF-8 is replaced. After raw
input, agents should continue using `input` with `read`/`logs` rather than structured `run`.

Native PTY checks: `python3 scripts/test-attach.py` (requires local Unix sockets).

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
$AB stop --home "$AB_HOME" --shutdown-sessions --broker-only
```

`--broker-only` does not contact HTTP. It acknowledges the shutdown request;
it is not a process-exit receipt.

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

## Diagnose an instance

```sh
$AB doctor --home "$AB_HOME"
```

Returns JSON with shell, private directory, token, socket, broker, version and
journal checks. Exit code is zero when all checks pass. It never starts a broker,
creates state, changes permissions or sends shell input; token values are omitted.
This diagnoses local broker connectivity, not the health of an SSH connection.

## Package committed source

```sh
bash scripts/package.sh
```

Requires Git and Python 3. The tracked worktree must be clean. The ZIP contains
committed `HEAD` content, `Cargo.lock`, executable file modes, a SHA-256 manifest
and the commit identity. Ignored and untracked local files are excluded. Output:
`dist/AgentBox-Rust-source.zip` and its `.sha256` file. Repeated packaging of the
same commit in the same environment produces identical bytes. Packaging checks
archive integrity; run the verification scripts to check behavior.
