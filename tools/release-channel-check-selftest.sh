#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)
REAL_PYTHON=$(command -v python3.14 || command -v python3.13 || command -v python3.12 || command -v python3.11 || command -v python3)
FAKE_BIN=$(mktemp -d)
trap 'rm -rf "$FAKE_BIN"' EXIT

cat >"$FAKE_BIN/python3.14" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
"$AICX_TEST_REAL_PYTHON" "$@" | sed $'s/$/\r/'
SH
chmod +x "$FAKE_BIN/python3.14"

output=$(
  cd "$ROOT_DIR"
  PATH="$FAKE_BIN:$PATH" AICX_TEST_REAL_PYTHON="$REAL_PYTHON" \
    bash tools/release-channel-check.sh
)

grep -Fq "All release channels in sync:" <<<"$output"
if grep -Fq "mismatch vs workspace" <<<"$output"; then
  echo "release channel checker retained a Windows carriage return" >&2
  exit 1
fi

echo "release channel CRLF self-test passed"
