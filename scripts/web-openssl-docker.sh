#!/usr/bin/env bash
# H01, H05 and H08 of the M4 test plan: the web server's certificate and TLS versions checked by
# `openssl` (an implementation that is not ours) inside the Rocky 9 test image.
# Usage: scripts/web-openssl-docker.sh [cargo-test-args...]
#   E2E_NO_BUILD=1 scripts/web-openssl-docker.sh     # use the local image as it is
# The tests are crates/web/tests/openssl.rs (`#[ignore]`d outside this script).
# Works on Linux, macOS and Windows (Git Bash with Docker Desktop).
set -euo pipefail
cd "$(dirname "$0")/.."
export MSYS_NO_PATHCONV=1
if pwd -W >/dev/null 2>&1; then HOST_PWD="$(pwd -W)"; else HOST_PWD="$PWD"; fi
IMAGE="${E2E_IMAGE:-sip-fax-rs-test:rocky9}"
if [ -z "${E2E_NO_BUILD:-}" ]; then docker build -q --platform linux/amd64 -f docker/test-rocky9.Dockerfile -t "$IMAGE" docker >/dev/null; fi
docker run --rm --platform linux/amd64 \
  -v "$HOST_PWD":/work \
  -v sip-fax-rs-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/docker \
  -e RUSTFLAGS="-D warnings" \
  -w /work "$IMAGE" \
  bash -euxc 'openssl version; cargo test -p web --test openssl -j 2 -- --ignored --nocapture --test-threads 1 "$@"' _ "$@"
