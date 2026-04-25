#!/usr/bin/env bash
set -euo pipefail

# aicx setup — install binaries + configure MCP for supported AI tools
#
# Usage:
#   bash install.sh
#   bash install.sh --skip-install  # MCP config only
# Run from a local checkout when crates.io / release artifacts are not your install path yet.
#
# Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

SOURCE_PATH="${BASH_SOURCE[0]:-$0}"
if [ -n "$SOURCE_PATH" ] && [ -e "$SOURCE_PATH" ]; then
  SCRIPT_DIR=$(cd "$(dirname "$SOURCE_PATH")" && pwd)
else
  SCRIPT_DIR="$PWD"
fi
MANIFEST_PATH="$SCRIPT_DIR/Cargo.toml"
HAS_LOCAL_MANIFEST=0
if [ -f "$MANIFEST_PATH" ]; then
  HAS_LOCAL_MANIFEST=1
fi
HAS_BUNDLED_BINARIES=0
if [ -x "$SCRIPT_DIR/aicx" ] && [ -x "$SCRIPT_DIR/aicx-mcp" ]; then
  HAS_BUNDLED_BINARIES=1
fi
AICX_INSTALL_MODE="${AICX_INSTALL_MODE:-auto}"
AICX_GIT_URL="${AICX_GIT_URL:-https://github.com/Loctree/aicx}"
AICX_BIN_DIR="${AICX_BIN_DIR:-$HOME/.local/bin}"
AICX_RELEASE_REPO="${AICX_RELEASE_REPO:-Loctree/aicx}"
AICX_RELEASE_TAG="${AICX_RELEASE_TAG:-latest}"
AICX_EMBEDDER_PICKER="${AICX_EMBEDDER_PICKER:-auto}"
AICX_EMBEDDER_PROFILE="${AICX_EMBEDDER_PROFILE:-}"
AICX_EMBEDDER_CONFIG_PATH="${AICX_EMBEDDER_CONFIG_PATH:-$HOME/.aicx/embedder.toml}"

SKIP_INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --skip-install) SKIP_INSTALL=1 ;;
    --release) AICX_INSTALL_MODE="release" ;;
    --release-tag=*) AICX_RELEASE_TAG="${arg#*=}" ;;
    --pick-embedder) AICX_EMBEDDER_PICKER="1" ;;
    --no-embedder-prompt) AICX_EMBEDDER_PICKER="0" ;;
    --embedder-profile=*) AICX_EMBEDDER_PROFILE="${arg#*=}" ;;
    --help|-h)
      echo "Usage: install.sh [--skip-install]"
      echo "  Install aicx + aicx-mcp and configure MCP for Claude Code, Codex, and Gemini."
      echo "  Run from a release bundle or the repo root / local checkout."
      echo ""
      echo "Install source is controlled by AICX_INSTALL_MODE:"
      echo "  auto    - prefer bundled binaries, then local checkout, otherwise crates.io"
      echo "  release - download an official GitHub Release, verify SHA256, then install its bundle"
      echo "  bundle  - copy bundled binaries into \$AICX_BIN_DIR"
      echo "  local   - cargo install --path <checkout> --locked"
      echo "  crates  - cargo install aicx --locked"
      echo "  git     - cargo install --git \$AICX_GIT_URL --locked aicx"
      echo ""
      echo "Bundle install target:"
      echo "  AICX_BIN_DIR=\$HOME/.local/bin   # default destination for bundled binaries"
      echo ""
      echo "Release download target:"
      echo "  AICX_RELEASE_REPO=Loctree/aicx"
      echo "  AICX_RELEASE_TAG=latest          # or vX.Y.Z"
      echo ""
      echo "Runtime/build profile shortcuts:"
      echo "  default runtime:  AICX_RUNTIME_PROFILE=base    # portable 1024-dim preset"
      echo "  heavier runtime:  AICX_RUNTIME_PROFILE=dev     # 2560-dim Qwen 4B preset"
      echo "  premium runtime:  AICX_RUNTIME_PROFILE=premium # 4096-dim Qwen 8B preset"
      echo "  native builds:    AICX_BUILD_PROFILE=dev cargo build --release --features native-embedder"
      echo ""
      echo "Optional native embedder picker:"
      echo "  --pick-embedder                    # interactive config for ~/.aicx/embedder.toml"
      echo "  --embedder-profile=base|dev|premium"
      echo "  --no-embedder-prompt               # suppress interactive picker"
      echo "  note: this does not change rust-memex/Qwen provider settings for memex-sync"
      exit 0
      ;;
  esac
done

resolve_aicx() {
  if [ -x "$AICX_BIN_DIR/aicx" ]; then
    AICX_RUN=("$AICX_BIN_DIR/aicx")
    return 0
  fi

  if command -v aicx >/dev/null 2>&1; then
    AICX_RUN=("aicx")
    return 0
  fi

  if [ "$HAS_LOCAL_MANIFEST" -eq 1 ] && command -v cargo >/dev/null 2>&1; then
    AICX_RUN=("cargo" "run" "--quiet" "--manifest-path" "$MANIFEST_PATH" "--bin" "aicx" "--")
    return 0
  fi

  return 1
}

resolve_aicx_mcp() {
  if [ -x "$AICX_BIN_DIR/aicx-mcp" ]; then
    AICX_MCP_COMMAND="$AICX_BIN_DIR/aicx-mcp"
    AICX_MCP_ARGS_JSON='[]'
    return 0
  fi

  if command -v aicx-mcp >/dev/null 2>&1; then
    AICX_MCP_COMMAND=$(command -v aicx-mcp)
    AICX_MCP_ARGS_JSON='[]'
    return 0
  fi

  if [ "$HAS_LOCAL_MANIFEST" -eq 1 ] && command -v cargo >/dev/null 2>&1; then
    AICX_MCP_COMMAND="cargo"
    AICX_MCP_ARGS_JSON=$(AICX_MANIFEST_PATH="$MANIFEST_PATH" python3 - <<'PY'
import json
import os

print(json.dumps([
    "run",
    "--quiet",
    "--manifest-path",
    os.environ["AICX_MANIFEST_PATH"],
    "--bin",
    "aicx-mcp",
    "--",
]))
PY
)
    return 0
  fi

  return 1
}

echo "=== aicx setup ==="

normalise_bool() {
  case "${1:-}" in
    1|true|TRUE|yes|YES|on|ON) echo "1" ;;
    0|false|FALSE|no|NO|off|OFF) echo "0" ;;
    *) echo "${1:-}" ;;
  esac
}

embedder_repo_for_profile() {
  case "${1:-}" in
    base) echo "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2" ;;
    dev) echo "microsoft/harrier-oss-v1-0.6b" ;;
    premium) echo "codefuse-ai/F2LLM-v2-1.7B" ;;
    *) return 1 ;;
  esac
}

write_embedder_config() {
  local profile="$1"
  local repo="$2"
  local path_override="$3"
  local config_path="$AICX_EMBEDDER_CONFIG_PATH"

  mkdir -p "$(dirname "$config_path")"
  EMBEDDER_CONFIG_PATH="$config_path" \
  EMBEDDER_PROFILE="$profile" \
  EMBEDDER_REPO="$repo" \
  EMBEDDER_PATH_OVERRIDE="$path_override" \
  python3 - <<'PY'
from pathlib import Path
import os

config_path = Path(os.environ["EMBEDDER_CONFIG_PATH"]).expanduser()
profile = os.environ["EMBEDDER_PROFILE"].strip()
repo = os.environ["EMBEDDER_REPO"].strip()
path_override = os.environ["EMBEDDER_PATH_OVERRIDE"].strip()

lines = [
    "# aicx native embedder preferences",
    "# This file is read by native-embedder builds and releases.",
    "",
    "[native_embedder]",
]

if profile:
    lines.append(f'profile = "{profile}"')
if repo:
    lines.append(f'repo = "{repo}"')
if path_override:
    lines.append(f'path = "{path_override}"')
lines.append("prefer_embedded = true")

config_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
print(config_path)
PY
}

maybe_prime_embedder_cache() {
  local repo="$1"

  if [ -z "$repo" ] || ! [ -t 0 ] || ! [ -t 1 ]; then
    return 0
  fi

  if ! command -v hf >/dev/null 2>&1; then
    echo "  native embedder cache not primed automatically (missing 'hf' CLI)"
    echo "  later, run: hf download $repo"
    return 0
  fi

  printf "  Download native embedder model into local HF cache now? [y/N] "
  read -r reply || true
  case "${reply:-}" in
    y|Y|yes|YES)
      echo "  priming HF cache for $repo ..."
      if hf download "$repo"; then
        echo "  HF cache primed"
      else
        echo "  warning: hf download failed; config was still saved" >&2
      fi
      ;;
    *)
      echo "  skipping model download"
      echo "  later, run: hf download $repo"
      ;;
  esac
}

maybe_configure_native_embedder() {
  local picker explicit_profile selected_profile selected_repo config_written
  picker=$(normalise_bool "$AICX_EMBEDDER_PICKER")
  explicit_profile="${AICX_EMBEDDER_PROFILE:-}"
  selected_profile=""
  selected_repo="${AICX_EMBEDDER_REPO:-}"
  config_written=""

  if [ -n "${AICX_EMBEDDER_PATH:-}" ]; then
    config_written=$(write_embedder_config "" "" "$AICX_EMBEDDER_PATH")
    echo "  native embedder path pinned in $config_written"
    return 0
  fi

  if [ -n "$selected_repo" ]; then
    config_written=$(write_embedder_config "" "$selected_repo" "")
    echo "  native embedder repo pinned in $config_written"
    maybe_prime_embedder_cache "$selected_repo"
    return 0
  fi

  if [ -n "$explicit_profile" ]; then
    case "$explicit_profile" in
      none|off|skip)
        echo "  native embedder picker: skipped by explicit profile"
        return 0
        ;;
      base|dev|premium)
        selected_profile="$explicit_profile"
        ;;
      *)
        echo "Error: unsupported --embedder-profile='$explicit_profile' (expected base, dev, premium, or none)." >&2
        exit 1
        ;;
    esac
  elif [ "$picker" = "1" ] || { [ "$picker" = "auto" ] && [ -t 0 ] && [ -t 1 ] && [ -z "${CI:-}" ]; }; then
    echo ""
    echo "Optional native embedder setup"
    echo "  This does not bloat the installed bundle."
    echo "  It only writes ~/.aicx/embedder.toml and can optionally prime the HF cache."
    echo "  Current public release bundles stay slim; native-embedder activation remains opt-in."
    echo ""
    echo "  1) skip"
    echo "  2) base    - MiniLM (~224 MB, safest default)"
    echo "  3) dev     - Harrier 0.6B (~1.1 GB, stronger workstation tier)"
    echo "  4) premium - F2 1.7B (~3.4 GB, runtime-only heavy tier)"
    printf "Choose native embedder profile [1-4]: "
    read -r reply || true
    case "${reply:-1}" in
      1|"")
        echo "  native embedder picker: skipped"
        return 0
        ;;
      2) selected_profile="base" ;;
      3) selected_profile="dev" ;;
      4) selected_profile="premium" ;;
      *)
        echo "  native embedder picker: invalid choice, skipping"
        return 0
        ;;
    esac
  else
    return 0
  fi

  selected_repo=$(embedder_repo_for_profile "$selected_profile")
  config_written=$(write_embedder_config "$selected_profile" "$selected_repo" "")
  echo "  native embedder preference saved to $config_written"
  maybe_prime_embedder_cache "$selected_repo"
}

cleanup_old_binaries() {
  local path="$1"
  if [ -L "$path" ] || [ -f "$path" ]; then
    rm -f "$path"
    echo "  removed stale $(basename "$path") from $(dirname "$path")"
  fi
}

path_has_dir() {
  case ":$PATH:" in
    *":$1:"*) return 0 ;;
    *) return 1 ;;
  esac
}

install_bundle_binaries() {
  local install_dir="$AICX_BIN_DIR"
  local source_aicx="$SCRIPT_DIR/aicx"
  local source_mcp="$SCRIPT_DIR/aicx-mcp"

  if [ "$HAS_BUNDLED_BINARIES" -ne 1 ]; then
    echo "Error: bundle install requested, but prebuilt aicx binaries are not present next to install.sh." >&2
    exit 1
  fi

  mkdir -p "$install_dir"
  echo "  target bin dir: $install_dir"
  echo "  pruning stale user-local / cargo installs..."
  cleanup_old_binaries "$install_dir/aicx"
  cleanup_old_binaries "$install_dir/aicx-mcp"
  cleanup_old_binaries "$install_dir/ai-contexters"
  cleanup_old_binaries "$install_dir/ai-contexters-mcp"
  cleanup_old_binaries "$HOME/.cargo/bin/aicx"
  cleanup_old_binaries "$HOME/.cargo/bin/aicx-mcp"
  cleanup_old_binaries "$HOME/.cargo/bin/ai-contexters"
  cleanup_old_binaries "$HOME/.cargo/bin/ai-contexters-mcp"

  install -m 755 "$source_aicx" "$install_dir/aicx"
  install -m 755 "$source_mcp" "$install_dir/aicx-mcp"
  echo "  installed bundled binaries into $install_dir"
}

detect_release_target() {
  local os arch
  os=$(uname -s)
  arch=$(uname -m)
  case "${os}:${arch}" in
    Darwin:arm64) echo "aarch64-apple-darwin" ;;
    Darwin:x86_64) echo "x86_64-apple-darwin" ;;
    Linux:x86_64) echo "x86_64-unknown-linux-musl" ;;
    *)
      echo "Error: unsupported platform for release installer: ${os}/${arch}" >&2
      echo "  Use a local bundle install instead, or install from source with cargo." >&2
      exit 1
      ;;
  esac
}

detect_release_archive_ext() {
  case "$(uname -s)" in
    Darwin) echo "zip" ;;
    Linux) echo "tar.gz" ;;
    *)
      echo "Error: unsupported archive format for platform $(uname -s)" >&2
      exit 1
      ;;
  esac
}

resolve_release_tag() {
  if [ "$AICX_RELEASE_TAG" != "latest" ]; then
    echo "$AICX_RELEASE_TAG"
    return 0
  fi

  if ! command -v curl >/dev/null 2>&1; then
    echo "Error: curl is required to resolve the latest GitHub release tag." >&2
    exit 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "Error: python3 is required to resolve the latest GitHub release tag." >&2
    exit 1
  fi

  curl -fsSL "https://api.github.com/repos/${AICX_RELEASE_REPO}/releases/latest" | python3 -c '
import json
import sys

data = json.load(sys.stdin)
tag = data.get("tag_name")
if not tag:
    raise SystemExit("GitHub API response did not include tag_name")
print(tag)
'
}

download_release_bundle() {
  local release_tag target version archive_ext archive_name base_url tmp_dir archive_path checksum_path bundle_dir

  if ! command -v curl >/dev/null 2>&1; then
    echo "Error: curl is required for release installer mode." >&2
    exit 1
  fi
  if ! command -v python3 >/dev/null 2>&1; then
    echo "Error: python3 is required for release installer mode." >&2
    exit 1
  fi
  release_tag=$(resolve_release_tag)
  target=$(detect_release_target)
  version="${release_tag#v}"
  archive_ext=$(detect_release_archive_ext)
  archive_name="aicx-v${version}-${target}.${archive_ext}"
  base_url="https://github.com/${AICX_RELEASE_REPO}/releases/download/${release_tag}"
  tmp_dir=$(mktemp -d "${TMPDIR:-/tmp}/aicx-release-install.XXXXXX")
  archive_path="$tmp_dir/$archive_name"
  checksum_path="$tmp_dir/${archive_name}.sha256"

  cleanup_release_tmp() {
    rm -rf "$tmp_dir"
  }
  trap cleanup_release_tmp EXIT

  echo "[1/4] Downloading verified release bundle..."
  echo "  release tag: $release_tag"
  echo "  target:      $target"
  curl -fsSL "$base_url/$archive_name" -o "$archive_path"
  curl -fsSL "$base_url/${archive_name}.sha256" -o "$checksum_path"

  echo "[2/4] Verifying SHA256..."
  ARCHIVE_PATH="$archive_path" CHECKSUM_PATH="$checksum_path" python3 - <<'PY'
import hashlib
import os
from pathlib import Path

archive = Path(os.environ["ARCHIVE_PATH"])
checksum = Path(os.environ["CHECKSUM_PATH"]).read_text(encoding="utf-8").strip().split()[0]
digest = hashlib.sha256(archive.read_bytes()).hexdigest()
if digest != checksum:
    raise SystemExit(
        f"SHA256 mismatch for {archive.name}: expected {checksum}, got {digest}"
    )
print(f"  checksum ok: {archive.name}")
PY

  echo "[3/4] Extracting release bundle..."
  ARCHIVE_PATH="$archive_path" DEST_DIR="$tmp_dir" python3 - <<'PY'
import os
import tarfile
import zipfile
from pathlib import Path

archive_path = Path(os.environ["ARCHIVE_PATH"])
dest_dir = Path(os.environ["DEST_DIR"])

if archive_path.name.endswith(".zip"):
    with zipfile.ZipFile(archive_path) as archive:
        archive.extractall(dest_dir)
elif archive_path.name.endswith(".tar.gz"):
    with tarfile.open(archive_path, "r:gz") as archive:
        archive.extractall(dest_dir)
else:
    raise SystemExit(f"Unsupported archive format: {archive_path.name}")
PY
  bundle_dir="$tmp_dir/aicx-v${version}-${target}"
  if [ ! -f "$bundle_dir/install.sh" ]; then
    echo "Error: release bundle does not contain install.sh: $bundle_dir" >&2
    exit 1
  fi

  echo "[4/4] Delegating to bundled installer..."
  AICX_INSTALL_MODE="bundle" \
  AICX_BIN_DIR="$AICX_BIN_DIR" \
  bash "$bundle_dir/install.sh"
  exit 0
}

resolve_install_mode() {
  case "$AICX_INSTALL_MODE" in
    auto)
      if [ "$HAS_BUNDLED_BINARIES" -eq 1 ]; then
        echo "bundle"
      elif [ "$HAS_LOCAL_MANIFEST" -eq 1 ]; then
        echo "local"
      else
        echo "crates"
      fi
      ;;
    release|bundle|local|crates|git)
      echo "$AICX_INSTALL_MODE"
      ;;
    *)
      echo "Error: unsupported AICX_INSTALL_MODE='$AICX_INSTALL_MODE' (expected auto, release, bundle, local, crates, or git)." >&2
      exit 1
      ;;
  esac
}

INSTALL_MODE=$(resolve_install_mode)
if [ "$INSTALL_MODE" = "release" ]; then
  if [ "$SKIP_INSTALL" -eq 1 ]; then
    echo "Error: --skip-install cannot be combined with release download mode." >&2
    exit 1
  fi
  download_release_bundle
fi

# --- Step 1: Install binaries ---
if [ "$SKIP_INSTALL" -eq 0 ]; then
  if [ "$INSTALL_MODE" != "bundle" ] && ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Install Rust first: https://rustup.rs"
    exit 1
  fi

  # Show live compilation progress: count Compiling lines → [1/4] Compiling... (N crates)
  cargo_install_with_progress() {
    local total=0
    "$@" 2>&1 | while IFS= read -r line; do
      case "$line" in
        *Compiling*)
          total=$((total + 1))
          printf '\r  Compiling... (%d crates)' "$total" >&2
          ;;
        *Finished*|*Installing*|*Installed*|*Replacing*)
          printf '\r  %s\n' "$line" >&2
          ;;
      esac
    done
    printf '\n' >&2
  }

  if [ "$INSTALL_MODE" = "bundle" ]; then
    echo "[1/4] Installing bundled aicx + aicx-mcp into $AICX_BIN_DIR..."
    install_bundle_binaries
  elif [ "$INSTALL_MODE" = "local" ]; then
    echo "[1/4] Installing aicx + aicx-mcp from this checkout..."
    cargo_install_with_progress cargo install --path "$SCRIPT_DIR" --locked --force --bin aicx --bin aicx-mcp
  elif [ "$INSTALL_MODE" = "crates" ]; then
    echo "[1/4] Installing aicx + aicx-mcp from crates.io..."
    cargo_install_with_progress cargo install aicx --locked
  else
    echo "[1/4] Installing aicx + aicx-mcp from git..."
    if ! cargo_install_with_progress cargo install --git "$AICX_GIT_URL" --locked aicx; then
      echo "Error: git install failed."
      echo "  If you only need the published release, use AICX_INSTALL_MODE=crates or run 'cargo install aicx --locked'."
      exit 1
    fi
  fi
else
  echo "[1/4] Skipping install (--skip-install)"
fi

# --- Step 2: Verify ---
echo "[2/4] Verifying..."
if ! resolve_aicx; then
  echo "Error: aicx is not available."
  if [ "$HAS_LOCAL_MANIFEST" -eq 1 ]; then
    echo "  From this checkout, run './install.sh' or 'cargo install --path . --locked --bin aicx --bin aicx-mcp'."
  else
    echo "  Ensure ~/.cargo/bin is in your PATH."
  fi
  exit 1
fi
echo "  aicx $("${AICX_RUN[@]}" --version 2>/dev/null | awk '{print $2}')"

if ! command -v python3 >/dev/null 2>&1; then
  echo "Error: python3 not found. install.sh uses python3 to update MCP settings."
  exit 1
fi

AICX_MCP_COMMAND=""
AICX_MCP_ARGS_JSON='[]'
if resolve_aicx_mcp; then
  if [ "$AICX_MCP_COMMAND" = "cargo" ]; then
    echo "  aicx-mcp via cargo run (local checkout fallback)"
  else
    echo "  aicx-mcp $AICX_MCP_COMMAND"
  fi
else
  echo "  Warning: aicx-mcp not found. MCP config will be skipped."
fi

# --- Step 3: Configure MCP ---
echo "[3/4] Configuring MCP servers..."

configure_mcp() {
  local tool_name="$1"
  local settings_path="$2"
  local settings_dir
  settings_dir=$(dirname "$settings_path")

  if [ ! -d "$settings_dir" ]; then
    echo "  [$tool_name] skipped (dir not found: $settings_dir)"
    return
  fi

  # Create settings file if it doesn't exist
  if [ ! -f "$settings_path" ]; then
    echo '{}' > "$settings_path"
  fi

  if [ -z "$AICX_MCP_COMMAND" ]; then
    echo "  [$tool_name] skipped (aicx-mcp unavailable)"
    return
  fi

  update_status=$(
    SETTINGS_PATH="$settings_path" \
    AICX_MCP_COMMAND="$AICX_MCP_COMMAND" \
    AICX_MCP_ARGS_JSON="$AICX_MCP_ARGS_JSON" \
    python3 - <<'PY'
import json
import os

path = os.environ["SETTINGS_PATH"]
desired = {
    "command": os.environ["AICX_MCP_COMMAND"],
    "args": json.loads(os.environ["AICX_MCP_ARGS_JSON"]),
}

with open(path) as f:
    data = json.load(f)

servers = data.setdefault("mcpServers", {})
current = servers.get("aicx")

if current == desired:
    print("already configured")
else:
    servers["aicx"] = desired
    with open(path, "w") as f:
        json.dump(data, f, indent=2)
        f.write("\n")
    print("configured")
PY
  ) || {
    echo "  [$tool_name] failed to configure (python3 error)"
    return
  }

  echo "  [$tool_name] ${update_status}: $settings_path"
}

# Claude Code
configure_mcp "claude" "$HOME/.claude/settings.json"

# Codex
configure_mcp "codex" "$HOME/.codex/settings.json"

# Gemini
configure_mcp "gemini" "$HOME/.gemini/settings.json"

# --- Step 4: Full store bootstrap ---
echo "[4/4] Full context extraction (this may take a moment)..."
"${AICX_RUN[@]}" all -H 10000 --emit none
echo "  store bootstrap complete"
echo "  memex runtime default: base (portable 1024-dim preset)"
maybe_configure_native_embedder
echo ""

# --- Done ---
echo "=== Setup complete ==="
echo ""
if path_has_dir "$AICX_BIN_DIR"; then
  echo "Install path:"
  echo "  $AICX_BIN_DIR is already on PATH"
else
  echo "PATH note:"
  echo "  Add $AICX_BIN_DIR to PATH so new shells pick up the bundled install first."
  echo "  Example: export PATH=\"$AICX_BIN_DIR:\$PATH\""
  echo ""
fi
if [ -d "$HOME/.ai-contexters" ]; then
  echo "Legacy store detected at ~/.ai-contexters/"
  echo "Run 'aicx migrate' to move your history to the new canonical ~/.aicx/ store."
  echo ""
fi
echo "Installed:"
echo "  aicx      — CLI for extraction, search, steer, dashboard"
echo "  aicx-mcp  — MCP server (3 tools: search, rank, steer)"
echo ""
echo "MCP tools available in Claude Code / Codex / Gemini:"
echo "  aicx_search  — fuzzy search across session history"
echo "  aicx_rank    — quality-score stored chunks"
echo "  aicx_steer   — retrieve chunks by run/prompt/project/agent/date metadata"
echo ""
echo "Quick start:"
echo "  aicx store -H 24                   # rescan last 24h from all agents"
echo "  aicx search 'query terms'          # fuzzy search across session history"
echo "  aicx refs -H 24                    # compact summary of recent files"
echo "  aicx steer --project aicx          # metadata-aware retrieval"
echo "  aicx memex-sync                    # optional memex materialization (base profile)"
echo ""
echo "Heavier retrieval on strong machines:"
echo "  AICX_RUNTIME_PROFILE=dev aicx memex-sync --reindex"
echo "  AICX_RUNTIME_PROFILE=premium aicx memex-sync --reindex"
echo ""
echo "Native embedder config (optional, future native-embedder builds/releases):"
echo "  ~/.aicx/embedder.toml"
echo "  bash install.sh --pick-embedder"
echo "  Note: large memex/Qwen provider settings still live in rust-memex config."
