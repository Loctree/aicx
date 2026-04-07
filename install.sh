#!/usr/bin/env bash
set -euo pipefail

# aicx setup — install binaries, configure MCP, and kick background indexing
#
# Usage:
#   bash install.sh
#   bash install.sh --skip-install  # reuse existing binaries, still reconfigure/bootstrap
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

choose_bin_dir() {
  local candidate=""
  for candidate in "$HOME/.local/bin" "$HOME/.cargo/bin"; do
    if [[ ":$PATH:" == *":$candidate:"* ]]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  for candidate in "$HOME/.local/bin" "$HOME/.cargo/bin"; do
    if [ -d "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  printf '%s\n' "$HOME/.local/bin"
}

AICX_BIN_DIR="${AICX_BIN_DIR:-$(choose_bin_dir)}"
RELEASE_BUNDLE_BIN_DIR=""
if [ -x "$SCRIPT_DIR/aicx" ] && [ -x "$SCRIPT_DIR/aicx-mcp" ] && [ -x "$SCRIPT_DIR/aicx-memex" ]; then
  RELEASE_BUNDLE_BIN_DIR="$SCRIPT_DIR"
fi
PATH_GUIDANCE_NEEDED=0
AICX_DISPLAY_COMMAND="aicx"
AICX_MEMEX_DISPLAY_COMMAND="aicx-memex"

SKIP_INSTALL=0
for arg in "$@"; do
  case "$arg" in
    --skip-install) SKIP_INSTALL=1 ;;
    --help|-h)
      echo "Usage: install.sh [--skip-install]"
      echo "  Install aicx + aicx-mcp + aicx-memex, configure MCP, and bootstrap background memex indexing."
      echo "  Run from the repo root or any local checkout that contains Cargo.toml."
      echo ""
      echo "Install source is controlled by AICX_INSTALL_MODE:"
      echo "  auto    - prefer local checkout, then bundled release binaries, otherwise install from crates.io"
      echo "  local   - cargo install --path <checkout> --locked"
      echo "  archive - copy bundled release binaries into \$AICX_BIN_DIR (default: $AICX_BIN_DIR)"
      echo "  crates  - cargo install ai-contexters --locked"
      echo "  git     - cargo install --git \$AICX_GIT_URL --locked ai-contexters"
      echo ""
      echo "Useful environment overrides:"
      echo "  AICX_BIN_DIR       - where release-bundle binaries should be copied (default: $AICX_BIN_DIR)"
      echo "  AICX_INSTALL_MODE  - auto | local | archive | crates | git"
      exit 0
      ;;
  esac
done

find_binary_path() {
  local name="$1"
  local candidate=""

  if command -v "$name" >/dev/null 2>&1; then
    command -v "$name"
    return 0
  fi

  for candidate in \
    "$AICX_BIN_DIR/$name" \
    "$SCRIPT_DIR/$name" \
    "$HOME/.local/bin/$name" \
    "$HOME/.cargo/bin/$name"; do
    if [ -x "$candidate" ]; then
      printf '%s\n' "$candidate"
      return 0
    fi
  done

  return 1
}

display_binary_path() {
  local name="$1"
  local resolved_path="$2"

  if command -v "$name" >/dev/null 2>&1 && [ "$(command -v "$name")" = "$resolved_path" ]; then
    printf '%s\n' "$name"
  else
    printf '%s\n' "$resolved_path"
  fi
}

install_release_bundle() {
  local target_dir="$AICX_BIN_DIR"
  local binary=""
  local source_path=""
  local target_path=""

  if [ -z "$RELEASE_BUNDLE_BIN_DIR" ]; then
    echo "Error: AICX_INSTALL_MODE=archive was requested, but this folder does not contain bundled aicx binaries." >&2
    echo "  Download a GitHub Release archive, extract it, and run ./install.sh from inside that folder." >&2
    exit 1
  fi

  mkdir -p "$target_dir"

  for binary in aicx aicx-mcp aicx-memex; do
    source_path="$RELEASE_BUNDLE_BIN_DIR/$binary"
    target_path="$target_dir/$binary"
    if [ "$source_path" != "$target_path" ]; then
      cp "$source_path" "$target_path"
    fi
    chmod 755 "$target_path"
    echo "  installed $binary -> $target_path"
  done

  if [[ ":$PATH:" != *":$target_dir:"* ]]; then
    PATH_GUIDANCE_NEEDED=1
  fi
}

resolve_aicx() {
  local aicx_path=""

  if aicx_path=$(find_binary_path "aicx"); then
    AICX_RUN=("$aicx_path")
    return 0
  fi

  if [ "$HAS_LOCAL_MANIFEST" -eq 1 ] && command -v cargo >/dev/null 2>&1; then
    AICX_RUN=("cargo" "run" "--quiet" "--manifest-path" "$MANIFEST_PATH" "--bin" "aicx" "--")
    return 0
  fi

  return 1
}

resolve_aicx_mcp() {
  local aicx_mcp_path=""

  if aicx_mcp_path=$(find_binary_path "aicx-mcp"); then
    AICX_MCP_COMMAND="$aicx_mcp_path"
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

resolve_aicx_memex() {
  local aicx_memex_path=""

  if aicx_memex_path=$(find_binary_path "aicx-memex"); then
    AICX_MEMEX_RUN=("$aicx_memex_path")
    return 0
  fi

  if [ "$HAS_LOCAL_MANIFEST" -eq 1 ] && command -v cargo >/dev/null 2>&1; then
    AICX_MEMEX_RUN=("cargo" "run" "--quiet" "--manifest-path" "$MANIFEST_PATH" "--bin" "aicx-memex" "--")
    return 0
  fi

  return 1
}

print_prefixed_block() {
  local prefix="$1"
  local text="$2"
  while IFS= read -r line; do
    [ -n "$line" ] || continue
    echo "${prefix}${line}"
  done <<<"$text"
}

run_with_heartbeat() {
  local message="$1"
  shift
  local heartbeat_seconds="${AICX_HEARTBEAT_SECONDS:-15}"

  "$@" &
  local cmd_pid=$!

  while kill -0 "$cmd_pid" 2>/dev/null; do
    sleep "$heartbeat_seconds"
    if kill -0 "$cmd_pid" 2>/dev/null; then
      echo "  $message"
    fi
  done

  wait "$cmd_pid"
}

bootstrap_memex_daemon() {
  if ! resolve_aicx_memex; then
    echo "  Warning: aicx-memex not found. Skipping background memex bootstrap."
    return 0
  fi

  local status_json=""
  local command_output=""
  local pid=""
  local phase=""
  local socket=""

  if command_output=$("${AICX_MEMEX_RUN[@]}" sync 2>&1); then
    echo "  aicx-memex daemon already running; refresh requested"
    print_prefixed_block "  " "$command_output"
  else
    echo "  starting aicx-memex daemon..."
    if command_output=$("${AICX_MEMEX_RUN[@]}" daemon 2>&1); then
      print_prefixed_block "  " "$command_output"
    else
      echo "  Warning: failed to start aicx-memex daemon."
      print_prefixed_block "    " "$command_output"
      return 0
    fi
  fi

  if status_json=$("${AICX_MEMEX_RUN[@]}" status --json 2>/dev/null); then
    read -r pid phase socket < <(
      DAEMON_STATUS_JSON="$status_json" python3 - <<'PY'
import json
import os

status = json.loads(os.environ["DAEMON_STATUS_JSON"])
print(status.get("pid", ""), status.get("phase", ""), status.get("socket_path", ""))
PY
    )
    echo "  daemon status: pid=${pid:-unknown} phase=${phase:-unknown}"
    if [ -n "$socket" ]; then
      echo "  daemon socket: $socket"
    fi
    echo "  inspect progress with: $AICX_MEMEX_DISPLAY_COMMAND status"
  else
    echo "  Warning: daemon started but status probe is not reachable yet."
  fi
}

echo "=== aicx setup ==="

resolve_install_mode() {
  case "$AICX_INSTALL_MODE" in
    auto)
      if [ "$HAS_LOCAL_MANIFEST" -eq 1 ]; then
        echo "local"
      elif [ -n "$RELEASE_BUNDLE_BIN_DIR" ]; then
        echo "archive"
      else
        echo "crates"
      fi
      ;;
    local|archive|crates|git)
      echo "$AICX_INSTALL_MODE"
      ;;
    *)
      echo "Error: unsupported AICX_INSTALL_MODE='$AICX_INSTALL_MODE' (expected auto, local, archive, crates, or git)." >&2
      exit 1
      ;;
  esac
}

# --- Step 1: Install binaries ---
if [ "$SKIP_INSTALL" -eq 0 ]; then
  INSTALL_MODE=$(resolve_install_mode)
  if [ "$INSTALL_MODE" != "archive" ] && ! command -v cargo >/dev/null 2>&1; then
    echo "Error: cargo not found. Install Rust first: https://rustup.rs"
    echo "  If you downloaded a release archive, rerun with AICX_INSTALL_MODE=archive from the extracted bundle."
    exit 1
  fi

  # Show live compilation progress and keep a heartbeat visible during long LTO/link phases.
  cargo_install_with_progress() {
    local total=0
    local heartbeat_seconds=15
    local last_update
    local temp_dir
    local stream_path
    local log_path
    local line=""
    local status=0

    temp_dir=$(mktemp -d "${TMPDIR:-/tmp}/aicx-install.XXXXXX")
    stream_path="$temp_dir/stream"
    log_path="$temp_dir/cargo-install.log"
    mkfifo "$stream_path"

    "$@" >"$stream_path" 2>&1 &
    local cmd_pid=$!

    exec 3<"$stream_path"
    last_update=$(date +%s)

    while true; do
      if IFS= read -r -u 3 -t 1 line; then
        printf '%s\n' "$line" >>"$log_path"
        last_update=$(date +%s)
        case "$line" in
          *Compiling*)
            total=$((total + 1))
            printf '\r  Compiling... (%d crates)' "$total" >&2
            ;;
          *Finished*|*Installing*|*Installed*|*Replacing*)
            printf '\r  %s\n' "$line" >&2
            ;;
          *error:*|*failed*|*Failed*)
            printf '\r  %s\n' "$line" >&2
            ;;
        esac
        continue
      fi

      if kill -0 "$cmd_pid" 2>/dev/null; then
        if [ $(( $(date +%s) - last_update )) -ge "$heartbeat_seconds" ]; then
          if [ "$total" -gt 0 ]; then
            printf '\r  Still building release binaries... (%d crates compiled so far)\n' "$total" >&2
          else
            printf '\r  Still building release binaries...\n' >&2
          fi
          last_update=$(date +%s)
        fi
        continue
      fi

      while IFS= read -r -u 3 line; do
        printf '%s\n' "$line" >>"$log_path"
        case "$line" in
          *Compiling*)
            total=$((total + 1))
            printf '\r  Compiling... (%d crates)' "$total" >&2
            ;;
          *Finished*|*Installing*|*Installed*|*Replacing*)
            printf '\r  %s\n' "$line" >&2
            ;;
          *error:*|*failed*|*Failed*)
            printf '\r  %s\n' "$line" >&2
            ;;
        esac
      done
      break
    done

    wait "$cmd_pid"
    status=$?

    if [ "$status" -ne 0 ] && [ -f "$log_path" ]; then
      echo "  cargo install failed. Last output:" >&2
      tail -n 20 "$log_path" >&2
    fi

    exec 3<&-
    rm -rf "$temp_dir"
    printf '\n' >&2
    return "$status"
  }

  if [ "$INSTALL_MODE" = "archive" ]; then
    echo "[1/4] Installing aicx + aicx-mcp + aicx-memex from this release bundle..."
    echo "  binaries will be copied into: $AICX_BIN_DIR"
    install_release_bundle
  elif [ "$INSTALL_MODE" = "local" ]; then
    echo "[1/4] Installing aicx + aicx-mcp + aicx-memex from this checkout..."
    echo "  release build can go quiet during LTO linking; heartbeat will continue below"
    cargo_install_with_progress cargo install --path "$SCRIPT_DIR" --locked --force --bin aicx --bin aicx-mcp --bin aicx-memex
  elif [ "$INSTALL_MODE" = "crates" ]; then
    echo "[1/4] Installing aicx + aicx-mcp + aicx-memex from crates.io..."
    echo "  release build can go quiet during LTO linking; heartbeat will continue below"
    cargo_install_with_progress cargo install ai-contexters --locked
  else
    echo "[1/4] Installing aicx + aicx-mcp + aicx-memex from git..."
    echo "  release build can go quiet during LTO linking; heartbeat will continue below"
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
    echo "  From this checkout, run './install.sh' or 'cargo install --path . --locked --bin aicx --bin aicx-mcp --bin aicx-memex'."
  elif [ -n "$RELEASE_BUNDLE_BIN_DIR" ]; then
    echo "  From this release archive, rerun './install.sh' without --skip-install so the bundled binaries are copied into $AICX_BIN_DIR."
  else
    echo "  Ensure ~/.cargo/bin or ~/.local/bin is in your PATH."
  fi
  exit 1
fi
echo "  aicx $("${AICX_RUN[@]}" --version 2>/dev/null | awk '{print $2}')"
if [ "${AICX_RUN[0]}" != "cargo" ]; then
  AICX_DISPLAY_COMMAND=$(display_binary_path "aicx" "${AICX_RUN[0]}")
fi

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

AICX_MEMEX_RUN=()
if resolve_aicx_memex; then
  if [ "${AICX_MEMEX_RUN[0]}" = "cargo" ]; then
    echo "  aicx-memex via cargo run (local checkout fallback)"
  else
    echo "  aicx-memex ${AICX_MEMEX_RUN[0]}"
    AICX_MEMEX_DISPLAY_COMMAND=$(display_binary_path "aicx-memex" "${AICX_MEMEX_RUN[0]}")
  fi
else
  echo "  Warning: aicx-memex not found. Background daemon bootstrap will be skipped."
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

# --- Step 4: Full store bootstrap + daemon kickoff ---
echo "[4/4] Full context extraction + memex daemon bootstrap (this may take a moment)..."
echo "  canonical store bootstrap heartbeat will continue below if extraction goes quiet"
run_with_heartbeat "Still bootstrapping canonical store..." "${AICX_RUN[@]}" all -H 10000 --incremental --emit none
echo "  store bootstrap complete"
bootstrap_memex_daemon
echo ""

# --- Done ---
echo "=== Setup complete ==="
echo ""
if [ -d "$HOME/.ai-contexters" ]; then
  echo "Legacy store detected at ~/.ai-contexters/"
  echo "Run '$AICX_DISPLAY_COMMAND migrate' to move your history to the new canonical ~/.aicx/ store."
  echo ""
fi
if [ "$PATH_GUIDANCE_NEEDED" -eq 1 ]; then
  echo "PATH note:"
  echo "  aicx binaries were copied to $AICX_BIN_DIR"
  echo "  MCP clients are configured with the absolute aicx-mcp path, so they are ready now."
  echo "  To use 'aicx' directly in future shells, add this to ~/.zshrc or ~/.bashrc:"
  echo "    export PATH=\"$AICX_BIN_DIR:\$PATH\""
  echo ""
fi
echo "Guided front door:"
"${AICX_RUN[@]}" -H 72 || true
echo ""
echo "Installed:"
echo "  aicx      — CLI for extraction, search, read, steer, dashboard"
echo "  aicx-mcp  — MCP server (4 tools: search, read, rank, steer)"
echo "  aicx-memex — background daemon launcher for steer/memex upkeep"
echo ""
echo "MCP tools available in Claude Code / Codex / Gemini:"
echo "  aicx_search  — fuzzy search across stored chunks"
echo "  aicx_read    — open one stored chunk by ref or path"
echo "  aicx_rank    — quality-score stored chunks"
echo "  aicx_steer   — retrieve chunks by run/prompt/project/agent/date metadata"
echo ""
echo "Quick start:"
echo "  $AICX_DISPLAY_COMMAND                              # guided front door + suggested next moves"
echo "  $AICX_DISPLAY_COMMAND doctor                       # one-command readiness + next steps"
echo "  $AICX_DISPLAY_COMMAND doctor --fix                 # repair what can be repaired automatically"
echo "  $AICX_DISPLAY_COMMAND dashboard --open             # local browser snapshot for non-terminal browsing"
echo "  $AICX_DISPLAY_COMMAND dashboard-serve --open       # live local UI that opens in your browser"
echo "  $AICX_DISPLAY_COMMAND latest --project ai-contexters # newest readable chunks for one repo"
echo "  $AICX_DISPLAY_COMMAND all -H 24 --incremental      # refresh last 24h from all agents"
echo "  $AICX_DISPLAY_COMMAND search 'query terms'         # fuzzy search across session history"
echo "  $AICX_DISPLAY_COMMAND read <ref-or-path>           # open one stored chunk directly"
echo "  $AICX_DISPLAY_COMMAND steer --project ai-contexters # metadata-aware retrieval"
echo "  $AICX_MEMEX_DISPLAY_COMMAND daemon                 # start background indexer on Unix socket"
