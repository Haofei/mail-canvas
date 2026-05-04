#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
RENDERER="${RENDERER:-$ROOT_DIR/target/debug/email-render}"
WORK_DIR="${WORK_DIR:-/tmp/email-render-external}"

if [[ ! -x "$RENDERER" ]]; then
  cargo build --manifest-path "$ROOT_DIR/Cargo.toml"
fi

rm -rf "$WORK_DIR"
mkdir -p "$WORK_DIR/html" "$WORK_DIR/png" "$WORK_DIR/pdf"

download() {
  local name="$1"
  local url="$2"
  curl -fsSL -o "$WORK_DIR/html/$name.html" "$url"
}

while IFS=$'\t' read -r name url; do
  download "$name" "$url"
done < <(
  cd "$ROOT_DIR"
  node --input-type=module -e 'import { TEMPLATES } from "./scripts/templates.mjs"; for (const [name, url] of TEMPLATES) console.log(name + "\t" + url);'
)

for html in "$WORK_DIR"/html/*.html; do
  name="$(basename "$html" .html)"
  log="$WORK_DIR/$name.log"
  "$RENDERER" \
    --html "$html" \
    --output "$WORK_DIR/png/$name.png" \
    --pdf-output "$WORK_DIR/pdf/$name.pdf" \
    --width 600 \
    --allow-remote \
    --timeout-ms 15000 >"$log" 2>&1
  printf '%s\t' "$name"
  sed -n '1p' "$log"
done

printf 'outputs: %s/{png,pdf}\n' "$WORK_DIR"
