# Python → Rust migration and parity

This is a separate implementation and separate state directory, not an in-place database migration. Preserve your existing Python installation and state. Do not point the Rust executable at the Python home: schema names overlap but formats and contracts are not guaranteed compatible. No existing Python files or runtime state were changed while producing this source package.

| Area | Rust source implementation | Difference / remaining verification |
|---|---|---|
| Shell | Persistent Bash process attached to PTY | Linux only; all Rust execution still unverified |
| State | cd/export/functions shared between structured commands | Shell instrumentation can be modified by same-privilege commands |
| Command control | Private FD with ID + parent PID checks | DONE fallback completes once; missing prompt prevents reuse |
| Lifecycle | Detached broker + replaceable HTTP | Socket end-to-end tests not run |
| Input | Raw keyboard data, resize, explicit interrupt | Any ordinary raw input permanently disables structured reuse |
| UI | Existing HTML/CSS/JS + vendored xterm embedded at build | Browser tests not run; device replies disabled |
| History | SQLite event/command/request records | New home required; no migration from Python DB |
| Idempotency | Exact command hash per session/request key | At-most-once submission, not transactional external execution |
| Retention | 20,000 events / 8 MiB event payload per session; 500 finished commands / 4 MiB metadata | Retains active command separately; pruning runs inside journal transactions |
| Limits | 24 live sessions, 256 total retained sessions/home, 10,000 request keys/session | No archived-session deletion command; choose new home for genuinely new work |
| CLI | Rust executable directly uses Unix socket | No Python SDK; no --url; outputs JSON; failures return nonzero |
| MCP | Same eight tool names and newline JSON-RPC | Direct Unix broker instead of HTTP SDK; actual host acceptance unverified |
| Remote use | Can run executable on remote Linux computer | No remote CLI URL, built-in SSH integration, reconnect guarantee, or tested SSH setup |

Data bounds apply to retained rows, not exact SQLite file size or a strict global RAM quota. SQLite may retain freed pages. A single currently active command and event may temporarily exceed configured test limits; defaults allow the maximum request size. MCP has its own smaller output cap.

Raw command limit is 256 KiB UTF-8, input is 64 KiB, no NUL. The serialized envelope also has a cap: HTTP 300 KiB, broker RPC 400 KiB, MCP 256 KiB. JSON escaping and metadata consume space, so some strings below the raw limit are still too large for a transport. Oversized requests are rejected, not silently split or retried.

The Rust package is source-only and uncompiled at delivery. Feature descriptions describe implementation intent, not a claim of complete compatibility or production readiness.
