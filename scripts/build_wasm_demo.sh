#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

OUT_DIR="demo/pkg"
WASM_TARGET_DIR="target/wasm32-unknown-unknown/release"
PRETEXT_OUT_DIR="demo/vendor/pretext"

cargo build -p mail-canvas-wasm --release --target wasm32-unknown-unknown
mkdir -p "$OUT_DIR"
wasm-bindgen \
  --target web \
  --no-typescript \
  --out-dir "$OUT_DIR" \
  "$WASM_TARGET_DIR/mail_canvas_wasm.wasm"

rm -rf "$PRETEXT_OUT_DIR"
mkdir -p "$PRETEXT_OUT_DIR"
cp node_modules/@chenglou/pretext/dist/*.js "$PRETEXT_OUT_DIR/"
mkdir -p "$PRETEXT_OUT_DIR/generated"
cp node_modules/@chenglou/pretext/dist/generated/*.js "$PRETEXT_OUT_DIR/generated/"
