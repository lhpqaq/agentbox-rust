# Linux verification

Verified on 2026-11-09 with Ubuntu 22.04.5 LTS x86_64, kernel
5.15.0-125-generic, Rust/Cargo 1.99.1, GCC 11.4 and Node 22.22.0.

Requirements: Rust with rustfmt/clippy, a C compiler (SQLite is bundled),
Bash, Zsh for native-shell/state tests, stty, Python 3 for CLI scenarios, and Node for frontend tests.
Use a host permitting local Unix/TCP sockets and PTYs.

## Checks

```sh
bash scripts/verify.sh
cargo build --locked
python3 scripts/test-macos-cli.py
AGENTBOX_ALLOW_SOCKET_TESTS=1 cargo test --locked --test local_e2e -- --ignored --test-threads=1
cargo build --locked --release
```

The default verification passed fmt, all-target compilation, 50 Rust tests
(46 unit tests and four Core integration tests), clippy with warnings denied,
and all six frontend test files. The locked release build and the release
binary's `--help` smoke check passed. The opt-in live integration test also passed:
HTTP authentication, HTTP restart while shell state persists, interactive input,
and MCP stdio discovery/listing. Seven separate CLI scenarios passed, covering
state persistence, request deduplication, exit status, 128 KiB output,
interrupts, raw input and broker shutdown/restart.

The CLI test keeps its historical filename but now uses the system temporary
directory instead of macOS-only `/private/tmp`. The PTY call accommodates the
different Linux/Darwin pointer signatures. Ubuntu CI repeats these checks.

## Run

Follow the CLI example in the root README. For a release binary, use
`./target/release/agentbox` instead of `./target/debug/agentbox`.

Real browser UI acceptance and other Linux architectures/distributions have
not been tested here. Frontend tests exercise mock/parser behavior, not browser
interaction. AgentBox executes with the current user's permissions and does
not provide sandbox isolation. Earlier source-only limitations in
`VERIFICATION.md` describe the original package, before this validation.
