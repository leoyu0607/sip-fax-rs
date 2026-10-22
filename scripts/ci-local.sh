#!/usr/bin/env bash
# Run the same checks as the required CI jobs. Run this before every push.
set -euo pipefail
cd "$(dirname "$0")/.."
# t4 oracle tests need libtiff/poppler; they run for real inside Docker (scripts/e2e-docker.sh).
export T4_ORACLE=skip
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
if command -v cargo-deny >/dev/null; then cargo deny check; else echo "warn: cargo-deny not installed, skipping"; fi
cargo test --workspace --all-targets
cargo test --workspace --doc
# web-ui (the CI job of the same name): needs Node 24 and npm
if command -v npm >/dev/null; then
  (cd web-ui && npm ci --no-audit --no-fund && npm run lint && npm run check && npm run coverage && npm run build)
else
  echo "warn: npm not installed, skipping the web-ui checks"
fi
# Concurrent `cargo llvm-cov` runs share target/llvm-cov-target and wipe each other's profile data
# (M2c: the likely cause of the one-off 83.5 % sip-core reading), so serialise them.
cov_lock=target/.llvm-cov.lock
mkdir -p target
until mkdir "$cov_lock" 2>/dev/null; do sleep 2; done
trap 'rmdir "$cov_lock" 2>/dev/null || true' EXIT
if cargo llvm-cov --version >/dev/null 2>&1; then
  # One command per line: under `set -e` a failure inside an `a && b && c` chain is ignored unless it
  # is the last command, which once let a failing coverage run pass silently.
  cargo llvm-cov -p sip-core --fail-under-lines 85 --summary-only
  cargo llvm-cov -p fax-in --fail-under-lines 70 --summary-only
  cargo llvm-cov -p t38 --fail-under-lines 85 --summary-only
  cargo llvm-cov -p t4 --fail-under-lines 85 --summary-only
  cargo llvm-cov -p t30 --fail-under-lines 85 --summary-only
  cargo llvm-cov -p store --fail-under-lines 80 --summary-only --ignore-filename-regex 'backend[\\/](mssql|mysql|postgres)\.rs'
  cargo llvm-cov -p delivery --fail-under-lines 80 --summary-only
  cargo llvm-cov -p web --fail-under-lines 80 --summary-only
else
  echo "warn: cargo-llvm-cov not installed, skipping coverage gate"
fi
rmdir "$cov_lock" 2>/dev/null || true
# Static musl build. CI uses zig 0.14.1; the zig of a developer machine is often newer (0.16 cannot
# build our C dependencies), so unless this is a Linux box that already has zig 0.14.x the build runs
# in Docker (Rocky 9 + zig 0.14.1 + cargo-zigbuild: scripts/musl-docker.sh).
if [ "$(uname -s)" = Linux ] && command -v cargo-zigbuild >/dev/null && command -v zig >/dev/null && zig version | grep -q '^0\.14\.'; then
  cargo zigbuild --release --locked --target x86_64-unknown-linux-musl -p sip-faxd
  file target/x86_64-unknown-linux-musl/release/sip-faxd | grep -q "statically linked"
elif command -v docker >/dev/null && docker info >/dev/null 2>&1; then
  scripts/musl-docker.sh x86_64-unknown-linux-musl
else
  echo "warn: no zig 0.14.x and no Docker, skipping the musl build"
fi
if command -v docker >/dev/null && docker info >/dev/null 2>&1; then
  scripts/e2e-docker.sh
  # M4b H01/H05/H08: the web certificate and TLS versions checked by openssl in the Rocky 9 image
  scripts/web-openssl-docker.sh
else
  echo "warn: docker not available, skipping tools/ SpanDSP e2e (scripts/e2e-docker.sh)"
fi
# Optional: the store suite on PostgreSQL / MySQL / MariaDB / MSSQL in Docker (several minutes, pulls
# ~3 GB of images the first time). CI always runs it as the db-matrix job.
if [ -n "${RUN_DB_MATRIX:-}" ]; then scripts/db-matrix.sh; else echo "note: set RUN_DB_MATRIX=1 to also run scripts/db-matrix.sh (store suite on the 4 server databases)"; fi
echo "ci-local: all checks passed"
