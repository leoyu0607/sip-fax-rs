#!/usr/bin/env bash
# Build the static musl sip-faxd inside Docker: Rocky 9 + zig 0.14.1 + cargo-zigbuild, the same
# toolchain as the CI `build-musl` job, so a Mac or Windows machine does not depend on the zig
# that happens to be installed on it (zig 0.16 cannot build our C dependencies).
#
# Usage: scripts/musl-docker.sh [target]            (default x86_64-unknown-linux-musl)
# Output: target/musl-docker/<target>/release/sip-faxd  (statically linked, checked with `file`)
# Also builds the store test executable and runs it (SQLite) when the target is x86_64.
set -euo pipefail
cd "$(dirname "$0")/.."
TARGET="${1:-x86_64-unknown-linux-musl}"
command -v docker >/dev/null && docker info >/dev/null 2>&1 || { echo "docker is required" >&2; exit 2; }
# Windows (Git Bash): keep the container paths as they are and give Docker a Windows path.
export MSYS_NO_PATHCONV=1
if pwd -W >/dev/null 2>&1; then HOST_PWD="$(pwd -W)"; else HOST_PWD="$PWD"; fi
BASE=sip-fax-rs-test:rocky9
IMAGE=sip-fax-rs-musl:zig0.14.1
if [ -z "${E2E_NO_BUILD:-}" ]; then
docker build -q --platform linux/amd64 -f docker/test-rocky9.Dockerfile -t "$BASE" docker >/dev/null
docker build -q --platform linux/amd64 -f docker/musl-zigbuild.Dockerfile -t "$IMAGE" docker >/dev/null
fi
docker run --rm --platform linux/amd64 \
  -v "$HOST_PWD":/work \
  -v sip-fax-rs-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/musl-docker \
  -e TARGET="$TARGET" \
  -w /work "$IMAGE" \
  bash -euxc '
    cargo zigbuild --release --locked --target "$TARGET" -p sip-faxd
    BIN="target/musl-docker/$TARGET/release/sip-faxd"
    file "$BIN" | tee /dev/stderr | grep -q "statically linked"
    # the store crate (sqlx with bundled SQLite, tiberius) must link statically as well
    EXE=$(cargo zigbuild --release --locked --target "$TARGET" -p store --tests --message-format=json \
      | jq -r "select(.profile.test == true and .executable != null) | .executable" | head -1)
    file "$EXE" | tee /dev/stderr | grep -q "statically linked"
    if [ "$TARGET" = x86_64-unknown-linux-musl ]; then "$EXE" --test-threads=4; fi
  '
echo "musl-docker: $TARGET OK (target/musl-docker/$TARGET/release/sip-faxd)"
