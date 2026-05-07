#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

OUT_DIR="browser/pkg"
WASM_TARGET_DIR="target/wasm32-unknown-unknown/release"

cargo build -p mail-canvas-wasm --release --target wasm32-unknown-unknown
mkdir -p "$OUT_DIR"
wasm-bindgen \
  --target web \
  --no-typescript \
  --out-dir "$OUT_DIR" \
  "$WASM_TARGET_DIR/mail_canvas_wasm.wasm"
