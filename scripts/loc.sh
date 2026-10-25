#!/usr/bin/env bash
# Counts Rust lines: shipped source (excluding trailing #[cfg(test)] modules
# and blank lines) vs test code. Usage: scripts/loc.sh
set -euo pipefail
cd "$(dirname "$0")/.."
count() { # prints "<source> <inline-test>" non-blank line counts for one file
  awk 'BEGIN{t=0} /^#\[cfg\(test\)\]/{t=1} NF{ if(t) b++; else a++ } END{print a+0, b+0}' "$1"
}
src=0; inline=0
printf '%-40s %8s %8s\n' "file" "source" "tests"
for f in $(find src crates/*/src -name '*.rs' | sort); do
  read -r a b < <(count "$f")
  printf '%-40s %8d %8d\n' "$f" "$a" "$b"
  src=$((src + a)); inline=$((inline + b))
done
ext=$(find tests crates/*/tests -name '*.rs' -print0 | xargs -0 awk 'NF' | wc -l | tr -d ' ')
echo "----"
echo "shipped source (non-blank, excluding test modules): $src"
echo "test code: $inline inline + $ext in integration test files = $((inline + ext))"
