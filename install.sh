#!/usr/bin/env bash
set -euo pipefail

# aicx setup — install binaries + configure MCP for supported AI tools
#
# Usage:
#   bash install.sh
#   bash install.sh --skip-install  # MCP config only
# Run from a local checkout, or pass --release to install from GitHub Releases.
#
# Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

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
AICX_CONFIG_PATH="${AICX_CONFIG_PATH:-$HOME/.aicx/config.toml}"
AICX_HOME_PICKER="${AICX_HOME_PICKER:-auto}"
AICX_STORAGE_HOME="${AICX_STORAGE_HOME:-}"
AICX_EMBEDDER_PICKER="${AICX_EMBEDDER_PICKER:-auto}"
AICX_EMBEDDER_PROFILE="${AICX_EMBEDDER_PROFILE:-}"
AICX_EMBEDDER_FILENAME="${AICX_EMBEDDER_FILENAME:-${AICX_EMBEDDER_FILE:-}}"
AICX_EMBEDDER_CONFIG_PATH="${AICX_EMBEDDER_CONFIG_PATH:-$HOME/.aicx/embedder.toml}"
AICX_INSTALL_FORCE="${AICX_INSTALL_FORCE:-0}"
AICX_INSTALL_DRY_RUN="${AICX_INSTALL_DRY_RUN:-0}"
AICX_CARGO_BIN_DIR="${AICX_CARGO_BIN_DIR:-${CARGO_INSTALL_ROOT:+$CARGO_INSTALL_ROOT/bin}}"
AICX_EMBEDDER_SETUP_DETAIL="No local embedder profile was configured in this run."
if [ -z "$AICX_CARGO_BIN_DIR" ]; then
  AICX_CARGO_BIN_DIR="$HOME/.cargo/bin"
fi

SKIP_INSTALL=0
SHADOW_CHECK_ONLY=0
VERIFY_PATH_ONLY=0
for arg in "$@"; do
  case "$arg" in
    --skip-install) SKIP_INSTALL=1 ;;
    --dry-run) AICX_INSTALL_DRY_RUN=1 ;;
    --force) AICX_INSTALL_FORCE=1 ;;
    --shadow-check-only) SHADOW_CHECK_ONLY=1 ;;
    --verify-path-only) VERIFY_PATH_ONLY=1 ;;
    --release) AICX_INSTALL_MODE="release" ;;
    --release-tag=*) AICX_RELEASE_TAG="${arg#*=}" ;;
    --pick-home) AICX_HOME_PICKER="1" ;;
    --no-home-prompt) AICX_HOME_PICKER="0" ;;
    --aicx-home=*) AICX_STORAGE_HOME="${arg#*=}" ;;
    --pick-embedder) AICX_EMBEDDER_PICKER="1" ;;
    --no-embedder-prompt) AICX_EMBEDDER_PICKER="0" ;;
    --embedder-profile=*) AICX_EMBEDDER_PROFILE="${arg#*=}" ;;
    --help|-h)
      echo "Usage: install.sh [--skip-install] [--dry-run] [--force]"
      echo "  Install aicx + aicx-mcp and configure MCP for Claude Code, Codex, and Gemini."
      echo "  Run from a release bundle or the repo root / local checkout."
      echo "  --dry-run shows shadow cleanup without installing or rewriting config."
      echo "  --force skips the multiple-aicx PATH confirmation."
      echo ""
      echo "Install source is controlled by AICX_INSTALL_MODE:"
      echo "  auto    - prefer bundled binaries, then local checkout, otherwise verified GitHub Release"
      echo "  release - download an official GitHub Release, verify SHA256, then install its bundle"
      echo "  bundle  - copy bundled binaries into \$AICX_BIN_DIR"
      echo "  local   - cargo install --path <checkout> --locked"
      echo "  crates  - legacy/unsupported: crates.io is not the active AICX distribution path"
      echo "  git     - cargo install --git \$AICX_GIT_URL --locked aicx"
      echo ""
      echo "Bundle install target:"
      echo "  AICX_BIN_DIR=\$HOME/.local/bin   # default destination for bundled binaries"
      echo ""
      echo "Release download target:"
      echo "  AICX_RELEASE_REPO=Loctree/aicx"
      echo "  AICX_RELEASE_TAG=latest          # or vX.Y.Z"
      echo ""
      echo "Native embedder profile shortcuts:"
      echo "  default: AICX_EMBEDDER_PROFILE=base    # F2LLM 0.6B Q4_K_M GGUF"
      echo "  dev:     AICX_EMBEDDER_PROFILE=dev     # F2LLM 1.7B Q4_K_M GGUF"
      echo "  premium: AICX_EMBEDDER_PROFILE=premium # F2LLM 1.7B Q6_K GGUF"
      echo "  build:   cargo build --release --features native-embedder"
      echo ""
      echo "Native embedder picker:"
      echo "  --pick-embedder                    # interactive config for ~/.aicx/embedder.toml"
      echo "  --embedder-profile=base|dev|premium"
      echo "  --no-embedder-prompt               # suppress interactive picker"
      echo "  note: writes only AICX local embedder preferences"
      echo ""
      echo "AICX storage root picker:"
      echo "  --pick-home                        # ask where AICX_HOME should live"
      echo "  --aicx-home=/absolute/path         # persist [storage].home in ~/.aicx/config.toml"
      echo "  --no-home-prompt                   # suppress interactive AICX_HOME picker"
      echo "  default: ~/.aicx                   # semantic index remains ~/.aicx/indexed/"
      exit 0
      ;;
  esac
done

resolve_aicx() {
  if [ -x "$AICX_BIN_DIR/aicx" ]; then
    AICX_RUN=("$AICX_BIN_DIR/aicx")
    return 0
  fi

  if [ -x "$AICX_CARGO_BIN_DIR/aicx" ]; then
    AICX_RUN=("$AICX_CARGO_BIN_DIR/aicx")
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

  if [ -x "$AICX_CARGO_BIN_DIR/aicx-mcp" ]; then
    AICX_MCP_COMMAND="$AICX_CARGO_BIN_DIR/aicx-mcp"
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
    base) echo "mradermacher/F2LLM-v2-0.6B-GGUF" ;;
    dev) echo "mradermacher/F2LLM-v2-1.7B-GGUF" ;;
    premium) echo "mradermacher/F2LLM-v2-1.7B-GGUF" ;;
    *) return 1 ;;
  esac
}

embedder_file_for_profile() {
  case "${1:-}" in
    base) echo "F2LLM-v2-0.6B.Q4_K_M.gguf" ;;
    dev) echo "F2LLM-v2-1.7B.Q4_K_M.gguf" ;;
    premium) echo "F2LLM-v2-1.7B.Q6_K.gguf" ;;
    *) return 1 ;;
  esac
}

write_storage_home_config() {
  local configured_home="$1"
  local config_path="$AICX_CONFIG_PATH"

  mkdir -p "$(dirname "$config_path")"
  AICX_CONFIG_PATH_FOR_WRITE="$config_path" \
  AICX_STORAGE_HOME_FOR_WRITE="$configured_home" \
  python3 - <<'PY'
from pathlib import Path
import json
import os
import re

config_path = Path(os.environ["AICX_CONFIG_PATH_FOR_WRITE"]).expanduser()
storage_home = os.environ["AICX_STORAGE_HOME_FOR_WRITE"].strip()
expanded = Path(storage_home).expanduser()
if not storage_home:
    raise SystemExit("empty AICX storage home")
if any(ord(ch) < 32 for ch in storage_home):
    raise SystemExit("invalid AICX home: control characters are not allowed")
if ".." in expanded.parts:
    raise SystemExit(
        "invalid AICX home: parent-directory traversal (`..`) is not allowed"
    )
if not (storage_home == "~" or storage_home.startswith("~/") or expanded.is_absolute()):
    raise SystemExit(
        f"invalid AICX home {storage_home!r}: use an absolute path or ~/..."
    )

home_line = f"home = {json.dumps(storage_home)}"
lines = config_path.read_text(encoding="utf-8").splitlines() if config_path.exists() else []
out = []
in_storage = False
storage_seen = False
home_written = False

for line in lines:
    stripped = line.strip()
    if stripped.startswith("[") and stripped.endswith("]") and not stripped.startswith("[["):
        if in_storage and not home_written:
            out.append(home_line)
            home_written = True
        in_storage = stripped == "[storage]"
        storage_seen = storage_seen or in_storage
        out.append(line)
        continue

    if in_storage and re.match(r"^\s*home\s*=", line):
        if not home_written:
            out.append(home_line)
            home_written = True
        continue

    out.append(line)

if in_storage and not home_written:
    out.append(home_line)
elif not storage_seen:
    if out and out[-1].strip():
        out.append("")
    out.append("[storage]")
    out.append(home_line)

config_path.write_text("\n".join(out).rstrip() + "\n", encoding="utf-8")
print(config_path)
PY
}

maybe_configure_aicx_home() {
  local picker selected_home config_written default_home
  picker=$(normalise_bool "$AICX_HOME_PICKER")
  default_home="$HOME/.aicx"
  selected_home="${AICX_STORAGE_HOME:-}"

  if [ -z "$selected_home" ] && { [ "$picker" = "1" ] || { [ "$picker" = "auto" ] && [ -t 0 ] && [ -t 1 ] && [ -z "${CI:-}" ]; }; }; then
    echo ""
    echo "AICX storage root setup"
    echo "  Default root: $default_home"
    echo "  Semantic index path: <AICX_HOME>/indexed/_all/embeddings.ndjson"
    echo "  Press Enter for the default, or enter an absolute path / ~/... for a persistent custom root."
    printf "AICX_HOME [%s]: " "$default_home"
    read -r selected_home || true
    selected_home="${selected_home:-}"
  fi

  if [ -z "$selected_home" ]; then
    echo "  AICX_HOME default kept: $default_home"
    return 0
  fi

  config_written=$(write_storage_home_config "$selected_home")
  echo "  config: $config_written"
  echo "  storage root: $selected_home"
  echo "  semantic index path: $selected_home/indexed/_all/embeddings.ndjson"
}

write_embedder_config() {
  local profile="$1"
  local repo="$2"
  local filename="$3"
  local path_override="$4"
  local config_path="$AICX_EMBEDDER_CONFIG_PATH"

  mkdir -p "$(dirname "$config_path")"
  EMBEDDER_CONFIG_PATH="$config_path" \
  EMBEDDER_PROFILE="$profile" \
  EMBEDDER_REPO="$repo" \
  EMBEDDER_FILENAME="$filename" \
  EMBEDDER_PATH_OVERRIDE="$path_override" \
  python3 - <<'PY'
from pathlib import Path
import os

config_path = Path(os.environ["EMBEDDER_CONFIG_PATH"]).expanduser()
profile = os.environ["EMBEDDER_PROFILE"].strip()
repo = os.environ["EMBEDDER_REPO"].strip()
filename = os.environ["EMBEDDER_FILENAME"].strip()
path_override = os.environ["EMBEDDER_PATH_OVERRIDE"].strip()

lines = [
    "# aicx native embedder preferences",
    "# First-choice local embeddings. Heavy retrieval remains rust-memex/Roost.",
    "",
    "[native_embedder]",
    'backend = "gguf"',
]

if profile:
    lines.append(f'profile = "{profile}"')
if repo:
    lines.append(f'repo = "{repo}"')
if filename:
    lines.append(f'filename = "{filename}"')
if path_override:
    lines.append(f'path = "{path_override}"')
lines.append("prefer_embedded = false")
lines.append("max_length = 512")

config_path.write_text("\n".join(lines) + "\n", encoding="utf-8")
print(config_path)
PY
}

maybe_prime_embedder_cache() {
  local repo="$1"
  local filename="$2"

  if [ -z "$repo" ] || ! [ -t 0 ] || ! [ -t 1 ]; then
    return 0
  fi

  if ! command -v hf >/dev/null 2>&1; then
    echo "  native embedder cache not primed automatically (missing 'hf' CLI)"
    if [ -n "$filename" ]; then
      echo "  later, run: hf download $repo $filename"
    else
      echo "  later, run: hf download <repo> <model.gguf>"
    fi
    return 0
  fi

  if [ -z "$filename" ]; then
    echo "  native embedder cache not primed automatically (custom repo has no GGUF filename)"
    echo "  later, run: hf download $repo <model.gguf>"
    return 0
  fi

  printf "  Download native embedder model into local HF cache now? [y/N] "
  read -r reply || true
  case "${reply:-}" in
    y|Y|yes|YES)
      echo "  priming HF cache for $repo $filename ..."
      if hf download "$repo" "$filename"; then
        echo "  HF cache primed"
      else
        echo "  warning: hf download failed; config was still saved" >&2
      fi
      ;;
    *)
      echo "  skipping model download"
      echo "  later, run: hf download $repo $filename"
      ;;
  esac
}

maybe_configure_native_embedder() {
  local picker explicit_profile selected_profile selected_repo selected_filename config_written
  picker=$(normalise_bool "$AICX_EMBEDDER_PICKER")
  explicit_profile="${AICX_EMBEDDER_PROFILE:-}"
  selected_profile=""
  selected_repo="${AICX_EMBEDDER_REPO:-}"
  selected_filename="${AICX_EMBEDDER_FILENAME:-}"
  config_written=""

  if [ -n "${AICX_EMBEDDER_PATH:-}" ]; then
    config_written=$(write_embedder_config "" "" "" "$AICX_EMBEDDER_PATH")
    echo "  native embedder path pinned in $config_written"
    AICX_EMBEDDER_SETUP_DETAIL="Saved explicit local embedder path in $config_written."
    return 0
  fi

  if [ -n "$selected_repo" ]; then
    config_written=$(write_embedder_config "" "$selected_repo" "$selected_filename" "")
    echo "  native embedder repo pinned in $config_written"
    AICX_EMBEDDER_SETUP_DETAIL="Saved local embedder repo preference in $config_written."
    maybe_prime_embedder_cache "$selected_repo" "$selected_filename"
    return 0
  fi

  if [ -n "$explicit_profile" ]; then
    case "$explicit_profile" in
      none|off|skip)
        echo "  native embedder picker: skipped by explicit profile"
        AICX_EMBEDDER_SETUP_DETAIL="You skipped native embedder setup."
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
    echo "Native embedder setup"
    echo "  This optional step writes $AICX_EMBEDDER_CONFIG_PATH."
    echo "  Model download is explicit and opt-in."
    echo ""
    echo "  1) Skip"
    echo "     Do not configure local embeddings now."
    echo ""
    echo "  2) Base"
    echo "     F2LLM 0.6B Q4_K_M GGUF"
    echo "     Approx. 397 MB"
    echo "     Recommended default."
    echo ""
    echo "  3) Dev"
    echo "     F2LLM 1.7B Q4_K_M GGUF"
    echo "     Approx. 1.1 GB"
    echo "     Better quality, requires more resources."
    echo ""
    echo "  4) Premium"
    echo "     F2LLM 1.7B Q6_K GGUF"
    echo "     Approx. 1.4 GB"
    echo "     Highest local quality, largest download."
    echo ""
    printf "Select profile [1-4]: "
    read -r reply || true
    case "${reply:-1}" in
      1|"")
        echo "  native embedder picker: skipped"
        AICX_EMBEDDER_SETUP_DETAIL="You skipped native embedder setup."
        return 0
        ;;
      2) selected_profile="base" ;;
      3) selected_profile="dev" ;;
      4) selected_profile="premium" ;;
      *)
        echo "  native embedder picker: invalid choice, skipping"
        AICX_EMBEDDER_SETUP_DETAIL="No local embedder profile was configured; invalid picker choice was ignored."
        return 0
        ;;
    esac
  else
    return 0
  fi

  selected_repo=$(embedder_repo_for_profile "$selected_profile")
  selected_filename=$(embedder_file_for_profile "$selected_profile")
  config_written=$(write_embedder_config "$selected_profile" "$selected_repo" "$selected_filename" "")
  echo "  native embedder preference saved to $config_written"
  AICX_EMBEDDER_SETUP_DETAIL="Saved the '$selected_profile' local embedder profile in $config_written."
  maybe_prime_embedder_cache "$selected_repo" "$selected_filename"
}

cleanup_old_binaries() {
  local path="$1"
  if [ -L "$path" ] || [ -f "$path" ]; then
    if [ "$AICX_INSTALL_DRY_RUN" = "1" ]; then
      echo "  would remove stale $(basename "$path") from $(dirname "$path")"
      return 0
    fi
    rm -f "$path"
    echo "  removed stale $(basename "$path") from $(dirname "$path")"
  fi
}

strip_trailing_slash() {
  local path="$1"
  while [ "${#path}" -gt 1 ] && [ "${path%/}" != "$path" ]; do
    path="${path%/}"
  done
  echo "$path"
}

same_path() {
  [ "$(strip_trailing_slash "$1")" = "$(strip_trailing_slash "$2")" ]
}

extract_semver() {
  local text="${1:-}"
  if [[ "$text" =~ ([0-9]+)\.([0-9]+)\.([0-9]+)([-+][0-9A-Za-z.-]+)? ]]; then
    echo "${BASH_REMATCH[1]}.${BASH_REMATCH[2]}.${BASH_REMATCH[3]}"
    return 0
  fi
  return 1
}

semver_le() {
  local left="$1"
  local right="$2"
  local la lb lc ra rb rc part
  IFS=. read -r la lb lc <<< "$left"
  IFS=. read -r ra rb rc <<< "$right"
  for part in "$la" "$lb" "$lc" "$ra" "$rb" "$rc"; do
    case "$part" in
      ''|*[!0-9]*) return 1 ;;
    esac
  done
  if [ "$la" -lt "$ra" ]; then return 0; fi
  if [ "$la" -gt "$ra" ]; then return 1; fi
  if [ "$lb" -lt "$rb" ]; then return 0; fi
  if [ "$lb" -gt "$rb" ]; then return 1; fi
  [ "$lc" -le "$rc" ]
}

binary_semver() {
  local path="$1"
  local output
  if ! [ -x "$path" ]; then
    return 1
  fi
  output=$("$path" --version 2>/dev/null || true)
  extract_semver "$output"
}

manifest_version() {
  local line value
  if [ ! -f "$MANIFEST_PATH" ]; then
    return 1
  fi
  while IFS= read -r line; do
    case "$line" in
      version\ =\ \"*\")
        value="${line#version = \"}"
        value="${value%%\"*}"
        echo "$value"
        return 0
        ;;
    esac
  done < "$MANIFEST_PATH"
  return 1
}

npm_global_bin_dir() {
  local bin_dir prefix
  if ! command -v npm >/dev/null 2>&1; then
    return 1
  fi
  bin_dir=$(npm bin -g 2>/dev/null || true)
  if [ -n "$bin_dir" ] && [ -d "$bin_dir" ]; then
    echo "$bin_dir"
    return 0
  fi
  prefix=$(npm prefix -g 2>/dev/null || true)
  if [ -n "$prefix" ]; then
    echo "$prefix/bin"
    return 0
  fi
  return 1
}

install_target_bin_dir() {
  case "$1" in
    bundle|release) echo "$AICX_BIN_DIR" ;;
    local|git|crates) echo "$AICX_CARGO_BIN_DIR" ;;
    *) echo "$AICX_BIN_DIR" ;;
  esac
}

target_install_version() {
  local mode="$1"
  case "$mode" in
    bundle)
      binary_semver "$SCRIPT_DIR/aicx" || true
      ;;
    release)
      if [ "$AICX_RELEASE_TAG" != "latest" ]; then
        extract_semver "$AICX_RELEASE_TAG" || true
      fi
      ;;
    local)
      manifest_version || true
      ;;
    *)
      echo ""
      ;;
  esac
}

scan_binary_shadows() {
  local binary_name="$1"
  local target_path="$2"
  local shadow_paths count path version resolved

  echo "Scanning current $binary_name installation surface..."
  shadow_paths=$(which -a "$binary_name" 2>/dev/null | sort -u || true)
  if [ -z "$shadow_paths" ]; then
    echo "  no existing $binary_name on PATH"
    return 0
  fi

  echo "Found existing $binary_name installations:"
  count=0
  while IFS= read -r path; do
    [ -n "$path" ] || continue
    count=$((count + 1))
    version=$("$path" --version 2>/dev/null || echo "unknown")
    echo "  $path -> $version"
  done <<< "$shadow_paths"

  resolved=$(command -v "$binary_name" 2>/dev/null || true)
  if [ "$count" -gt 1 ] || { [ -n "$resolved" ] && ! same_path "$resolved" "$target_path"; }; then
    echo ""
    echo "WARNING: Multiple or shadowing $binary_name binaries detected."
    echo "  target install path: $target_path"
    if [ -n "$resolved" ]; then
      echo "  PATH currently resolves to: $resolved"
    fi
    if [ "$AICX_INSTALL_FORCE" != "1" ] && [ "$AICX_INSTALL_DRY_RUN" != "1" ]; then
      if [ -t 0 ]; then
        printf "Continue install? [y/N] "
        read -r confirm
        case "${confirm:-}" in
          y|Y|yes|YES) ;;
          *)
            echo "Aborted. Set AICX_INSTALL_FORCE=1 or pass --force to skip this check."
            exit 1
            ;;
        esac
      else
        echo "Aborted in non-interactive mode. Set AICX_INSTALL_FORCE=1 to skip this check."
        exit 1
      fi
    fi
  fi
}

scan_aicx_shadows() {
  local target_aicx="$1"
  local target_mcp="$2"

  scan_binary_shadows "aicx" "$target_aicx"
  scan_binary_shadows "aicx-mcp" "$target_mcp"
}

cleanup_shadow_pair() {
  local dir="$1"
  local target_dir="$2"
  local target_version="$3"
  local candidate="$dir/aicx"
  local candidate_version=""

  if same_path "$dir" "$target_dir"; then
    return 0
  fi
  if ! { [ -f "$candidate" ] || [ -L "$candidate" ]; }; then
    return 0
  fi

  candidate_version=$(binary_semver "$candidate" || true)
  if [ -z "$target_version" ] || [ -z "$candidate_version" ]; then
    echo "  shadow retained at $candidate (version unknown; cleanup requires an older/equal version)"
    return 0
  fi

  if semver_le "$candidate_version" "$target_version"; then
    if [ "$AICX_INSTALL_DRY_RUN" = "1" ]; then
      echo "  would remove shadow aicx $candidate_version from $dir (target $target_version)"
    else
      echo "  removing shadow aicx $candidate_version from $dir (target $target_version)"
    fi
    cleanup_old_binaries "$dir/aicx"
    cleanup_old_binaries "$dir/aicx-mcp"
  else
    echo "  newer aicx $candidate_version retained at $dir (target $target_version)"
  fi
}

cleanup_shadow_aicx_binaries() {
  local target_dir="$1"
  local target_version="$2"
  local npm_bin

  echo "Checking canonical shadow install paths..."
  cleanup_shadow_pair "$HOME/.local/bin" "$target_dir" "$target_version"
  cleanup_shadow_pair "$AICX_CARGO_BIN_DIR" "$target_dir" "$target_version"
  if npm_bin=$(npm_global_bin_dir); then
    cleanup_shadow_pair "$npm_bin" "$target_dir" "$target_version"
  fi
}

verify_install_path_resolution() {
  local installed_path="$1"
  local binary_name="${2:-aicx}"
  local installed_version path_resolved path_resolved_version

  if ! [ -x "$installed_path" ]; then
    return 0
  fi

  installed_version=$("$installed_path" --version 2>/dev/null || echo "unknown")
  path_resolved=$(command -v "$binary_name" 2>/dev/null || true)
  if [ -z "$path_resolved" ]; then
    echo ""
    echo "=========================================="
    echo "WARNING: installed $binary_name is not on PATH"
    echo "  Installed to: $installed_path -> $installed_version"
    echo "  Add $(dirname "$installed_path") to PATH before running $binary_name."
    echo "=========================================="
    return 0
  fi
  path_resolved_version=$("$binary_name" --version 2>/dev/null || echo "unknown")
  if [ -n "$path_resolved" ] && { ! same_path "$path_resolved" "$installed_path" || [ "$installed_version" != "$path_resolved_version" ]; }; then
    echo ""
    echo "=========================================="
    echo "WARNING: PATH version mismatch detected"
    echo "  Installed to: $installed_path -> $installed_version"
    echo "  PATH resolves to: $path_resolved -> $path_resolved_version"
    echo ""
    echo "Other $binary_name binaries in PATH:"
    which -a "$binary_name" 2>/dev/null | sort -u | while IFS= read -r path; do
      if [ -n "$path" ] && ! same_path "$path" "$installed_path"; then
        echo "  $path"
      fi
    done
    echo ""
    echo "To fix: ensure $(dirname "$installed_path") is before other $binary_name locations in your PATH,"
    echo "or remove the older binary shown above."
    echo "=========================================="
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
  echo "  pruning stale files in target bin dir..."
  cleanup_old_binaries "$install_dir/aicx"
  cleanup_old_binaries "$install_dir/aicx-mcp"
  cleanup_old_binaries "$install_dir/ai-contexters"
  cleanup_old_binaries "$install_dir/ai-contexters-mcp"
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
    Darwin:x86_64)
      echo "Error: x86_64-apple-darwin release assets are not published in the current AICX release asset set." >&2
      echo "  Use a local bundle install instead, or install from source with cargo." >&2
      exit 1
      ;;
    Linux:x86_64) echo "x86_64-linux-gnu" ;;
    Linux:aarch64|Linux:arm64) echo "aarch64-linux-gnu" ;;
    *)
      echo "Error: unsupported platform for release installer: ${os}/${arch}" >&2
      echo "  Use a local bundle install instead, or install from source with cargo." >&2
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
  local release_tag target archive_ext archive_name bundle_name base_url tmp_dir archive_path checksum_path bundle_dir

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
  case "$target" in
    *apple-darwin|*windows*) archive_ext="zip" ;;
    *) archive_ext="tar.gz" ;;
  esac
  bundle_name="aicx-${release_tag}-${target}-slim"
  archive_name="${bundle_name}.${archive_ext}"
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
  echo "  asset:       $archive_name"
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
    # zipfile.extractall does NOT restore POSIX modes from external_attr, so the
    # bundled `aicx`/`aicx-mcp`/`install.sh` would land as 0644 and the delegated
    # bundled installer would reject them (no executable bit). Extract per entry
    # and restore the mode from the high 16 bits of external_attr. Skip macOS
    # AppleDouble (`._*`) sidecar entries produced by the system zip tool.
    with zipfile.ZipFile(archive_path) as archive:
        for info in archive.infolist():
            if os.path.basename(info.filename).startswith("._"):
                continue
            extracted = archive.extract(info, dest_dir)
            mode = info.external_attr >> 16
            if mode:
                os.chmod(extracted, mode)
elif archive_path.name.endswith(".tar.gz"):
    with tarfile.open(archive_path, "r:gz") as archive:
        archive.extractall(dest_dir)
else:
    raise SystemExit(f"Unsupported archive format: {archive_path.name}")
PY
  bundle_dir="$tmp_dir/$bundle_name"
  if [ ! -f "$bundle_dir/install.sh" ]; then
    echo "Error: release bundle does not contain install.sh: $bundle_dir" >&2
    exit 1
  fi

  echo "[4/4] Delegating to bundled installer..."
  AICX_INSTALL_MODE="bundle" \
  AICX_BIN_DIR="$AICX_BIN_DIR" \
  AICX_INSTALL_FORCE="$AICX_INSTALL_FORCE" \
  AICX_INSTALL_DRY_RUN="$AICX_INSTALL_DRY_RUN" \
  AICX_CONFIG_PATH="$AICX_CONFIG_PATH" \
  AICX_HOME_PICKER="$AICX_HOME_PICKER" \
  AICX_STORAGE_HOME="$AICX_STORAGE_HOME" \
  AICX_EMBEDDER_PICKER="$AICX_EMBEDDER_PICKER" \
  AICX_EMBEDDER_PROFILE="$AICX_EMBEDDER_PROFILE" \
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
        echo "release"
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
INSTALL_TARGET_BIN_DIR=$(install_target_bin_dir "$INSTALL_MODE")
INSTALL_TARGET_AICX="$INSTALL_TARGET_BIN_DIR/aicx"
INSTALL_TARGET_AICX_MCP="$INSTALL_TARGET_BIN_DIR/aicx-mcp"
INSTALL_TARGET_VERSION=$(target_install_version "$INSTALL_MODE")
if [ "$VERIFY_PATH_ONLY" -eq 1 ]; then
  verify_install_path_resolution "$INSTALL_TARGET_AICX" "aicx"
  verify_install_path_resolution "$INSTALL_TARGET_AICX_MCP" "aicx-mcp"
  exit 0
fi
if [ "$SKIP_INSTALL" -eq 0 ]; then
  scan_aicx_shadows "$INSTALL_TARGET_AICX" "$INSTALL_TARGET_AICX_MCP"
  cleanup_shadow_aicx_binaries "$INSTALL_TARGET_BIN_DIR" "$INSTALL_TARGET_VERSION"
fi
if [ "$SHADOW_CHECK_ONLY" -eq 1 ]; then
  exit 0
fi
if [ "$AICX_INSTALL_DRY_RUN" = "1" ]; then
  echo "Dry run complete; no binaries installed and no config files changed."
  exit 0
fi
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
    echo "Error: crates.io is not the active AICX distribution path." >&2
    echo "  Use AICX_INSTALL_MODE=release for verified GitHub Release assets, or install from a local checkout." >&2
    exit 1
  else
    echo "[1/4] Installing aicx + aicx-mcp from git..."
    if ! cargo_install_with_progress cargo install --git "$AICX_GIT_URL" --locked aicx; then
      echo "Error: git install failed."
      echo "  If you only need the published release, use AICX_INSTALL_MODE=release."
      exit 1
    fi
  fi
else
  echo "[1/4] Skipping install (--skip-install)"
fi

if [ "$SKIP_INSTALL" -eq 0 ]; then
  verify_install_path_resolution "$INSTALL_TARGET_AICX" "aicx"
  verify_install_path_resolution "$INSTALL_TARGET_AICX_MCP" "aicx-mcp"
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

# --- Step 3: Configure storage + MCP ---
echo "[3/4] Configuring storage + MCP servers..."
maybe_configure_aicx_home

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
echo "  local embedder default: base (F2LLM 0.6B Q4_K_M GGUF, hydrated on demand)"
maybe_configure_native_embedder
echo ""

# --- Done ---
echo "=== AICX setup complete ==="
echo ""
echo "Installed:"
echo "  aicx      - command-line tool for indexing and searching agent history"
echo "  aicx-mcp  - MCP server for Claude Code, Codex and Gemini"
echo ""
echo "Install path:"
echo "  $AICX_BIN_DIR"
if path_has_dir "$AICX_BIN_DIR"; then
  echo "  This path is already available in PATH."
else
  echo "  Add this path to PATH so new shells pick up the bundled install first."
  echo "  Example: export PATH=\"$AICX_BIN_DIR:\$PATH\""
fi
echo ""
if [ -d "$HOME/.ai-contexters" ]; then
  echo "Legacy store:"
  echo "  Found ~/.ai-contexters/"
  echo "  Run 'aicx migrate' to move your history to the canonical ~/.aicx/ store."
  echo ""
fi
echo "MCP tools:"
echo "  aicx_search  - search session history"
echo "  aicx_read    - read a stored chunk"
echo "  aicx_rank    - score stored chunks"
echo "  aicx_steer   - search by project, agent, date or run metadata"
echo ""
echo "Quick start:"
echo "  aicx all -H 24"
echo "      Index the last 24 hours from supported agents."
echo ""
echo "  aicx search \"query terms\""
echo "      Search indexed session history."
echo ""
echo "  aicx refs -H 24"
echo "      Show compact references from the last 24 hours."
echo ""
echo "  aicx steer --project aicx"
echo "      Search using project metadata."
echo ""
echo "Local embeddings:"
echo "  $AICX_EMBEDDER_SETUP_DETAIL"
echo ""
echo "  To configure or change local embeddings later, run:"
echo "    bash install.sh --pick-embedder"
echo ""
echo "  Config file:"
echo "    $AICX_EMBEDDER_CONFIG_PATH"
