# Changelog

## 0.3.0 — 2026-11-12

### Shared terminal workflows

- Native `attach` shares a persistent Bash terminal with human and agent operators.
  Ctrl-] detaches, reattach preserves the live shell, and local terminal settings
  are restored on exit. Consecutive prompts work with macOS Bash 3.2.
- `attach --read-only` receives output without sending keystrokes or resizing.
- `read` and MCP `agentbox_read` provide bounded, cursor-based transcript pages
  for interactive sessions, including SSH output. Complete output events and
  cursor advancement stay aligned; retention uncertainty is explicit.
- New brokers return transcript pages in a single journal snapshot, omitting
  command history. Clients retain a read-only fallback for older running brokers.
- Session arguments accept exact names and unique ID prefixes. Ambiguity fails
  without selecting a shell. `sessions --active --table` gives a readable list.
- `doctor` diagnoses local configuration and broker connectivity without starting
  or modifying anything. Token values are never displayed.
- `stop --shutdown-sessions --broker-only` skips HTTP shutdown.

### Reliability and delivery

- Native output filtering handles UTF-8 C1 control forms and zero-padded focus
  reporting modes while preserving ordinary Unicode and color output.
- macOS RPC reads drain buffered responses after peer closure without the
  SO_RCVTIMEO/EINVAL race. Reads use a nonblocking socket and absolute deadline;
  mutations are never automatically retried.
- Cleanup drains bounded pending PTY output while verifying child reaping, fixing
  an immediate-input/close race reproduced in macOS CI.
- Event retention uses a covering SQLite index. Observation errors distinguish
  incomplete reads from timeouts and preserve known command completion.
- CI verifies Linux and macOS: Rust/frontend checks, CLI and native PTY tests,
  live HTTP/MCP/RPC tests, 9 MiB output, concurrent clients and release builds.
- Source packages use clean committed Git content, preserve Cargo.lock and file
  modes, and include revision metadata and SHA-256 checksums. Local state and
  untracked notes are excluded.

### Upgrade notes

Replacing or installing the executable does not restart an existing broker.
Client-only features and compatible reads can be used while the old broker keeps
its shells running. Broker-side fixes take effect after a deliberate restart;
finish live work first because restarting terminates shells. Doctor reports
version differences. No Python-state migration or process checkpointing is added.

AgentBox still runs with local-user privileges, has bounded transcript retention,
and disables structured run after raw input. Native attach is a filtered output
stream, not full-screen state reconstruction.
