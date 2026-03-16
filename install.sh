#!/usr/bin/env bash
set -euo pipefail

# aicx setup — install binaries + configure MCP for all AI tools
#
# Usage:
#   curl -fsSL https://raw.githubusercontent.com/VetCoders/ai-contexters/main/install.sh | bash
#   bash install.sh
#   bash install.sh --skip-install  # MCP config only
#
# Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

SKIP_INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --skip-install) SKIP_INSTALL=1 ;;
    --help|-h)
      echo "Usage: install.sh [--skip-install]"
      echo "  Install aicx + aicx-mcp and configure MCP for Claude Code, Codex, and Gemini."
      exit 0
      ;;
  esac
done

echo "=== aicx setup ==="

# --- Step 1: Install binaries ---
if [ "$SKIP_INSTALL" -eq 0 ]; then
  if ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Install Rust first: https://rustup.rs"
    exit 1
  fi

  echo "[1/4] Installing aicx + aicx-mcp..."
  cargo install ai-contexters 2>&1 | tail -3
  echo "  aicx:     $(command -v aicx)"
  echo "  aicx-mcp: $(command -v aicx-mcp)"
else
  echo "[1/4] Skipping install (--skip-install)"
fi

# --- Step 2: Verify ---
echo "[2/4] Verifying..."
if ! command -v aicx >/dev/null 2>&1; then
  echo "Error: aicx not found in PATH after install."
  echo "  Ensure ~/.cargo/bin is in your PATH."
  exit 1
fi
echo "  aicx $(aicx --version 2>/dev/null | awk '{print $2}')"

AICX_MCP_BIN=$(command -v aicx-mcp 2>/dev/null || echo "")
if [ -z "$AICX_MCP_BIN" ]; then
  echo "  Warning: aicx-mcp not found. MCP server won't be available."
  AICX_MCP_BIN="aicx-mcp"
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

  # Check if aicx MCP is already configured
  if python3 -c "
import json, sys
with open('$settings_path') as f:
    d = json.load(f)
servers = d.get('mcpServers', {})
if 'aicx' in servers:
    sys.exit(0)
sys.exit(1)
" 2>/dev/null; then
    echo "  [$tool_name] already configured"
    return
  fi

  # Add aicx MCP server config
  python3 -c "
import json
path = '$settings_path'
with open(path) as f:
    d = json.load(f)
if 'mcpServers' not in d:
    d['mcpServers'] = {}
d['mcpServers']['aicx'] = {
    'command': '$AICX_MCP_BIN',
    'args': []
}
with open(path, 'w') as f:
    json.dump(d, f, indent=2)
    f.write('\n')
" 2>/dev/null

  if [ $? -eq 0 ]; then
    echo "  [$tool_name] configured: $settings_path"
  else
    echo "  [$tool_name] failed to configure (python3 error)"
  fi
}

# Claude Code
configure_mcp "claude" "$HOME/.claude/settings.json"

# Codex
configure_mcp "codex" "$HOME/.codex/settings.json"

# Gemini
configure_mcp "gemini" "$HOME/.gemini/settings.json"

# --- Step 4: Initial store ---
echo "[4/4] Initial context extraction..."
aicx store -H 168 --incremental 2>&1 | tail -5
echo ""

# --- Done ---
echo "=== Setup complete ==="
echo ""
echo "Installed:"
echo "  aicx      — CLI for extraction, ranking, search, dashboard"
echo "  aicx-mcp  — MCP server (4 tools: search, rank, refs, store)"
echo ""
echo "MCP tools available in Claude Code / Codex / Gemini:"
echo "  aicx_search  — fuzzy search across session history"
echo "  aicx_rank    — quality-score stored chunks"
echo "  aicx_refs    — list stored context files"
echo "  aicx_store   — trigger incremental extraction"
echo ""
echo "Quick start:"
echo "  aicx rank -p <project> --strict    # see quality chunks"
echo "  aicx dashboard-serve --port 8033   # web dashboard + search API"
echo "  aicx serve --transport sse         # MCP over HTTP for multi-agent"
