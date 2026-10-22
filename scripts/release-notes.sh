#!/usr/bin/env bash
# The GitHub Release description of a tag: the English and the Traditional Chinese section of
# CHANGELOG.md for that version. Fails when either is missing or empty, so that a release is
# never published without both.
#
#   scripts/release-notes.sh v0.2.0-alpha [CHANGELOG.md] > notes.md
set -euo pipefail
tag=${1:?usage: release-notes.sh <tag> [changelog]}
file=${2:-"$(dirname "$0")/../CHANGELOG.md"}
ver=${tag#v}

# English: "## [ver] ..." up to the next "## " heading or the "---" before the Chinese part.
en=$(awk -v v="$ver" '
  index($0, "## [" v "]") == 1 { on = 1; next }
  on && (/^## / || /^---/) { exit }
  on { print }
' "$file")
# Chinese: after "## 繁體中文", "### [ver] ..." up to the next "### [" heading.
zh=$(awk -v v="$ver" '
  /^## 繁體中文/ { part = 1; next }
  part && index($0, "### [" v "]") == 1 { on = 1; next }
  on && /^### \[/ { exit }
  on { print }
' "$file")

trim() { sed -e '/./,$!d' | tac | sed -e '/./,$!d' | tac; }
en=$(printf '%s\n' "$en" | trim)
zh=$(printf '%s\n' "$zh" | trim)
if [ -z "$en" ]; then echo "release-notes: no English section [$ver] in $file" >&2; exit 1; fi
if [ -z "$zh" ]; then echo "release-notes: no 繁體中文 section [$ver] in $file" >&2; exit 1; fi

printf '## English\n\n%s\n\n## 繁體中文\n\n%s\n' "$en" "$zh"
