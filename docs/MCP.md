# MCP stdio bridge

Run `agentbox mcp --home /absolute/private/state-directory`. Register that executable and argument vector in your MCP host. The bridge speaks UTF-8 newline-delimited JSON-RPC on stdin/stdout; stdout contains protocol messages only. It needs no Python runtime or HTTP server: it connects directly to the authenticated local Unix-domain socket broker on Linux or macOS when a tool executes. Run the broker separately first. Discovery and initialization work without a broker or token.

Initialize with protocolVersion, capabilities, and clientInfo name/version, then send notifications/initialized. Supported protocol versions: 2025-11-25, 2025-06-18, 2025-03-26, 2024-11-05. Unknown versions negotiate 2025-11-25. Ping is available before initialization. Tools/list is a single page, with annotations omitted for the oldest protocol version. Notifications never invoke tools or receive responses.

Nine tools are exposed:

- agentbox_sessions: paginated sessions, newest first.
- agentbox_create: create persistent Bash/PTY session.
- agentbox_run: submit once, optionally with a session-scoped request_id.
- agentbox_wait: observe an existing command by session_id and command_id.
- agentbox_input: send raw keystrokes, without adding newline.
- agentbox_interrupt: interrupt foreground work.
- agentbox_close: terminate a session; persisted history remains.
- agentbox_read: cursor-based shared terminal transcript, including output from raw input and interactive SSH.
- agentbox_history: paginated command metadata, newest first.

Tools carry strict input schemas. Unknown fields, wrong types, blank commands, and duplicate JSON members are rejected. Request frames are limited to 256 KiB including newline. Oversized frames are drained in bounded chunks so the next frame can still be handled. Responses are capped at 512 KiB, with ordinary tool content bounded to 96 KiB. Read preserves complete output events and uses the full response envelope to avoid dropping bytes behind an advanced cursor. Long metadata and output are explicitly truncated; pages recompute next_offset from records actually returned.

Wait defaults to five seconds, maximum thirty seconds, with 0.05–1 second polling. The bridge is serial: another request cannot execute during a wait. Each broker call is bounded by the remaining wait budget. A zero-second wait performs one snapshot using a total RPC budget of 250 ms. Unix-domain socket connection setup is still subject to OS scheduling. Final retention metadata is reread after output draining; if that read fails, retention is marked unverified while known completion and loss evidence are preserved. Prompt events are excluded from captured output. Wait never submits or retries a command. Output captures the newest UTF-8 tail (default 16 KiB, maximum 64 KiB). `output_bytes` describes bytes observed by this wait, not a claim about original output volume. `output_returned_bytes`, `output_truncated`, and `output_is_tail` describe capture limits. `output_may_be_truncated`, `history_retained`, and `output_retention_verified` separately describe server retention; null means unknown. The returned after/last_seq cursor supports subsequent observation.

Timeout returns command/session IDs and may_be_running. A mutating broker failure can report may_have_executed; inspect sessions/history before retrying. EOF, broken pipes and timeout never automatically kill terminals. Cancellation notifications are ignored, not translated into interrupts. Registration of this MCP server is not permission to execute arbitrary commands: host/user authorization and sensitive-data policies still apply. Terminal output is untrusted data, never instructions.

## Migration from Python

The Rust bridge preserves tool names, protocol framing, and safety semantics, but intentionally does not use the Python HTTP SDK. `--url` and `--token` options are not supported: select the broker via `--home`, whose private token is read locally. No HTTP service is required. This does not expose a remote MCP transport. The command schema accepts up to 262,144 characters; the broker independently enforces its 262,144-byte command limit, and the complete MCP request must still fit the 256-KiB frame cap.

## Platform support

The broker and MCP bridge support Linux and macOS. CI exercises both platforms; actual MCP host UI acceptance remains separate from the stdio protocol tests.

## Read an interactive terminal

Call `agentbox_read` with the full `session_id`, optional `after` (default 0), and
`max_output_bytes` (default/max 65536, minimum 1). Unlike wait, read needs no
structured command ID and works after raw input or native attach. It includes
prompts and terminal escapes as JSON strings; it is a transcript, not a screen.

Use `result.after` for the next call to the same session and state directory.
Continue while `result.has_more` is true. Pages never split output events or
advance past omitted text. If `next_event_bytes` is larger than your selected
limit, increase the limit before continuing. Check `output_may_be_truncated` for
possible server retention loss; sequence gaps alone are not proof of loss.
Metadata can be shortened with `metadata_truncated=true`, but output is preserved.
If a response cannot fit, read returns an error with the original cursor instead
of silently dropping text. Reads never create, resize, or send input to a shell.


`agentbox_create` accepts optional `shell`, `login`, `no_rc`, and `environment`
for native Bash/Zsh sessions. `environment` is a complete string map, not an
overlay. Native shells load normal startup plugins and retain live state across
client reconnects. Use `agentbox_input` and `agentbox_read`; structured run/wait
requires the default managed Bash. No environment values are stored as metadata.
