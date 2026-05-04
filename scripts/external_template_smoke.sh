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
download "waypoint-banking-payout" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/banking-payout.html"
download "waypoint-ecommerce-promo-code" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-promo-code.html"
download "waypoint-ecommerce-welcome" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/ecommerce-welcome.html"
download "waypoint-saas-reset-password" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-reset-password.html"
download "waypoint-saas-payment-declined" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/saas-payment-declined.html"
download "waypoint-social-new-comment" \
  "https://raw.githubusercontent.com/usewaypoint/responsive-transactional-email-templates/main/templates/social-new-comment.html"
download "mailpace-welcome" \
  "https://raw.githubusercontent.com/mailpace/templates/main/dist/welcome.html"
download "mailpace-confirmation" \
  "https://raw.githubusercontent.com/mailpace/templates/main/dist/confirmation.html"
download "mailpace-password-reset" \
  "https://raw.githubusercontent.com/mailpace/templates/main/dist/password_reset.html"
download "mailpace-receipt" \
  "https://raw.githubusercontent.com/mailpace/templates/main/dist/receipt.html"
download "mailpace-security-alert" \
  "https://raw.githubusercontent.com/mailpace/templates/main/dist/security_alert.html"
download "mailpace-account-deleted" \
  "https://raw.githubusercontent.com/mailpace/templates/main/dist/account_deleted.html"
download "postmark-welcome" \
  "https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/welcome/content.html"
download "postmark-password-reset" \
  "https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/password-reset/content.html"
download "postmark-receipt" \
  "https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/receipt/content.html"
download "postmark-invoice" \
  "https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/invoice/content.html"
download "postmark-comment-notification" \
  "https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/comment-notification/content.html"
download "postmark-dunning" \
  "https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/dunning/content.html"
download "postmark-user-invitation" \
  "https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/user-invitation/content.html"
download "postmark-trial-expiring" \
  "https://raw.githubusercontent.com/ActiveCampaign/postmark-templates/main/templates/basic/trial-expiring/content.html"

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
