#!/usr/bin/env bash
set -euo pipefail

# Build a signed + notarized macOS release bundle for aicx.
#
# Inputs are read from either:
#   - KEYS=/path/to/keys-dir
#   - AICX_KEYS_DIR=/path/to/keys-dir
# or fallback:
#   - ~/.keys
#
# Notarization auth is resolved in this order:
#   1. NOTARY_PROFILE / AICX_NOTARY_PROFILE
#   2. NOTARY_KEYCHAIN_PROFILE from KEYS/.notary.env
#   3. NOTARY_APPLE_ID + NOTARY_TEAM_ID + NOTARY_PASSWORD from KEYS/.notary.env

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
REPO_ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

KEYS_DIR="${KEYS:-${AICX_KEYS_DIR:-$HOME/.keys}}"
NOTARY_PROFILE_VALUE="${NOTARY_PROFILE:-${AICX_NOTARY_PROFILE:-}}"
DIST_DIR="${DIST_DIR:-$REPO_ROOT/dist}"
PACKAGE_NAME="${PACKAGE_NAME:-}"
CLEAN_AFTER_BUILD="${AICX_CLEAN_AFTER_BUILD:-1}"
DRY_RUN="${DRY_RUN:-0}"
FEATURES_VALUE="${FEATURES:-${AICX_CARGO_FEATURES:-}}"
NATIVE_VALUE="${NATIVE:-0}"

usage() {
  cat <<'EOF'
Usage:
  make release-bundle [KEYS=/path/to/.keys] [NOTARY_PROFILE=name] [TARGET=<triple>] [NATIVE=1]

Environment:
  KEYS / AICX_KEYS_DIR      Path to keys directory (default: ~/.keys)
  NOTARY_PROFILE            notarytool keychain profile to use
  AICX_NOTARY_PROFILE       env fallback for NOTARY_PROFILE
  TARGET                    Rust target triple (default: host triple)
  DIST_DIR                  Output directory (default: ./dist)
  PACKAGE_NAME              Bundle prefix (default: Cargo package name)
  NATIVE=1                  Build with --features native-embedder (model weights are still not bundled)
  FEATURES                  Explicit Cargo feature list for the bundle build
  AICX_CLEAN_AFTER_BUILD    Run cargo clean --target <triple> after bundle creation (default: 1)
  DRY_RUN=1                 Print resolved actions without signing/notarizing

Expected KEYS directory shape:
  signing-identity.txt
  Certificates.p12
  cert_password.txt
  .notary.env

Optional .notary.env variables:
  NOTARY_KEYCHAIN_PROFILE
  NOTARY_APPLE_ID
  NOTARY_TEAM_ID
  NOTARY_PASSWORD
EOF
}

if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
  usage
  exit 0
fi

require_cmd() {
  if ! command -v "$1" >/dev/null 2>&1; then
    echo "Error: missing required command: $1" >&2
    exit 1
  fi
}

read_trimmed_file() {
  local path="$1"
  if [[ ! -f "$path" ]]; then
    echo "Error: missing required file: $path" >&2
    exit 1
  fi
  python3 - "$path" <<'PY'
from pathlib import Path
import sys
print(Path(sys.argv[1]).read_text(encoding="utf-8", errors="ignore").strip())
PY
}

toml_value() {
  local key="$1"
  python3 - "$REPO_ROOT/Cargo.toml" "$key" <<'PY'
import sys, tomllib
from pathlib import Path
path = Path(sys.argv[1])
key = sys.argv[2]
data = tomllib.load(path.open("rb"))
node = data
for part in key.split("."):
    node = node[part]
print(node)
PY
}

host_target() {
  rustc -vV | sed -n 's/^host: //p'
}

cleanup() {
  local status=$?
  if [[ -n "${TEMP_KEYCHAIN_PATH:-}" && -f "${TEMP_KEYCHAIN_PATH}" ]]; then
    security delete-keychain "${TEMP_KEYCHAIN_PATH}" >/dev/null 2>&1 || true
  fi
  exit $status
}

if [[ "$DRY_RUN" != "1" ]]; then
  trap cleanup EXIT
fi

require_cmd cargo
require_cmd python3

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Error: release-bundle is currently supported only on macOS hosts." >&2
  exit 1
fi

require_cmd codesign
require_cmd security
require_cmd xcrun
require_cmd ditto
require_cmd shasum

TARGET="${TARGET:-$(host_target)}"
if [[ "$TARGET" != *apple-darwin ]]; then
  echo "Error: release-bundle requires an Apple target, got: $TARGET" >&2
  exit 1
fi

KEYS_DIR=$(python3 - "$KEYS_DIR" <<'PY'
import os, sys
print(os.path.expanduser(sys.argv[1]))
PY
)

SIGNING_IDENTITY="${SIGNING_IDENTITY:-}"
if [[ -z "$SIGNING_IDENTITY" ]]; then
  SIGNING_IDENTITY=$(read_trimmed_file "$KEYS_DIR/signing-identity.txt")
fi

CERT_P12="$KEYS_DIR/Certificates.p12"
CERT_PASSWORD=$(read_trimmed_file "$KEYS_DIR/cert_password.txt")
NOTARY_ENV_FILE="$KEYS_DIR/.notary.env"

if [[ -f "$NOTARY_ENV_FILE" ]]; then
  # Local trusted operator-owned file.
  # shellcheck disable=SC1090
  set -a
  source "$NOTARY_ENV_FILE"
  set +a
fi

if [[ -z "$NOTARY_PROFILE_VALUE" && -n "${NOTARY_KEYCHAIN_PROFILE:-}" ]]; then
  NOTARY_PROFILE_VALUE="$NOTARY_KEYCHAIN_PROFILE"
fi

VERSION="$(toml_value package.version)"
if [[ -z "$PACKAGE_NAME" ]]; then
  PACKAGE_NAME="$(toml_value package.name)"
fi
DIST_DIR=$(python3 - "$DIST_DIR" <<'PY'
import os, sys
print(os.path.abspath(os.path.expanduser(sys.argv[1])))
PY
)
mkdir -p "$DIST_DIR"

RELEASE_DIR="$REPO_ROOT/target/$TARGET/release"
if [[ -z "$FEATURES_VALUE" && "$NATIVE_VALUE" == "1" ]]; then
  FEATURES_VALUE="native-embedder"
fi
if [[ -n "$FEATURES_VALUE" ]]; then
  BUILD_FLAVOR="${AICX_BUNDLE_FLAVOR:-native}"
else
  BUILD_FLAVOR="${AICX_BUNDLE_FLAVOR:-slim}"
fi
BUNDLE_BASENAME="${PACKAGE_NAME}-v${VERSION}-${TARGET}-${BUILD_FLAVOR}-signed"
BUNDLE_DIR="$DIST_DIR/$BUNDLE_BASENAME"
ARCHIVE_PATH="$DIST_DIR/${BUNDLE_BASENAME}.zip"
CHECKSUM_PATH="${ARCHIVE_PATH}.sha256"
NOTARY_LOG_PATH="$DIST_DIR/${BUNDLE_BASENAME}.notary.json"

echo "=== AICX production release bundle ==="
echo "Repo:            $REPO_ROOT"
echo "Version:         $VERSION"
echo "Target:          $TARGET"
echo "Build flavor:    $BUILD_FLAVOR"
echo "Cargo features:  ${FEATURES_VALUE:-<none>}"
echo "Cleanup target:  $CLEAN_AFTER_BUILD"
echo "Keys dir:        $KEYS_DIR"
echo "Dist dir:        $DIST_DIR"
echo "Signing identity:$SIGNING_IDENTITY"
if [[ -n "$NOTARY_PROFILE_VALUE" ]]; then
  echo "Notary auth:     keychain profile ($NOTARY_PROFILE_VALUE)"
elif [[ -n "${NOTARY_APPLE_ID:-}" && -n "${NOTARY_TEAM_ID:-}" && -n "${NOTARY_PASSWORD:-}" ]]; then
  echo "Notary auth:     KEYS/.notary.env credentials"
else
  echo "Error: no notarization credentials found. Set NOTARY_PROFILE or provide KEYS/.notary.env." >&2
  exit 1
fi

if [[ "$DRY_RUN" == "1" ]]; then
  echo ""
  echo "[dry-run] Would:"
  if [[ -n "$FEATURES_VALUE" ]]; then
    echo "  1. cargo build --locked --release --target $TARGET --features $FEATURES_VALUE --bin aicx --bin aicx-mcp"
  else
    echo "  1. cargo build --locked --release --target $TARGET --bin aicx --bin aicx-mcp"
  fi
  echo "  2. import $CERT_P12 into a temporary keychain"
  echo "  3. codesign target binaries"
  echo "  4. build $ARCHIVE_PATH"
  echo "  5. notarize via notarytool"
  exit 0
fi

if [[ ! -f "$CERT_P12" ]]; then
  echo "Error: missing signing certificate bundle: $CERT_P12" >&2
  exit 1
fi

echo "[1/6] Building release binaries..."
(
  cd "$REPO_ROOT"
  if [[ -n "$FEATURES_VALUE" ]]; then
    cargo build --locked --release --target "$TARGET" --features "$FEATURES_VALUE" --bin aicx --bin aicx-mcp
  else
    cargo build --locked --release --target "$TARGET" --bin aicx --bin aicx-mcp
  fi
)

rm -rf "$BUNDLE_DIR"
mkdir -p "$BUNDLE_DIR/docs"
cp "$RELEASE_DIR/aicx" "$BUNDLE_DIR/aicx"
cp "$RELEASE_DIR/aicx-mcp" "$BUNDLE_DIR/aicx-mcp"
cp "$REPO_ROOT/LICENSE" "$BUNDLE_DIR/LICENSE"
cp "$REPO_ROOT/README.md" "$BUNDLE_DIR/README.md"
cp "$REPO_ROOT/install.sh" "$BUNDLE_DIR/install.sh"
cp "$REPO_ROOT/docs/COMMANDS.md" "$BUNDLE_DIR/docs/COMMANDS.md"
cp "$REPO_ROOT/docs/RELEASES.md" "$BUNDLE_DIR/docs/RELEASES.md"
chmod +x "$BUNDLE_DIR/install.sh"

echo "[2/6] Preparing temporary signing keychain..."
TEMP_KEYCHAIN_PATH="$DIST_DIR/${BUNDLE_BASENAME}.keychain-db"
TEMP_KEYCHAIN_PASSWORD="$(uuidgen | tr '[:upper:]' '[:lower:]')"
EXISTING_KEYCHAINS="$(security list-keychains -d user | tr -d '"' | tr '\n' ' ')"

security create-keychain -p "$TEMP_KEYCHAIN_PASSWORD" "$TEMP_KEYCHAIN_PATH"
security set-keychain-settings -lut 21600 "$TEMP_KEYCHAIN_PATH"
security unlock-keychain -p "$TEMP_KEYCHAIN_PASSWORD" "$TEMP_KEYCHAIN_PATH"
security list-keychains -d user -s "$TEMP_KEYCHAIN_PATH" $EXISTING_KEYCHAINS >/dev/null
security import "$CERT_P12" \
  -k "$TEMP_KEYCHAIN_PATH" \
  -P "$CERT_PASSWORD" \
  -T /usr/bin/codesign \
  -T /usr/bin/security >/dev/null
security set-key-partition-list \
  -S apple-tool:,apple:,codesign: \
  -s \
  -k "$TEMP_KEYCHAIN_PASSWORD" \
  "$TEMP_KEYCHAIN_PATH" >/dev/null

echo "[3/6] Signing release binaries..."
for binary in "$BUNDLE_DIR/aicx" "$BUNDLE_DIR/aicx-mcp"; do
  codesign \
    --force \
    --timestamp \
    --options runtime \
    --sign "$SIGNING_IDENTITY" \
    --keychain "$TEMP_KEYCHAIN_PATH" \
    "$binary"
  codesign --verify --verbose=2 "$binary" >/dev/null
done

echo "[4/6] Packaging notarization archive..."
rm -f "$ARCHIVE_PATH" "$CHECKSUM_PATH" "$NOTARY_LOG_PATH"
ditto -c -k --keepParent "$BUNDLE_DIR" "$ARCHIVE_PATH"
shasum -a 256 "$ARCHIVE_PATH" > "$CHECKSUM_PATH"

echo "[5/6] Submitting archive for notarization..."
if [[ -n "$NOTARY_PROFILE_VALUE" ]]; then
  xcrun notarytool submit "$ARCHIVE_PATH" \
    --keychain-profile "$NOTARY_PROFILE_VALUE" \
    --wait \
    --output-format json > "$NOTARY_LOG_PATH"
else
  xcrun notarytool submit "$ARCHIVE_PATH" \
    --apple-id "$NOTARY_APPLE_ID" \
    --team-id "$NOTARY_TEAM_ID" \
    --password "$NOTARY_PASSWORD" \
    --wait \
    --output-format json > "$NOTARY_LOG_PATH"
fi

echo "[6/6] Final artifact summary..."
echo "Bundle dir:      $BUNDLE_DIR"
echo "Archive:         $ARCHIVE_PATH"
echo "Checksum:        $CHECKSUM_PATH"
echo "Notary log:      $NOTARY_LOG_PATH"
echo ""
echo "Note: zip archives are notarized server-side but cannot be stapled like .pkg/.dmg/.app."

if [[ "$CLEAN_AFTER_BUILD" == "1" ]]; then
  echo ""
  echo "[post] Cleaning Cargo target dir for $TARGET ..."
  if (
    cd "$REPO_ROOT"
    cargo clean --target "$TARGET"
  ); then
    echo "[post] Cargo target cleaned."
  else
    echo "[post] Warning: cargo clean failed; release bundle is still valid." >&2
  fi
fi
