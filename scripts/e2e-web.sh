#!/usr/bin/env bash
# M4c E2E of the web interface (docs/test-plans/M4.md 6.2, 6.3: U08, U10-U19): Playwright
# (Chromium, on the host) against a real sip-faxd that runs in the Rocky 9 test image, with
# faxes sent by SpanDSP (`sip-uac`) from inside the same container.
#
#   scripts/e2e-web.sh                      # everything
#   scripts/e2e-web.sh -g U11               # Playwright arguments (here: only the cases named U11)
#   E2E_NO_BUILD=1 scripts/e2e-web.sh       # use the local image as it is
#   E2E_KEEP=1 scripts/e2e-web.sh           # leave the container running (docker rm -f sipfax-e2e-web)
#
# Needs Docker, Node 24 and (once) `npx playwright install chromium` in web-ui/.
# The binaries are built in the container into target/docker (shared with scripts/e2e-docker.sh).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$PWD"
export MSYS_NO_PATHCONV=1
if pwd -W >/dev/null 2>&1; then HOST_PWD="$(pwd -W)"; else HOST_PWD="$PWD"; fi
IMAGE="${E2E_IMAGE:-sip-fax-rs-test:rocky9}"
NAME="${E2E_CONTAINER:-sipfax-e2e-web}"
HTTP_PORT="${E2E_HTTP_PORT:-18080}"
HTTPS_PORT="${E2E_HTTPS_PORT:-18443}"

# the interface is embedded in the binary: build it first
(cd web-ui && { [ -d node_modules ] || npm ci --no-audit --no-fund; } && npm run build)

if [ -z "${E2E_NO_BUILD:-}" ]; then docker build -q --platform linux/amd64 -f docker/test-rocky9.Dockerfile -t "$IMAGE" docker >/dev/null; fi

mkdir -p "$ROOT/target/e2e-web"
cleanup() {
  rc=$?
  if [ "$rc" -ne 0 ]; then
    docker exec "$NAME" sh -c 'for f in /tmp/e2e/*/serve.log; do echo "== $f"; tail -n 60 "$f"; done' > "$ROOT/target/e2e-web/server.log" 2>&1 || true
    echo "e2e-web failed: the server logs are in target/e2e-web/server.log"
  fi
  if [ -z "${E2E_KEEP:-}" ]; then docker rm -f "$NAME" >/dev/null 2>&1 || true; fi
  exit "$rc"
}
trap cleanup EXIT
docker rm -f "$NAME" >/dev/null 2>&1 || true
docker run -d --name "$NAME" --platform linux/amd64 \
  -p "127.0.0.1:$HTTP_PORT:8080" -p "127.0.0.1:$HTTPS_PORT:8443" \
  -v "$HOST_PWD":/work \
  -v sip-fax-rs-cargo-registry:/usr/local/cargo/registry \
  -e CARGO_TARGET_DIR=/work/target/docker \
  -e SIPFAX_REQUIRE_WEB_UI=1 \
  -w /work "$IMAGE" sleep infinity >/dev/null

# sip-faxd with the interface, and the SpanDSP sender (sip-uac) and TIFF maker from tools/
docker exec "$NAME" bash -euc 'cargo build -p sip-faxd && cd tools && cargo build -p fax-sim --bins'
docker exec "$NAME" /work/scripts/e2e-web-container.sh

cd web-ui
export E2E_CONTAINER="$NAME"
export E2E_HTTP_URL="http://127.0.0.1:$HTTP_PORT"
export E2E_HTTPS_URL="https://127.0.0.1:$HTTPS_PORT"
npx playwright test "$@"
