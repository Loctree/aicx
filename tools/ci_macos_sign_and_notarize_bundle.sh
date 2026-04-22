#!/usr/bin/env bash
set -euo pipefail

if [[ $# -lt 3 ]]; then
  echo "Usage: $0 <bundle-dir> <output-archive.zip> <notary-log.json>" >&2
  exit 1
fi

BUNDLE_DIR="$1"
ARCHIVE_PATH="$2"
NOTARY_LOG_PATH="$3"

: "${MACOS_DEVELOPER_ID_APPLICATION:?Set MACOS_DEVELOPER_ID_APPLICATION}"
: "${MACOS_KEYCHAIN_PATH:?Set MACOS_KEYCHAIN_PATH}"
: "${APPLE_API_KEY_BASE64:?Set APPLE_API_KEY_BASE64}"
: "${APPLE_API_KEY_ID:?Set APPLE_API_KEY_ID}"
: "${APPLE_API_ISSUER_ID:?Set APPLE_API_ISSUER_ID}"

if [[ ! -d "$BUNDLE_DIR" ]]; then
  echo "Missing bundle dir: $BUNDLE_DIR" >&2
  exit 1
fi

cleanup() {
  if [[ -n "${APPLE_API_KEY_PATH:-}" && -f "${APPLE_API_KEY_PATH}" ]]; then
    rm -f "${APPLE_API_KEY_PATH}"
  fi
}
trap cleanup EXIT

for bin in aicx aicx-mcp; do
  target="$BUNDLE_DIR/$bin"
  if [[ ! -f "$target" ]]; then
    echo "Missing required binary in bundle: $target" >&2
    exit 1
  fi

  codesign \
    --force \
    --timestamp \
    --options runtime \
    --sign "$MACOS_DEVELOPER_ID_APPLICATION" \
    --keychain "$MACOS_KEYCHAIN_PATH" \
    "$target"
  codesign --verify --verbose=2 "$target" >/dev/null
done

APPLE_API_KEY_PATH="${RUNNER_TEMP:-/tmp}/AuthKey_${APPLE_API_KEY_ID}.p8"
APPLE_API_KEY_PATH="$APPLE_API_KEY_PATH" python3 - <<'PY'
import base64
import os
from pathlib import Path

Path(os.environ["APPLE_API_KEY_PATH"]).write_bytes(
    base64.b64decode(os.environ["APPLE_API_KEY_BASE64"])
)
PY

rm -f "$ARCHIVE_PATH" "$NOTARY_LOG_PATH"
ditto -c -k --keepParent "$BUNDLE_DIR" "$ARCHIVE_PATH"

xcrun notarytool submit \
  "$ARCHIVE_PATH" \
  --key "$APPLE_API_KEY_PATH" \
  --key-id "$APPLE_API_KEY_ID" \
  --issuer "$APPLE_API_ISSUER_ID" \
  --wait \
  --output-format json > "$NOTARY_LOG_PATH"

echo "Notarized archive ready: $ARCHIVE_PATH"
echo "Notary log written to: $NOTARY_LOG_PATH"
