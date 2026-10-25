#!/usr/bin/env bash
# Offline end-to-end demo of wasmlet against the bundled fixtures.
# Honors CARGO_TARGET_DIR / CARGO_BUILD_JOBS from the environment.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

cargo build -q --release -p wasmlet-cli -p wasmlet-spec
bin="${CARGO_TARGET_DIR:-$root/target}/release"
w="$bin/wasmlet"

step() { printf '\n\033[1m== %s\033[0m\n$ %s\n' "$1" "$2"; }

step "1. A rustc-compiled WASI program (std, args, env, collections)" \
  "wasmlet run --deterministic --env NAME=demo fixtures/wasm/hello-rust.wasm alpha beta"
"$w" run --deterministic --env NAME=demo fixtures/wasm/hello-rust.wasm alpha beta

step "2. Exit codes propagate from proc_exit" \
  "wasmlet run fixtures/wasm/hello-rust.wasm --fail; echo \$?"
set +e
"$w" run fixtures/wasm/hello-rust.wasm --fail >/dev/null 2>&1
echo "exit status: $?"
set -e

step "3. A rustc-compiled wc reading stdin" \
  "wasmlet run --stdin fixtures/text/poem.txt fixtures/wasm/wc-rust.wasm"
"$w" run --stdin fixtures/text/poem.txt fixtures/wasm/wc-rust.wasm

step "4. Calling an export directly, with phase timings" \
  "wasmlet run --stats --invoke fib fixtures/wat/fib.wat 25"
"$w" run --stats --invoke fib fixtures/wat/fib.wat 25

step "5. Fuel metering stops a runaway computation with a clean trap" \
  "wasmlet run --fuel 100000 --invoke fib fixtures/wat/fib.wat 40"
set +e
"$w" run --fuel 100000 --invoke fib fixtures/wat/fib.wat 40
echo "exit status: $?"
set -e

step "6. The compact IR, with and without superinstructions" \
  "wasmlet inspect --ir fixtures/wat/loop.wat   (then --no-fuse)"
"$w" inspect --ir fixtures/wat/loop.wat | sed -n '5,40p'
echo "--- without superinstructions:"
"$w" inspect --ir --no-fuse fixtures/wat/loop.wat | sed -n '5,40p'

step "7. Malformed and invalid modules are rejected with a reason" \
  "wasmlet validate <truncated module>; wasmlet validate <ill-typed module>"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
printf '\000asm\001\000\000\000\001\005\001\140' > "$tmp/truncated.wasm"
printf '(module (func (result i32) (i64.const 1)))' > "$tmp/badtype.wat"
"$w" validate "$tmp/truncated.wasm" || true
"$w" validate "$tmp/badtype.wat" || true

step "8. The pinned official spec testsuite (90 .wast files, baseline-checked)" \
  "wasmlet-spec --expect tests/spec/expected.tsv tests/spec/core | tail"
"$bin/wasmlet-spec" --expect tests/spec/expected.tsv tests/spec/core | tail -n 9
