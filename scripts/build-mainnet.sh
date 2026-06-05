#!/usr/bin/env bash
# Build mainnet-ready WASMs.
#
# 1. `stellar contract build` produces target/wasm32v1-none/release/*.wasm
#    with the workspace's release profile (opt-level=z, lto, panic=abort,
#    strip=symbols, codegen-units=1).
# 2. We run wasm-opt -Oz over each output, which shrinks the WASMs by
#    roughly 10-30% on top of rustc's output. Soroban upload cost scales
#    with WASM byte size, so this directly reduces mainnet deploy XLM.
# 3. mock-blend is excluded — testnet-only.
#
# Run from the protocol repo root:
#   ./scripts/build-mainnet.sh

set -euo pipefail

cd "$(dirname "$0")/.."

if ! command -v wasm-opt >/dev/null 2>&1; then
  echo "wasm-opt missing. Install Binaryen: brew install binaryen" >&2
  exit 1
fi

echo ">> stellar contract build"
stellar contract build >/dev/null

OUT_DIR="target/wasm32v1-none/release"
MAINNET=(amm blend_adapter pt_token strate_factory strate_oracle yield_stripping yt_token)

echo ""
echo ">> wasm-opt -Oz pass"
printf "%-25s %10s %10s %8s\n" "wasm" "rustc" "opt" "saved"
total_rustc=0
total_opt=0
for name in "${MAINNET[@]}"; do
  file="$OUT_DIR/$name.wasm"
  if [[ ! -f "$file" ]]; then
    echo "missing: $file" >&2
    exit 1
  fi
  before=$(stat -f%z "$file")
  wasm-opt -Oz \
    --enable-bulk-memory --enable-mutable-globals \
    --enable-sign-ext --enable-nontrapping-float-to-int \
    --strip-debug --strip-producers --strip-target-features --vacuum \
    "$file" -o "$file.opt"
  mv "$file.opt" "$file"
  after=$(stat -f%z "$file")
  pct=$(awk "BEGIN {printf \"%.1f\", (1 - $after/$before) * 100}")
  printf "%-25s %10d %10d %7s%%\n" "$name.wasm" "$before" "$after" "-$pct"
  total_rustc=$((total_rustc + before))
  total_opt=$((total_opt + after))
done

echo ""
saved=$((total_rustc - total_opt))
pct=$(awk "BEGIN {printf \"%.1f\", (1 - $total_opt/$total_rustc) * 100}")
printf "%-25s %10d %10d %7s%%  (%d bytes saved)\n" "TOTAL" "$total_rustc" "$total_opt" "-$pct" "$saved"
echo ""
echo "Mainnet WASMs ready in $OUT_DIR/"
echo "Upload with: stellar contract upload --wasm <path> --network mainnet --source strate-deploy"
