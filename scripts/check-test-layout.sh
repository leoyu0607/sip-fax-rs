#!/usr/bin/env bash
# ADR-072: test code is never written inside a product source file. A module gated on `test`
# (`#[cfg(test)]`, `#[cfg(any(test, ...))]`) is declared there as `mod tests;` and lives in its own
# file (`foo.rs` -> `foo/tests.rs`; also `*_tests.rs`, `testutil.rs`, `testtx.rs`), so that a
# source scan of the product can leave all test code out by file name. Fails on an inline one.
set -euo pipefail
cd "$(dirname "$0")/.."
bad=0
while IFS= read -r f; do
  case "$f" in
    */tests.rs | */*_tests.rs | */testutil.rs | */testtx.rs | */src/tests/*) continue ;;
  esac
  if awk '
    /^[[:space:]]*#\[cfg\((any\()?test/ { gated = 1; next }
    gated && /^[[:space:]]*#\[/ { next }
    gated && /^[[:space:]]*(pub(\(crate\))? )?mod [A-Za-z0-9_]+ *\{/ { print FILENAME ":" FNR ": " $0; found = 1 }
    { gated = 0 }
    END { exit found ? 0 : 1 }
  ' "$f"; then
    bad=1
  fi
done < <(git ls-files 'crates/*/src/*.rs' 'crates/*/src/**/*.rs' 'apps/*/src/*.rs' 'apps/*/src/**/*.rs')
if [ "$bad" -ne 0 ]; then
  echo "error: inline test module(s) above: move each into its own file (#[cfg(test)] mod tests; -> <module>/tests.rs), see ADR-072" >&2
  exit 1
fi
echo "test layout: ok (no inline test modules in product sources)"
