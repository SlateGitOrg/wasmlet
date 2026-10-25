#!/usr/bin/env bash
# Rebuilds the committed guest binaries in fixtures/wasm/.
# Requires: rustup target add wasm32-wasip1. Not needed for `make check`.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/target}/fixtures"
cargo build --manifest-path "$root/fixtures/rust/Cargo.toml" --release \
  --target wasm32-wasip1 --target-dir "$target_dir"
out="$target_dir/wasm32-wasip1/release"
cp "$out/hello.wasm" "$root/fixtures/wasm/hello-rust.wasm"
cp "$out/wc.wasm" "$root/fixtures/wasm/wc-rust.wasm"
cp "$out/wasmlet_fixture_kernels.wasm" "$root/fixtures/wasm/kernels-rust.wasm"
ls -l "$root/fixtures/wasm"
