#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/mail-canvas-text-hints-smoke.XXXXXX")"
trap 'rm -rf "$WORK_DIR"' EXIT

HTML="$ROOT/fixtures/text-hints/smoke.html"
ANNOTATED_HTML="$WORK_DIR/smoke.annotated.html"
ANNOTATE_REPORT="$WORK_DIR/annotate.report.json"
PASS1_LAYOUT="$WORK_DIR/pass1.layout.json"
PASS1_PNG="$WORK_DIR/pass1.png"
HINTS_JSON="$WORK_DIR/text-hints.json"
HINTS_REPORT="$WORK_DIR/text-hints.report.json"
PASS2_PNG="$WORK_DIR/pass2.png"

node "$ROOT/scripts/annotate_text_candidates.mjs" \
  --html "$HTML" \
  --out "$ANNOTATED_HTML" \
  --report-json "$ANNOTATE_REPORT" >/dev/null

BASE_URL="$(node -p "JSON.parse(require('fs').readFileSync('$ANNOTATE_REPORT', 'utf8')).baseUrl")"

cargo run -q -p mail-canvas-cli -- \
  --html "$ANNOTATED_HTML" \
  --base-url "$BASE_URL" \
  --output "$PASS1_PNG" \
  --layout-json "$PASS1_LAYOUT" \
  --width 420 \
  --viewport-height 600 \
  --font-file "$ROOT/fixtures/fonts/NotoSans-Regular.ttf" \
  --font-file "$ROOT/fixtures/fonts/NotoSans-Bold.ttf" >/dev/null

node "$ROOT/scripts/generate_text_hints.mjs" \
  --layout-json "$PASS1_LAYOUT" \
  --out "$HINTS_JSON" \
  --report-json "$HINTS_REPORT" \
  --strict \
  --font-file "$ROOT/fixtures/fonts/NotoSans-Regular.ttf" \
  --font-file "$ROOT/fixtures/fonts/NotoSans-Bold.ttf" >/dev/null

cargo run -q -p mail-canvas-cli -- \
  --html "$ANNOTATED_HTML" \
  --base-url "$BASE_URL" \
  --text-hints-json "$HINTS_JSON" \
  --output "$PASS2_PNG" \
  --width 420 \
  --viewport-height 600 \
  --font-file "$ROOT/fixtures/fonts/NotoSans-Regular.ttf" \
  --font-file "$ROOT/fixtures/fonts/NotoSans-Bold.ttf" >/dev/null

node - <<'NODE' "$ANNOTATE_REPORT" "$HINTS_REPORT"
const fs = require('fs');
const [annotatePath, hintsPath] = process.argv.slice(2);
const annotate = JSON.parse(fs.readFileSync(annotatePath, 'utf8'));
const hints = JSON.parse(fs.readFileSync(hintsPath, 'utf8'));
if (annotate.candidateCount < 1) {
  throw new Error(`expected annotated candidates, got ${annotate.candidateCount}`);
}
if (hints.applied < 1) {
  throw new Error(`expected at least one applied text hint, got ${hints.applied}`);
}
console.log(JSON.stringify({
  annotated: annotate.candidateCount,
  matched: hints.matched,
  eligible: hints.eligible,
  applied: hints.applied,
}, null, 2));
NODE
