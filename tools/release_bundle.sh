#!/usr/bin/env bash
set -euo pipefail

# Build an AICX release bundle.
#
# Modes:
#   - default: Apple-codesigned + notarized macOS bundle, GPG-detached
#   - AICX_RELEASE_BUNDLE_ONLY_BINARIES=1: GPG-detached slim tar.gz for
#     non-Apple targets (linux/bsd). No Apple codesign here, but every
#     archive is still GPG-signed — Loctree never ships unsigned artifacts.
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

# Pick the newest Python ≥ 3.11 available. tomllib is stdlib only from 3.11,
# and bash scripts don't inherit .zshrc PATH ordering, so we detect explicitly
# instead of trusting `python3`. Caller can override with PYTHON=...
PYTHON="${PYTHON:-$(command -v python3.14 2>/dev/null \
  || command -v python3.13 2>/dev/null \
  || command -v python3.12 2>/dev/null \
  || command -v python3.11 2>/dev/null \
  || command -v python3 2>/dev/null)}"
if [[ -z "$PYTHON" ]]; then
  echo "Error: no Python interpreter found (need 3.11+ for stdlib tomllib)." >&2
  exit 1
fi
if ! "$PYTHON" -c 'import tomllib' >/dev/null 2>&1; then
  echo "Error: $PYTHON lacks stdlib tomllib — need Python 3.11+." >&2
  exit 1
fi

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
  AICX_CARGO_BUILD_CMD      Explicit build command for binary-only mode.
                            Default: cargo build.
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
  "$PYTHON" - "$path" <<'PY'
from pathlib import Path
import sys
print(Path(sys.argv[1]).read_text(encoding="utf-8", errors="ignore").strip())
PY
}

toml_value() {
  local key="$1"
  "$PYTHON" - "$REPO_ROOT/Cargo.toml" "$key" <<'PY'
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

# Map a rust target triple to a clean release asset name. Linux triples drop
# the cosmetic `unknown` vendor (`x86_64-unknown-linux-gnu` → `x86_64-linux-gnu`).
# Apple targets stay as-is because `apple-darwin` is genuinely informative.
clean_target() {
  case "$1" in
    *-unknown-linux-*) echo "${1//-unknown-linux-/-linux-}" ;;
    *) echo "$1" ;;
  esac
}

write_release_manifest() {
  local output="$1"
  local version="$2"
  local target="$3"
  local flavor="$4"
  local signed="$5"
  local notarized="$6"
  local commit full_commit

  full_commit=$(git -C "$REPO_ROOT" rev-parse HEAD)
  commit=$(git -C "$REPO_ROOT" rev-parse --short=12 HEAD)
  "$PYTHON" - "$output" "$version" "$target" "$flavor" "$signed" "$notarized" "$full_commit" "$commit" <<'PY'
import json
import sys
from pathlib import Path

output = Path(sys.argv[1])
version = sys.argv[2]
target = sys.argv[3]
flavor = sys.argv[4]
signed = sys.argv[5] == "1"
notarized = sys.argv[6] == "1"
full_commit = sys.argv[7]
short_commit = sys.argv[8]

data = {
    "source": "Loctree/aicx",
    "version": version,
    "target": target,
    "flavor": flavor,
    "commit": full_commit,
    "short_commit": short_commit,
    "signed": signed,
    "notarized": notarized,
    "components": [
        {
            "name": "aicx",
            "version": version,
            "source": "Loctree/aicx",
            "commit": full_commit,
            "short_commit": short_commit,
        },
        {
            "name": "aicx-mcp",
            "version": version,
            "source": "Loctree/aicx",
            "commit": full_commit,
            "short_commit": short_commit,
        },
    ],
}
output.write_text(json.dumps(data, indent=2) + "\n", encoding="utf-8")
PY
}

cleanup() {
  local status=$?
  # Restore the operator's original default keychain before tearing the
  # temporary one down (see the default-keychain switch in the signing path).
  if [[ -n "${ORIGINAL_DEFAULT_KEYCHAIN:-}" ]]; then
    security default-keychain -s "${ORIGINAL_DEFAULT_KEYCHAIN}" >/dev/null 2>&1 || true
  fi
  if [[ -n "${TEMP_KEYCHAIN_PATH:-}" && -f "${TEMP_KEYCHAIN_PATH}" ]]; then
    security delete-keychain "${TEMP_KEYCHAIN_PATH}" >/dev/null 2>&1 || true
  fi
  exit $status
}

if [[ "$DRY_RUN" != "1" ]]; then
  trap cleanup EXIT
fi

require_cmd cargo

require_cmd shasum

if [[ "${AICX_RELEASE_BUNDLE_ONLY_BINARIES:-0}" == "1" ]]; then
  echo "=== AICX GPG-signed bundle (binaries-only, no Apple codesign) ==="
  TARGET="${TARGET:-$(host_target)}"
  # Windows targets ARE supported on this path: the staging/zip/sign logic
  # below branches on `*windows*` (.exe suffix, portable zip, no install.sh).
  # The earlier blanket `exit 1` guard was a stale leftover from before that
  # branching landed and is intentionally gone.

  VERSION="$(toml_value package.version)"
  if [[ -z "$PACKAGE_NAME" ]]; then
    PACKAGE_NAME="$(toml_value package.name)"
  fi
  DIST_DIR=$("$PYTHON" - "$DIST_DIR" <<'PY'
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
  HOST_TARGET="$(host_target)"
  BUILD_CMD="${AICX_CARGO_BUILD_CMD:-cargo build}"
  if [[ -z "${AICX_CARGO_BUILD_CMD:-}" && "$TARGET" != "$HOST_TARGET" ]]; then
    cat >&2 <<EOF
Error: binary-only release bundles default to native builds.
Host target: $HOST_TARGET
Requested:   $TARGET

Use a Linux runner matching TARGET for Linux release assets, or set
AICX_CARGO_BUILD_CMD explicitly to opt into cross-compilation.
EOF
    exit 1
  fi
  ASSET_TARGET="$(clean_target "$TARGET")"
  # Loctree releases are never shipped unsigned — GPG detached-sign is
  # mandatory below. The basename intentionally omits any "-unsigned" tag.
  BUNDLE_BASENAME="${PACKAGE_NAME}-v${VERSION}-${ASSET_TARGET}-${BUILD_FLAVOR}"
  BUNDLE_DIR="$DIST_DIR/$BUNDLE_BASENAME"
  # Windows targets produce .zip + .exe binaries; everything else stays tar.gz.
  if [[ "$TARGET" == *windows* ]]; then
    EXE_SUFFIX=".exe"
    ARCHIVE_PATH="$DIST_DIR/${BUNDLE_BASENAME}.zip"
  else
    EXE_SUFFIX=""
    ARCHIVE_PATH="$DIST_DIR/${BUNDLE_BASENAME}.tar.gz"
  fi
  CHECKSUM_PATH="${ARCHIVE_PATH}.sha256"

  echo "Repo:            $REPO_ROOT"
  echo "Version:         $VERSION"
  echo "Target:          $TARGET"
  echo "Build command:   $BUILD_CMD"
  echo "Build flavor:    $BUILD_FLAVOR"
  echo "Cargo features:  ${FEATURES_VALUE:-<none>}"
  echo "Cleanup target:  $CLEAN_AFTER_BUILD"
  echo "Dist dir:        $DIST_DIR"
  echo "Archive:         $ARCHIVE_PATH"

  if [[ "$DRY_RUN" == "1" ]]; then
    echo ""
    echo "[dry-run] Would:"
    if [[ -n "$FEATURES_VALUE" ]]; then
      echo "  1. $BUILD_CMD --locked --release --target $TARGET --features $FEATURES_VALUE --bin aicx --bin aicx-mcp"
    else
      echo "  1. $BUILD_CMD --locked --release --target $TARGET --bin aicx --bin aicx-mcp"
    fi
    echo "  2. compose $BUNDLE_DIR layout (bin + LICENSE + README + install.sh + docs)"
    echo "  3. tar -czf $ARCHIVE_PATH"
    echo "  4. shasum -a 256 -> $CHECKSUM_PATH"
    exit 0
  fi

  echo "[1/3] Building release binaries..."
  (
    cd "$REPO_ROOT"
    if [[ -n "$FEATURES_VALUE" ]]; then
      # shellcheck disable=SC2086
      $BUILD_CMD --locked --release --target "$TARGET" --features "$FEATURES_VALUE" --bin aicx --bin aicx-mcp
    else
      # shellcheck disable=SC2086
      $BUILD_CMD --locked --release --target "$TARGET" --bin aicx --bin aicx-mcp
    fi
  )

  rm -rf "$BUNDLE_DIR"
  mkdir -p "$BUNDLE_DIR/docs"
  cp "$RELEASE_DIR/aicx${EXE_SUFFIX}" "$BUNDLE_DIR/aicx${EXE_SUFFIX}"
  cp "$RELEASE_DIR/aicx-mcp${EXE_SUFFIX}" "$BUNDLE_DIR/aicx-mcp${EXE_SUFFIX}"
  cp "$REPO_ROOT/LICENSE" "$BUNDLE_DIR/LICENSE"
  cp "$REPO_ROOT/README.md" "$BUNDLE_DIR/README.md"
  cp "$REPO_ROOT/docs/COMMANDS.md" "$BUNDLE_DIR/docs/COMMANDS.md"
  cp "$REPO_ROOT/docs/RELEASES.md" "$BUNDLE_DIR/docs/RELEASES.md"
  write_release_manifest "$BUNDLE_DIR/release-manifest.json" "$VERSION" "$TARGET" "$BUILD_FLAVOR" 0 0
  if [[ "$TARGET" == *windows* ]]; then
    # No install.sh on Windows; PowerShell users invoke the .exe directly.
    :
  else
    cp "$REPO_ROOT/install.sh" "$BUNDLE_DIR/install.sh"
    chmod +x "$BUNDLE_DIR/install.sh"
  fi

  echo "[2/3] Packaging archive..."
  rm -f "$ARCHIVE_PATH" "$CHECKSUM_PATH" "${ARCHIVE_PATH}.asc"
  if [[ "$TARGET" == *windows* ]]; then
    # Portable zip via Python stdlib — works on every runner regardless of
    # whether `zip` is on PATH.
    ( cd "$DIST_DIR" && "$PYTHON" -c "
import sys, zipfile, os
src = sys.argv[1]
dst = sys.argv[2]
with zipfile.ZipFile(dst, 'w', zipfile.ZIP_DEFLATED) as zf:
    for root, _, files in os.walk(src):
        for f in files:
            full = os.path.join(root, f)
            zf.write(full, os.path.relpath(full, os.path.dirname(src)))
" "$BUNDLE_BASENAME" "$(basename "$ARCHIVE_PATH")" )
  else
    (cd "$DIST_DIR" && tar -czf "$ARCHIVE_PATH" "$BUNDLE_BASENAME")
  fi
  (cd "$DIST_DIR" && shasum -a 256 "$(basename "$ARCHIVE_PATH")") > "$CHECKSUM_PATH"

  # Bundle staging directory only matters for inspection; remove it so it
  # never reaches `gh release upload` (which fails on directories).
  rm -rf "$BUNDLE_DIR"

  # GPG detached signature: mandatory on real release (tag push, release.yml).
  # In CI contexts that don't carry the signing key (e.g. merge_queue_gate),
  # set AICX_RELEASE_SKIP_SIGNING=1 to produce an unsigned bundle for
  # packaging-shape verification only. Loctree real releases never ship
  # unsigned — release.yml asserts the key is present before this script runs.
  if [[ "${AICX_RELEASE_SKIP_SIGNING:-0}" == "1" ]]; then
    echo "[2b/3] AICX_RELEASE_SKIP_SIGNING=1 — bundling without GPG signature (CI gate context)"
    echo "       This bundle MUST NOT be uploaded as a real release artifact."
  else
    if [[ -z "${LOCTREE_GPG_KEY_ID:-}" ]]; then
      echo "Error: LOCTREE_GPG_KEY_ID is not set — refusing to produce an unsigned release archive." >&2
      echo "Export it in your shell (e.g. .zshrc) or pass LOCTREE_GPG_KEY_ID=... inline." >&2
      echo "Set AICX_RELEASE_SKIP_SIGNING=1 to bypass for non-release builds (e.g. merge queue gate)." >&2
      exit 1
    fi
    PASSPHRASE_FILE="${LOCTREE_GPG_PASSPHRASE_FILE:-$HOME/.keys/.gpg.passphrase}"
    if [[ ! -r "$PASSPHRASE_FILE" ]]; then
      echo "Error: GPG passphrase file not readable at $PASSPHRASE_FILE." >&2
      echo "Provide LOCTREE_GPG_PASSPHRASE_FILE=/path or place the passphrase at \$HOME/.keys/.gpg.passphrase (mode 600)." >&2
      exit 1
    fi
    require_cmd gpg
    if ! gpg --list-secret-keys "${LOCTREE_GPG_KEY_ID}" >/dev/null 2>&1; then
      echo "Error: GPG secret key ${LOCTREE_GPG_KEY_ID} not imported on this host." >&2
      exit 1
    fi
    echo "[2b/3] GPG detached-signing archive with ${LOCTREE_GPG_KEY_ID}..."
    gpg --batch --yes --armor --pinentry-mode loopback \
      --passphrase-file "$PASSPHRASE_FILE" \
      --sig-notation "apple-developer-team-id@loctree.io=MW223P3NPX" \
      --sig-notation "release-source@loctree.io=Loctree/aicx" \
      --detach-sign --local-user "${LOCTREE_GPG_KEY_ID}" \
      "$ARCHIVE_PATH"

    # Export public key alongside artifacts so consumers can import + verify
    # without keyserver lookup. ASCII armor, small, redistributable.
    gpg --export --armor "${LOCTREE_GPG_KEY_ID}" \
      > "${DIST_DIR}/loctree-release-pubkey.asc"
  fi

  echo "[3/3] Final artifact summary..."
  echo "Archive:         $ARCHIVE_PATH"
  echo "Checksum:        $CHECKSUM_PATH"
  if [[ -f "${ARCHIVE_PATH}.asc" ]]; then
    echo "Signature:       ${ARCHIVE_PATH}.asc"
  fi
  echo ""
  echo "Note: this archive is GPG-signed (.asc) but not Apple-codesigned/notarized."
  echo "      For Apple-notarized macOS builds use \`make release-bundle KEYS=...\`."

  if [[ "$CLEAN_AFTER_BUILD" == "1" ]]; then
    echo ""
    echo "[post] Cleaning Cargo target dir for $TARGET ..."
    if (
      cd "$REPO_ROOT"
      cargo clean --target "$TARGET"
    ); then
      echo "[post] Cargo target cleaned."
    else
      echo "[post] Warning: cargo clean failed; bundle is still valid." >&2
    fi
  fi

  exit 0
fi

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "Error: release-bundle is currently supported only on macOS hosts." >&2
  exit 1
fi

require_cmd codesign
require_cmd security
require_cmd xcrun
require_cmd ditto

TARGET="${TARGET:-$(host_target)}"
if [[ "$TARGET" != *apple-darwin ]]; then
  echo "Error: release-bundle requires an Apple target, got: $TARGET" >&2
  exit 1
fi

KEYS_DIR=$("$PYTHON" - "$KEYS_DIR" <<'PY'
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
DIST_DIR=$("$PYTHON" - "$DIST_DIR" <<'PY'
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
ASSET_TARGET="$(clean_target "$TARGET")"
# Signed+notarized macOS bundle. Basename intentionally omits "-signed" —
# every Loctree release archive is signed by definition (Apple codesign +
# GPG detached). The .zip extension + .asc sidecar carry the proof.
BUNDLE_BASENAME="${PACKAGE_NAME}-v${VERSION}-${ASSET_TARGET}-${BUILD_FLAVOR}"
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
write_release_manifest "$BUNDLE_DIR/release-manifest.json" "$VERSION" "$TARGET" "$BUILD_FLAVOR" 1 1
chmod +x "$BUNDLE_DIR/install.sh"

echo "[2/6] Preparing temporary signing keychain..."
TEMP_KEYCHAIN_PATH="$DIST_DIR/${BUNDLE_BASENAME}.keychain-db"
TEMP_KEYCHAIN_PASSWORD="$(uuidgen | tr '[:upper:]' '[:lower:]')"
EXISTING_KEYCHAINS="$(security list-keychains -d user | tr -d '"' | tr '\n' ' ')"

security create-keychain -p "$TEMP_KEYCHAIN_PASSWORD" "$TEMP_KEYCHAIN_PATH"
security set-keychain-settings -lut 21600 "$TEMP_KEYCHAIN_PATH"
security unlock-keychain -p "$TEMP_KEYCHAIN_PASSWORD" "$TEMP_KEYCHAIN_PATH"
security list-keychains -d user -s "$TEMP_KEYCHAIN_PATH" $EXISTING_KEYCHAINS >/dev/null
# Make the temp keychain the default. Under a launchd runner session
# (`SessionCreate=true`), codesign resolves a signing identity BY NAME from
# the default keychain only — the search-list set above is not consulted, so
# without this codesign reports "no identity found" even though the import
# succeeded. `cleanup` restores the original default on exit.
# A non-interactive runner session may have no default keychain at all
# (`SecKeychainCopyDomainDefault user: A default keychain could not be found`),
# which would abort under `set -e`. Tolerate the empty case; cleanup only
# restores when a previous default actually existed.
ORIGINAL_DEFAULT_KEYCHAIN="$(security default-keychain -d user 2>/dev/null | tr -d ' "' || true)"
security default-keychain -d user -s "$TEMP_KEYCHAIN_PATH"
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
rm -f "$ARCHIVE_PATH" "$CHECKSUM_PATH" "$NOTARY_LOG_PATH" "${ARCHIVE_PATH}.asc"
ditto -c -k --keepParent "$BUNDLE_DIR" "$ARCHIVE_PATH"
shasum -a 256 "$ARCHIVE_PATH" > "$CHECKSUM_PATH"

# GPG detached signature is mandatory — Loctree releases never ship unsigned.
if [[ -z "${LOCTREE_GPG_KEY_ID:-}" ]]; then
  echo "Error: LOCTREE_GPG_KEY_ID is not set — refusing to produce an unsigned signed/notarized bundle." >&2
  exit 1
fi
PASSPHRASE_FILE="${LOCTREE_GPG_PASSPHRASE_FILE:-$HOME/.keys/.gpg.passphrase}"
if [[ ! -r "$PASSPHRASE_FILE" ]]; then
  echo "Error: GPG passphrase file not readable at $PASSPHRASE_FILE." >&2
  exit 1
fi
require_cmd gpg
if ! gpg --list-secret-keys "${LOCTREE_GPG_KEY_ID}" >/dev/null 2>&1; then
  echo "Error: GPG secret key ${LOCTREE_GPG_KEY_ID} not imported on this host." >&2
  exit 1
fi
echo "  GPG detached-signing zip with ${LOCTREE_GPG_KEY_ID}..."
gpg --batch --yes --armor --pinentry-mode loopback \
  --passphrase-file "$PASSPHRASE_FILE" \
  --sig-notation "apple-developer-team-id@loctree.io=MW223P3NPX" \
  --sig-notation "release-source@loctree.io=Loctree/aicx" \
  --detach-sign --local-user "${LOCTREE_GPG_KEY_ID}" \
  "$ARCHIVE_PATH"

# Export public key alongside the notarized macOS bundle for verification.
gpg --export --armor "${LOCTREE_GPG_KEY_ID}" \
  > "${DIST_DIR}/loctree-release-pubkey.asc"

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

# Bundle staging directory only matters for inspection; remove it so it
# never reaches `gh release upload` (which fails on directories).
rm -rf "$BUNDLE_DIR"

echo "[6/6] Final artifact summary..."
echo "Archive:         $ARCHIVE_PATH"
echo "Checksum:        $CHECKSUM_PATH"
echo "Notary log:      $NOTARY_LOG_PATH"
if [[ -f "${ARCHIVE_PATH}.asc" ]]; then
  echo "Signature:       ${ARCHIVE_PATH}.asc"
fi
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
