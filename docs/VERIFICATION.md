# Verification

Updated: 2026-11-12. This file describes the adapted Linux/macOS project.
The original uncompiled-package record is preserved in
[UPSTREAM-VERIFICATION.md](UPSTREAM-VERIFICATION.md).

## Current evidence

- Native-shell/state work: [Linux and macOS CI for d0a0d4c](https://github.com/lhpqaq/agentbox-rust/actions/runs/43253228657) passed with pinned real Oh My Zsh, autosuggestions and syntax-highlighting fixtures, native Bash/Zsh reconnect, state import and HTTP/MCP native creation tests. Later commits require their own CI evidence.
- Local state tests additionally cover startup prompts during pending import, explicit success/failure metadata, closing a pending import, UTF-8 state under a C locale, virtual-environment PATH restoration, and invoking embedded shell integration without putting the binary on PATH.
- Plugin acceptance executes an actual ghost suggestion with an editing key, observes syntax colors and checks real completion/plugin bindings. Local fixture versions: autosuggestions 0.7.0 and syntax-highlighting 0.8.0; Oh My Zsh commit `67cd8c4673512d08238461f7ce00715da01665d4`. CI pins the corresponding source commits. All fixture runs use disposable HOME/cache/history directories and disable framework self-updates.

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
a C compiler, Bash, Zsh, Python 3, Git and Node.js. Local verification scripts do not install dependencies. CI explicitly installs Zsh and checks out pinned plugin fixtures.
Use the committed Cargo.lock.

```sh
bash scripts/verify.sh
cargo build --locked
python3 scripts/test-macos-cli.py
python3 scripts/test-attach.py
python3 scripts/test-shell.py
python3 scripts/test-native-concurrency.py
python3 scripts/test-shell-state.py
AGENTBOX_TEST_LOCALE=C python3 scripts/test-shell-state.py
python3 scripts/test-bulk-output.py
python3 scripts/test-concurrency.py
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

## Testing the optimized binary

The Python CLI/PTY/bulk/concurrency scripts accept `AGENTBOX_TEST_BIN` so the same
scenarios can exercise the release artifact rather than only a debug build:

```sh
AGENTBOX_TEST_BIN="$PWD/target/release/agentbox" python3 scripts/test-macos-cli.py
AGENTBOX_TEST_BIN="$PWD/target/release/agentbox" python3 scripts/test-attach.py
AGENTBOX_TEST_BIN="$PWD/target/release/agentbox" python3 scripts/test-shell.py
AGENTBOX_TEST_BIN="$PWD/target/release/agentbox" python3 scripts/test-shell-state.py
AGENTBOX_TEST_BIN="$PWD/target/release/agentbox" python3 scripts/test-zsh-plugins.py
AGENTBOX_TEST_BIN="$PWD/target/release/agentbox" python3 scripts/test-bulk-output.py
AGENTBOX_TEST_BIN="$PWD/target/release/agentbox" python3 scripts/test-concurrency.py
```

Local 0.3.0 release checks passed the CLI, shared PTY, bulk-output and concurrent-client suites. The 9 MiB release check took about 8 seconds in one local run.
`cargo install --path . --locked --offline --root TEMP_DIRECTORY` was also tested
in an isolated prefix; the installed executable reported `agentbox 0.3.0`.


Local 0.4.0 optimized-binary validation passed all seven runtime scripts: managed
CLI, shared attach, native shells, Zsh state import, real plugins, bulk output and
concurrent clients. The bulk fixture observed its complete 9 MiB with a bounded
returned tail in 8.10 seconds in this run; this is a single local measurement.
The C-locale state fixture seeds known UTF-8 bytes with ASCII escapes so it tests
serialization separately from ZLE's locale-dependent keyboard decoding.


Native concurrency validation starts 16 Zsh sessions simultaneously with different
client variables while every startup file waits for input. Creation and listing
remain responsive; each answer/import stays in its own session and all private
import directories are removed afterward. The fixture passed against local debug
and optimized binaries and is included in both CI runtime phases.
