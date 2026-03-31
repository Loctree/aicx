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

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
MANIFEST_PATH="$SCRIPT_DIR/Cargo.toml"
HAS_LOCAL_MANIFEST=0
if [ -f "$MANIFEST_PATH" ]; then
  HAS_LOCAL_MANIFEST=1
fi
AICX_INSTALL_MODE="${AICX_INSTALL_MODE:-auto}"
AICX_GIT_URL="${AICX_GIT_URL:-https://github.com/VetCoders/ai-contexters}"

SKIP_INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --skip-install) SKIP_INSTALL=1 ;;
    --help|-h)
      echo "Usage: install.sh [--skip-install]"
      echo "  Install aicx + aicx-mcp and configure MCP for Claude Code, Codex, and Gemini."
      echo "  Run from the repo root or any local checkout that contains Cargo.toml."
      echo ""
      echo "Install source is controlled by AICX_INSTALL_MODE:"
      echo "  auto   - prefer local checkout, otherwise install from crates.io"
      echo "  local - cargo install --path <checkout> --locked"
      echo "  crates - cargo install ai-contexters --locked"
      echo "  git    - cargo install --git \$AICX_GIT_URL --locked ai-contexters"
      exit 0
      ;;
  esac
done

resolve_aicx() {
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

resolve_install_mode() {
  case "$AICX_INSTALL_MODE" in
    auto)
      if [ "$HAS_LOCAL_MANIFEST" -eq 1 ]; then
        echo "local"
      else
        echo "crates"
      fi
      ;;
    local|crates|git)
      echo "$AICX_INSTALL_MODE"
      ;;
    *)
      echo "Error: unsupported AICX_INSTALL_MODE='$AICX_INSTALL_MODE' (expected auto, local, crates, or git)." >&2
      exit 1
      ;;
  esac
}

# --- Step 1: Install binaries ---
if [ "$SKIP_INSTALL" -eq 0 ]; then
  if ! command -v cargo >/dev/null 2>&1; then
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

  INSTALL_MODE=$(resolve_install_mode)
  if [ "$INSTALL_MODE" = "local" ]; then
    echo "[1/4] Installing aicx + aicx-mcp from this checkout..."
    cargo_install_with_progress cargo install --path "$SCRIPT_DIR" --locked --force --bin aicx --bin aicx-mcp
  elif [ "$INSTALL_MODE" = "crates" ]; then
    echo "[1/4] Installing aicx + aicx-mcp from crates.io..."
    cargo_install_with_progress cargo install ai-contexters --locked
  else
    echo "[1/4] Installing aicx + aicx-mcp from git..."
    if ! cargo_install_with_progress cargo install --git "$AICX_GIT_URL" --locked ai-contexters; then
      echo "Error: git install failed."
      echo "  If you only need the published release, use AICX_INSTALL_MODE=crates or run 'cargo install ai-contexters --locked'."
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
"${AICX_RUN[@]}" all -H 10000 --incremental --emit none
echo "  store bootstrap complete"
echo ""

# --- Done ---
echo "=== Setup complete ==="
echo ""
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
echo "  aicx steer --project ai-contexters # metadata-aware retrieval"
