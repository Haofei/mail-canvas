#!/usr/bin/env bash
set -euo pipefail

REV="${1:-3994650ae3f2d575a583898776d1eafd38b90ed5}"
OUT_DIR="${BLINK_REFERENCE_DIR:-blink-reference}"

FETCH_DIRS=(
  "third_party/blink/renderer/core/css"
  "third_party/blink/renderer/core/html"
  "third_party/blink/renderer/core/style"
  "third_party/blink/renderer/core/layout"
  "third_party/blink/renderer/core/paint"
  "third_party/blink/renderer/platform/fonts"
)

fetch_dir() {
  local remote="$1"
  local dest="${OUT_DIR}/${remote}"
  local archive="/tmp/blink-$(echo "${remote}" | tr '/' '-').tar.gz"

  rm -rf "${dest}"
  mkdir -p "${dest}"

  curl \
    -L \
    --fail \
    --retry 3 \
    --connect-timeout 20 \
    --max-time 300 \
    -o "${archive}" \
    "https://chromium.googlesource.com/chromium/src/+archive/${REV}/${remote}.tar.gz"

  tar -xzf "${archive}" -C "${dest}"
}

mkdir -p "${OUT_DIR}"

for remote in "${FETCH_DIRS[@]}"; do
  echo "Fetching ${remote} @ ${REV}"
  fetch_dir "${remote}"
done

cat > "${OUT_DIR}/REVISION" <<EOF
${REV}
EOF

echo "Blink reference written to ${OUT_DIR}"
