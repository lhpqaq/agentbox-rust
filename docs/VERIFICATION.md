# Verification

Updated: 2026-11-12. This file describes the adapted Linux/macOS project.
The original uncompiled-package record is preserved in
[UPSTREAM-VERIFICATION.md](UPSTREAM-VERIFICATION.md).

## Current evidence

- Local macOS: formatting, all-target checking, Rust tests, clippy with warnings
  denied, frontend parser/mock tests, and source-packaging regression pass.
- Disposable broker CLI tests cover persistent shell state, request deduplication,
  names/ID prefixes, ambiguous selection refusal, diagnostics, exit codes,
  output paging, interruption, raw input and broker restart.
- Native PTY tests cover consecutive prompts, shared keyboard/environment,
  read-only observation, resize, Ctrl-C, query filtering, detach/reattach and
  local terminal restoration.
- Opt-in live tests cover HTTP authentication and restart, MCP stdio discovery
  and shared transcript reads, and closed-peer RPC buffering/deadlines.
- The 9 MiB bulk-output test verifies command completion, observed byte count,
  bounded returned tail and server-retention warnings. A local debug run took
  about 14 seconds; this is not a general performance guarantee.
- [Linux and macOS CI for 24fb766](https://github.com/lhpqaq/agentbox-rust/actions/runs/43240848428)
  passed, including the bulk-output and native PTY checks and release builds.
  Later commits must be assessed using their own CI result.

## Reproduce

Prerequisites: a supported Linux or macOS host, Rust/Cargo with rustfmt and clippy,
a C compiler, Bash, Python 3, Git and Node.js. No script installs dependencies.
Use the committed Cargo.lock.

```sh
bash scripts/verify.sh
cargo build --locked
python3 scripts/test-macos-cli.py
python3 scripts/test-attach.py
python3 scripts/test-bulk-output.py
AGENTBOX_ALLOW_SOCKET_TESTS=1 cargo test --locked --test local_e2e -- --ignored --test-threads=1
cargo build --locked --release
bash scripts/package.sh
```

The default verification starts harmless local Bash/PTY processes and uses
SQLite/temp files, but does not open sockets. The remaining commands use their
own temporary private homes and local sockets. They never target the user's
normal AgentBox home. `test-macos-cli.py` retains its historical name but runs
on both supported platforms. Packaging requires clean committed tracked files;
it excludes untracked and ignored files and does not imply runtime validation.

## Coverage limits

Automated frontend tests cover parser, input provenance, replay and retention
logic. They do not establish real-browser visual or accessibility acceptance.
Actual MCP host approval UI, Windows/WSL-specific behavior, alternate Linux
architectures/distributions and remote SSH server administration are not covered
by this suite. Shared-terminal tests use local shells and do not connect to a
user's production SSH session.

AgentBox is not a sandbox. A broker/machine restart does not restore running
processes. Raw input permanently disables structured run for that session.
Output retention is finite. Native attach streams a transcript rather than
reconstructing a full screen and filters terminal device/clipboard controls.
