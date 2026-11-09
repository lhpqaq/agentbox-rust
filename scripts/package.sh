#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."
# Source-only archive; this script does not imply a build/test pass.
mkdir -p dist
find src tests docs scripts web -type f -print0 | sort -z | xargs -0 sha256sum > SOURCE-MANIFEST.sha256
sha256sum Cargo.toml README.md 开始使用.md LICENSE .gitignore >> SOURCE-MANIFEST.sha256
zip -q -r dist/AgentBox-Rust-source-unverified.zip Cargo.toml README.md 开始使用.md LICENSE .gitignore SOURCE-MANIFEST.sha256 src tests docs scripts web
unzip -tq dist/AgentBox-Rust-source-unverified.zip
sha256sum dist/AgentBox-Rust-source-unverified.zip > dist/SHA256SUMS.txt
