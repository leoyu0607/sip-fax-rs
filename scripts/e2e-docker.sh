#!/usr/bin/env bash
# Run the tools/ workspace (SpanDSP fax simulation) checks inside the Rocky 9 test image.
# Usage: scripts/e2e-docker.sh [cargo-test-args...]
set -euo pipefail
cd "$(dirname "$0")/.."
IMAGE=sip-fax-rs-test:rocky9
# Always (re)build: layer cache makes this fast, and it keeps the image in sync with the Dockerfile.
docker build -q --platform linux/amd64 -f docker/test-rocky9.Dockerfile -t "$IMAGE" docker >/dev/null
docker run --rm --platform linux/amd64 \
  -v "$PWD":/work \
  -v sip-fax-rs-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/docker \
  -e RUSTFLAGS="-D warnings" \
  -w /work/tools "$IMAGE" \
  bash -euxc 'cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test -- --nocapture "$@" && cd /work && cargo test -p t4 --test oracle --test png_oracle' _ "$@"
