#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --check
cargo check --all-targets
cargo test -- --test-threads=1
cargo clippy --all-targets -- -D warnings
for test in web/test-*.mjs; do node "$test"; done
printf '\nDefault checks deliberately do not open sockets. See docs/VERIFICATION.md for opt-in end-to-end checks.\n'
