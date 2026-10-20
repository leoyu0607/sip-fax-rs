#!/usr/bin/env bash
# Package a built sip-faxd binary into an unpack-and-run tarball under dist/.
set -euo pipefail
TARGET="${1:-x86_64-unknown-linux-musl}"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
VERSION="$(grep -m1 '^version' "$ROOT/Cargo.toml" | cut -d'"' -f2)"
ARCH="${TARGET%%-*}"
NAME="sip-fax-rs-${VERSION}-linux-${ARCH}"
STAGE="$ROOT/dist/$NAME"

rm -rf "$STAGE" && mkdir -p "$STAGE"/{conf,data,logs}
cp "$ROOT/target/$TARGET/release/sip-faxd" "$STAGE/"
cp "$ROOT/conf/sip-faxd.toml.example" "$STAGE/conf/"
cp "$ROOT/LICENSE" "$ROOT/README.md" "$ROOT/README.zh-TW.md" "$STAGE/"
tar -C "$ROOT/dist" -czf "$ROOT/dist/$NAME.tar.gz" "$NAME"
echo "$ROOT/dist/$NAME.tar.gz"
