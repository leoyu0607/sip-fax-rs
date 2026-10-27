#!/usr/bin/env bash
# The three PDF documents in docs/pdf/ from their Markdown sources (run it again at every release,
# after the screenshots are up to date):
#   docs/integration.md  -> docs/pdf/integration-guide.pdf  (串接指南)
#   docs/manual/agent.md -> docs/pdf/agent-manual.pdf       (座席使用手冊)
#   docs/manual/it.md    -> docs/pdf/it-manual.pdf          (IT 安裝維運手冊)
# Needs pandoc 3.x, the Playwright Chromium of web-ui (npm ci there, then npx playwright install
# chromium) and a Traditional Chinese font (Microsoft JhengHei on Windows, Noto Sans CJK TC on
# Linux).
set -euo pipefail
cd "$(dirname "$0")/.."
ROOT="$(pwd)"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
DATE="$(date +%Y-%m-%d)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
mkdir -p docs/pdf

# <markdown> <resource dir> <html out>: the first `# heading` becomes the title and a following
# `適用版本：...` line the subtitle; a table of contents comes before the body.
html() {
  local md="$1" res="$2" out="$3"
  local title subtitle body="$WORK/body.md"
  title="$(sed -n '1s/^# //p' "$md")"
  subtitle="$(sed -n '2,4{/^適用版本：/p}' "$md" | head -1)"
  [ -n "$subtitle" ] || subtitle="適用版本：v$VERSION-alpha"
  sed '1d' "$md" | sed '/^適用版本：/d' > "$body"
  pandoc "$body" -f gfm -t html5 -s --embed-resources --resource-path="$res" \
    --toc --toc-depth=2 -c "$ROOT/scripts/docs-pdf/style.css" \
    --metadata title="$title" --metadata subtitle="$subtitle" --metadata date="$DATE" \
    --metadata lang=zh-TW --metadata toc-title="目錄" -o "$out" 2>/dev/null
}

html docs/integration.md docs "$WORK/integration.html"
html docs/manual/agent.md docs/manual "$WORK/agent.html"
html docs/manual/it.md docs/manual "$WORK/it.html"

# Playwright is installed in web-ui/: print.mjs loads it from the working directory
cd web-ui
node ../scripts/docs-pdf/print.mjs \
  "$WORK/integration.html" "$ROOT/docs/pdf/integration-guide.pdf" "sip-fax 串接指南 v$VERSION" \
  "$WORK/agent.html" "$ROOT/docs/pdf/agent-manual.pdf" "sip-fax 座席使用手冊 v$VERSION" \
  "$WORK/it.html" "$ROOT/docs/pdf/it-manual.pdf" "sip-fax IT 安裝維運手冊 v$VERSION"
