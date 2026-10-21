#!/usr/bin/env bash
# Run the tools/ workspace (SpanDSP fax simulation) checks inside the Rocky 9 test image.
# Usage: scripts/e2e-docker.sh [cargo-test-args...]
#   E2E_ONLY="--test e2e_m3b" scripts/e2e-docker.sh        # only one test binary (cargo test arguments)
#   E2E_ONLY="--test e2e_m3b" E2E_SKIP_CHECKS=1 ...        # ... and skip fmt / clippy / the t4 oracle
#   E2E_NO_BUILD=1 ...                                      # use the local image as it is (no registry access needed)
# Works on Linux, macOS and Windows (Git Bash with Docker Desktop).
set -euo pipefail
cd "$(dirname "$0")/.."
# Windows (Git Bash): keep the container paths (/work, ...) as they are and give Docker a Windows path.
export MSYS_NO_PATHCONV=1
if pwd -W >/dev/null 2>&1; then HOST_PWD="$(pwd -W)"; else HOST_PWD="$PWD"; fi
IMAGE="${E2E_IMAGE:-sip-fax-rs-test:rocky9}"
# Always (re)build: layer cache makes this fast, and it keeps the image in sync with the Dockerfile.
if [ -z "${E2E_NO_BUILD:-}" ]; then docker build -q --platform linux/amd64 -f docker/test-rocky9.Dockerfile -t "$IMAGE" docker >/dev/null; fi
if [ -n "${E2E_SKIP_CHECKS:-}" ]; then
  CMD='cargo test ${E2E_ONLY:-} -- --nocapture "$@"'
else
  CMD='cargo fmt --check && cargo clippy --all-targets -- -D warnings && cargo test ${E2E_ONLY:-} -- --nocapture "$@" && cd /work && cargo test -p t4 --test oracle --test png_oracle'
fi
docker run --rm --platform linux/amd64 \
  -v "$HOST_PWD":/work \
  -v sip-fax-rs-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/docker \
  -e RUSTFLAGS="-D warnings" \
  -e E2E_ONLY="${E2E_ONLY:-}" \
  -w /work/tools "$IMAGE" \
  bash -euxc "$CMD" _ "$@"
