#!/usr/bin/env bash
# Compile itm-wasm et génère le module JavaScript dans web/pkg (servi tel quel par GitHub Pages).
# Prérequis : cible wasm32-unknown-unknown, wasm-bindgen-cli à la version du crate
# (cargo install --locked wasm-bindgen-cli --version 0.2.129). wasm-opt (binaryen) est facultatif.
set -euo pipefail
cd "$(dirname "$0")"

cargo build --release --target wasm32-unknown-unknown -p itm-wasm
rm -rf web/pkg
wasm-bindgen --target web --no-typescript --out-dir web/pkg \
  "${CARGO_TARGET_DIR:-target}/wasm32-unknown-unknown/release/itm_wasm.wasm"
if command -v wasm-opt >/dev/null; then
  wasm-opt -O3 --enable-bulk-memory --enable-nontrapping-float-to-int \
    web/pkg/itm_wasm_bg.wasm -o web/pkg/itm_wasm_bg.wasm
fi
ls -l web/pkg
