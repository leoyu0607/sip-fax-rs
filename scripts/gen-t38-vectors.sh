#!/usr/bin/env bash
# Regenerate crates/t38/tests/vectors/spandsp/*.ifpb from real SpanDSP sessions (needs Docker).
set -euo pipefail
cd "$(dirname "$0")/.."
IMAGE=sip-fax-rs-test:rocky9
docker build -q --platform linux/amd64 -f docker/test-rocky9.Dockerfile -t "$IMAGE" docker >/dev/null
docker run --rm --platform linux/amd64 \
  -v "$PWD":/work \
  -v sip-fax-rs-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/docker \
  -w /work/tools "$IMAGE" \
  cargo run -q --release -p fax-sim --bin gen_t38_vectors -- /work/crates/t38/tests/vectors/spandsp
