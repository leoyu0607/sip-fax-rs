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
cp "$ROOT/LICENSE" "$ROOT/README.md" "$ROOT/README.zh-TW.md" "$ROOT/CHANGELOG.md" "$STAGE/"
mkdir -p "$STAGE/docs" && cp "$ROOT/docs/field-test.md" "$ROOT/docs/database.md" "$STAGE/docs/"
export COPYFILE_DISABLE=1   # no macOS ._ AppleDouble files in local builds
# Store files as root:root so extracting as root never maps them to an unrelated local
# user (the build machine's uid would otherwise be restored, e.g. showing up as mysql).
if tar --version 2>/dev/null | grep -q "GNU tar"; then
  OWNER_FLAGS=(--owner=0 --group=0 --numeric-owner)
else
  OWNER_FLAGS=(--uid 0 --gid 0 --numeric-owner)
fi
tar "${OWNER_FLAGS[@]}" -C "$ROOT/dist" -czf "$ROOT/dist/$NAME.tar.gz" "$NAME"
echo "$ROOT/dist/$NAME.tar.gz"
