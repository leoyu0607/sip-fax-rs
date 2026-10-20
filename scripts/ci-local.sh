#!/usr/bin/env bash
# Run the same checks as the required CI jobs. Run this before every push.
set -euo pipefail
cd "$(dirname "$0")/.."
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
if command -v cargo-deny >/dev/null; then cargo deny check; else echo "warn: cargo-deny not installed, skipping"; fi
cargo test --workspace --all-targets
cargo test --workspace --doc
cargo zigbuild --release --locked --target x86_64-unknown-linux-musl -p sip-faxd
file target/x86_64-unknown-linux-musl/release/sip-faxd | grep -q "statically linked"
if command -v docker >/dev/null && docker info >/dev/null 2>&1; then
  scripts/e2e-docker.sh
else
  echo "warn: docker not available, skipping tools/ SpanDSP e2e (scripts/e2e-docker.sh)"
fi
echo "ci-local: all checks passed"
