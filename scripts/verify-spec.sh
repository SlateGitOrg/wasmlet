#!/usr/bin/env bash
# Verifies the vendored spec testsuite against tests/spec/SHA256SUMS.
# With --upstream, also re-downloads every file from the pinned upstream
# commit and checks it hashes to the same value (needs network).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
commit=fffc6e12fa454e475455a7b58d3b5dc343980c10 # WebAssembly/spec, branch wg-2.0
cd "$root/tests/spec/core"
shasum -a 256 -c ../SHA256SUMS --quiet
echo "local: $(wc -l < ../SHA256SUMS | tr -d ' ') files match SHA256SUMS"
if [[ "${1:-}" == "--upstream" ]]; then
  tmp="$(mktemp -d)"
  trap 'rm -rf "$tmp"' EXIT
  while read -r _ name; do
    curl -fsSL --max-time 30 -o "$tmp/$name" \
      "https://raw.githubusercontent.com/WebAssembly/spec/$commit/test/core/$name"
  done < ../SHA256SUMS
  (cd "$tmp" && shasum -a 256 -c "$root/tests/spec/SHA256SUMS" --quiet)
  echo "upstream: all files match WebAssembly/spec@$commit test/core"
fi
