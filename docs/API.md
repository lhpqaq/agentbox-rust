# Rust HTTP API

The server binds only `127.0.0.1`, on the configured nonzero port. `/health` and bundled static assets are public, but all `/api/` endpoints require `Authorization: Bearer <private token>`. Host must be loopback with the exact configured port. Browser Origin, when present, must exactly match `http://<Host>`; cross-site fetches are rejected. There is no CORS allowance. Never expose this API through a public proxy or share its token: commands run as the broker's local user, without sandboxing.

JSON bodies are limited to 300 KiB, reject unknown fields, invalid types, duplicate keys, and out-of-range values. Input supports only `source: "user"`; terminal-generated device replies are intentionally disabled. Binary tokens are never logged. Responses include no-store, nosniff, frame denial and a restrictive Content Security Policy. Static assets are compiled into the binary and selected from an explicit allowlist; filesystem paths are never served.

| Method | Path | Broker operation |
|---|---|---|
| GET | `/api/status` | status |
| GET / POST | `/api/sessions` | list / create |
| GET / PATCH / DELETE | `/api/sessions/{id}` | detail / rename / close |
| POST | `/api/sessions/{id}/commands` | run once |
| GET | `/api/sessions/{id}/commands/{command_id}` | durable command lookup |
| GET | `/api/sessions/{id}/history` | retained history |
| GET | `/api/sessions/{id}/log?after=0&limit=1000` | retained events |
| GET | `/api/sessions/{id}/events?after=0` | SSE |
| POST | `/api/sessions/{id}/input` | raw input |
| POST | `/api/sessions/{id}/interrupt` | interrupt foreground process |
| POST | `/api/sessions/{id}/resize` | resize PTY |
| POST | `/api/shutdown` | stop HTTP; optional `shutdown_sessions: true` stops broker too |

Create accepts name (1–80 characters), cwd, rows (2–300), cols (10–500). Run accepts a nonblank command and optional request_id (1–128 ASCII letters, digits, `. _ : -`). Reusing a key with the identical command returns the existing record, not a new execution. Different content with that key conflicts. This does not guarantee exactly-once external side effects.

SSE begins with an unnumbered `stream_sync` JSON event containing the session, commands, first_seq, and replay_through snapshot watermark. Subsequent events retain broker sequence IDs and original event types/data. `Last-Event-ID` and `after` are combined using the greater cursor. Retention may remove old events; the UI must compare first_seq and its cursor, and apply the snapshot at the replay watermark. Idle streams keep alive every ten seconds. Broker work runs outside the Tokio executor on blocking workers. Shutdown terminates streams; ordinary HTTP shutdown or Ctrl-C preserves the detached broker and live shells.

## Migration notes

The path/body/event contract follows the Python version. Host validation is tighter: a matching explicit port is required. HTTP body limit is now 300 KiB. Automatic terminal device replies are disabled. Errors use HTTP status and a JSON `detail` field; unknown paths return 404. Do not retry a failed mutating request blindly: transport failure can leave execution uncertain. Inspect command/history or reuse a known submission key where appropriate.

## Platform support

This backend supports Linux and macOS. Platform-specific PTY/process identity and filesystem helpers are tested on both through CI. Other operating systems are not supported. See VERIFICATION.md for test commands and coverage limits.

The 256-KiB command and 64-KiB input limits apply to decoded UTF-8 text; the entire serialized HTTP body must also fit 300 KiB. JSON escaping and metadata count toward the body limit. A string below its decoded limit can therefore still be rejected as too large. The broker RPC envelope has a separate 400-KiB limit.


### Native shell creation

`POST /api/sessions` also accepts `shell` (Bash/Zsh executable), `login` and
`no_rc` (booleans), and `environment` (complete string-to-string environment
snapshot, maximum 128 KiB / 2048 entries). Without `environment`, the daemon's
inherited environment is used. Values are transient, not journaled. Returned
`shell` metadata reports mode, executable, startup policy, environment source,
structured command support and whether cwd is tracked. Native sessions start as
`interactive`, use input/read, and reject structured run. They are not sandboxes.
