#!/usr/bin/env bash
# U06 (docs/test-plans/M4.md 6.3): scripts/package.sh refuses to package a binary that does not
# carry this checkout's web interface. Runs package.sh from a scratch copy of the repository
# layout (a fake "binary" and a fake web-ui/dist), so no build is needed.
#   scripts/test-package.sh
set -uo pipefail
HERE="$(cd "$(dirname "$0")/.." && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
TARGET=x86_64-unknown-linux-musl
fails=0

fresh() {   # a layout package.sh could package, apart from what a case removes
  rm -rf "$WORK/r" && mkdir -p "$WORK/r/scripts" "$WORK/r/conf" "$WORK/r/docs" \
    "$WORK/r/target/$TARGET/release" "$WORK/r/web-ui/dist/assets"
  cp "$HERE/scripts/package.sh" "$WORK/r/scripts/"
  printf '[workspace.package]\nversion = "9.9.9"\n' > "$WORK/r/Cargo.toml"
  for f in conf/sip-faxd.toml.example conf/runtime.toml.example LICENSE README.md README.zh-TW.md CHANGELOG.md docs/field-test.md docs/database.md docs/web.md; do
    echo x > "$WORK/r/$f"
  done
  echo 'console.log(1)' > "$WORK/r/web-ui/dist/assets/index-AbC123xy.js"
  echo '<html></html>' > "$WORK/r/web-ui/dist/index.html"
  # a binary that embeds the entry file name of the dist
  printf 'ELF....index-AbC123xy.js....' > "$WORK/r/target/$TARGET/release/sip-faxd"
}

run() { OUT="$("$WORK/r/scripts/package.sh" "$TARGET" 2>&1)"; RC=$?; }

expect_fail() {   # name, text the message must contain
  if [ "$RC" -eq 0 ]; then echo "FAIL $1: package.sh succeeded"; fails=$((fails + 1)); return; fi
  if ! grep -q -- "$2" <<<"$OUT"; then echo "FAIL $1: no '$2' in: $OUT"; fails=$((fails + 1)); return; fi
  if ls "$WORK"/r/dist/*.tar.gz >/dev/null 2>&1; then echo "FAIL $1: a tarball was written"; fails=$((fails + 1)); return; fi
  echo "ok   $1"
}

fresh; rm -rf "$WORK/r/web-ui/dist"; run
expect_fail "U06 no web-ui/dist" "web-ui/dist is missing"

fresh; rm -rf "$WORK/r/web-ui/dist/assets"/*; run
expect_fail "U06 dist without an entry file" "web-ui/dist is missing"

fresh; printf 'ELF....index-OldOld99.js....' > "$WORK/r/target/$TARGET/release/sip-faxd"; run
expect_fail "U06 binary of another build (other entry name)" "does not embed"

fresh; printf 'ELF....no interface....' > "$WORK/r/target/$TARGET/release/sip-faxd"; run
expect_fail "U06 binary without any interface" "does not embed"

fresh; rm "$WORK/r/target/$TARGET/release/sip-faxd"; run
expect_fail "U06 no binary at all" "does not embed"

# and the positive case: the binary carries this dist
fresh; run
tarball="$WORK/r/dist/sip-fax-rs-9.9.9-linux-x86_64.tar.gz"
if [ "$RC" -ne 0 ] || [ ! -s "$tarball" ]; then
  echo "FAIL U06 matching binary was refused: $OUT"; fails=$((fails + 1))
elif ! tar -tzf "$tarball" | grep -q 'sip-faxd$'; then
  echo "FAIL U06 tarball has no sip-faxd"; fails=$((fails + 1))
else
  echo "ok   U06 binary with this dist is packaged"
fi

if [ "$fails" -ne 0 ]; then echo "$fails failed"; exit 1; fi
echo "test-package: all passed"
