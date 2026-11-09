# Verification record

Date: 2026-11-07. This record describes this Rust source package, not earlier Python validation.

## Actually executed

- Existing source inspected: Python broker/PTY, storage, API/MCP, CLI, browser modules and original regression tests.
- Independent static source review; corrections made to completion/UI statuses, raw Ctrl-C semantics, missing-prompt completion, retention size/atomicity, wait deadlines/completeness, token publication/file checks, spawn classification and process cleanup.
- Six web/test-*.mjs files passed against the included JS and vendored xterm. See `frontend-test-results.txt` for actual output. This covers pure/mock/parser behavior, not a real browser running the server.
- Archive file inventory and CRC integrity are checked when packaging.

## Not executed

- `cargo fmt`, dependency resolution, `cargo check`, Rust compilation, Rust unit/integration tests, `cargo clippy`, release build.
- Rust PTY or database runtime behavior. Tests exist but have not run.
- Unix socket or live TCP HTTP tests, HTTP restart preservation, actual SSE stream, actual MCP host integration.
- Browser rendering/accessibility/interaction on a running app; macOS, Windows/WSL2 or SSH.

Reason: Rust compiler/Cargo are absent from the managed build environment. No toolchain was downloaded or installed while permission was pending. Socket creation was restricted in the original environment; the Rust rewrite has not attempted to bypass that restriction. Passing JavaScript tests must not be represented as passing Rust backend checks.

## First verification on an authorized Linux machine

Install/use the current stable Rust toolchain through your trusted official process, with rustfmt and clippy, and a C compiler. This package never installs tools automatically. Then:

```sh
cargo fmt
./scripts/verify.sh
cargo build --release
```

The default tests do not open sockets. They include real Bash/PTY tests, SQLite transaction/recovery tests, config permissions, direct Core tests, offline HTTP router tests and MCP protocol/observation tests. They can start harmless local Bash commands and write temporary files; use a permitted test environment. Node is needed only for frontend tests. Source is not yet rustfmt-formatted, so format before running the fmt check. The code may need compile/test corrections; it is not a verified release.

After a successful dependency resolution, retain Cargo.lock, then use `cargo ... --locked` for reproducibility. No resolved lockfile is included in this uncompiled source draft.

## Optional live integration test

Only on a Linux machine where local sockets and PTYs are allowed, explicitly enable:

```sh
AGENTBOX_ALLOW_SOCKET_TESTS=1 cargo test --test local_e2e -- --ignored --test-threads=1
```

The ignored test creates a disposable private home, starts its own broker/HTTP process on an ephemeral loopback port, verifies HTTP authentication, creates a test shell, checks state while HTTP is down, restarts HTTP, resumes a running `read`, checks real MCP stdio discovery/listing and closes the session. Cleanup is scoped to that disposable instance. It does not target your normal home. **This test has never been run here.**

Before relying on the UI, manually check unlock/lock, create/rename/close, duplicate clicks, command failure, Ctrl-C, raw input, resize, long output, stream disconnect/reconnect, browser refresh, session switching during output, failed submissions without automatic retries, and refusal of unsafe terminal links. Verify the actual MCP host's approval UI separately.
