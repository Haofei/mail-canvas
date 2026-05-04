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

download "leemunroe-inlined" \
  "https://raw.githubusercontent.com/leemunroe/responsive-html-email-template/master/email-inlined.html"
download "mailgun-action" \
  "https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/action.html"
download "mailgun-alert" \
  "https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/alert.html"
download "mailgun-billing" \
  "https://raw.githubusercontent.com/mailgun/transactional-email-templates/master/templates/inlined/billing.html"
download "waypoint-saas-otp" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-one-time-passcode-otp.html"
download "waypoint-saas-receipt" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-subscription-receipt.html"
download "waypoint-ecommerce-delivery" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-delivery-notification.html"
download "waypoint-marketplace-qr" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/marketplace-qr-tickets.html"

for html in "$WORK_DIR"/html/*.html; do
  name="$(basename "$html" .html)"
  log="$WORK_DIR/$name.log"
  "$RENDERER" \
    --html "$html" \
    --output "$WORK_DIR/png/$name.png" \
    --pdf-output "$WORK_DIR/pdf/$name.pdf" \
    --width 600 >"$log" 2>&1
  printf '%s\t' "$name"
  sed -n '1p' "$log"
done

printf 'outputs: %s/{png,pdf}\n' "$WORK_DIR"
