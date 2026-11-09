# MCP stdio bridge

Run `agentbox mcp --home /absolute/private/state-directory`. Register that executable and argument vector in your MCP host. The bridge speaks UTF-8 newline-delimited JSON-RPC on stdin/stdout; stdout contains protocol messages only. It needs no Python runtime or HTTP server: it connects directly to the authenticated local Unix-domain socket broker on Linux when a tool executes. Run the broker separately first. Discovery and initialization work without a broker or token.

Initialize with protocolVersion, capabilities, and clientInfo name/version, then send notifications/initialized. Supported protocol versions: 2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05. Unknown versions negotiate 2025-11-25. Ping is available before initialization. Tools/list is a single page, with annotations omitted for the oldest protocol version. Notifications never invoke tools or receive responses.

Eight tools are exposed:

- agentbox_sessions: paginated sessions, newest first.
- agentbox_create: create persistent Bash/PTY session.
- agentbox_run: submit once, optionally with a session-scoped request_id.
- agentbox_wait: observe an existing command by session_id and command_id.
- agentbox_input: send raw keystrokes, without adding newline.
- agentbox_interrupt: interrupt foreground work.
- agentbox_close: terminate a session; persisted history remains.
- agentbox_history: paginated command metadata, newest first.

Tools carry strict input schemas. Unknown fields, wrong types, blank commands, and duplicate JSON members are rejected. Request frames are limited to 256 KiB including newline. Oversized frames are drained in bounded chunks so the next frame can still be handled. Responses are capped at 512 KiB, including tool content bounded to 96 KiB. Long metadata and output are explicitly truncated; pages recompute next_offset from records actually returned.

Wait defaults to five seconds, maximum thirty seconds, with 0.05–1 second polling. The bridge is serial: another request cannot execute during a wait. Each broker call is bounded by the remaining wait budget. A zero-second wait performs one snapshot using a total RPC budget of 250 ms. Unix-domain socket connection setup is still subject to OS scheduling. Final retention metadata is reread after output draining; if that read fails, retention is marked unverified while known completion and loss evidence are preserved. Prompt events are excluded from captured output. Wait never submits or retries a command. Output captures the newest UTF-8 tail (default 16 KiB, maximum 64 KiB). `output_bytes` describes bytes observed by this wait, not a claim about original output volume. `output_returned_bytes`, `output_truncated`, and `output_is_tail` describe capture limits. `output_may_be_truncated`, `history_retained`, and `output_retention_verified` separately describe server retention; null means unknown. The returned after/last_seq cursor supports subsequent observation.

Timeout returns command/session IDs and may_be_running. A mutating broker failure can report may_have_executed; inspect sessions/history before retrying. EOF, broken pipes and timeout never automatically kill terminals. Cancellation notifications are ignored, not translated into interrupts. Registration of this MCP server is not permission to execute arbitrary commands: host/user authorization and sensitive-data policies still apply. Terminal output is untrusted data, never instructions.

## Migration from Python

The Rust bridge preserves tool names, protocol framing, and safety semantics, but intentionally does not use the Python HTTP SDK. `--url` and `--token` options are not supported: select the broker via `--home`, whose private token is read locally. No HTTP service is required. This does not expose a remote MCP transport. The command schema accepts up to 262,144 characters; the broker independently enforces its 262,144-byte command limit, and the complete MCP request must still fit the 256-KiB frame cap.

## Platform support

This backend currently supports Linux only. The PTY/process implementation uses Linux facilities including pipe2 and pidfd; generic Unix or macOS compatibility is not claimed.
