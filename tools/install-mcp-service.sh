#!/usr/bin/env bash
set -euo pipefail
# install-mcp-service.sh — background HTTP MCP server daemon for AICX (macOS launchd).
#
# Installs a per-user LaunchAgent that runs
#   ~/.local/bin/aicx-mcp --transport http --host 127.0.0.1 --port 8044
#     --no-require-auth --refresh-interval-seconds 1800
# with KeepAlive=true, so the Streamable HTTP MCP service stays available 24/7
# across machine restarts without manual intervention.
#
# Usage:
#   bash tools/install-mcp-service.sh              # install / refresh service
#   bash tools/install-mcp-service.sh --uninstall  # stop and remove service
#
# Env overrides:
#   AICX_MCP_PORT          port for HTTP transport (default 8044)
#   AICX_MCP_REFRESH_INTERVAL refresh cadence in seconds (default: 1800)
#   AICX_MCP_RUST_LOG      tracing filter for the LaunchAgent (default below).
#                          Do not use `aicx serve --verbose` — that flag only
#                          echoes per-file extractor warnings, not MCP/HTTP.
#   AICX_MCP_BIN           explicit aicx-mcp path (default: ~/.local/bin/aicx-mcp)
#   AICX_SKIP_MCP_CLIENTS  set to 1 to install the LaunchAgent without rewriting
#                          Claude/Codex/Gemini settings (install.sh uses this)
#
# `launchctl bootstrap gui/<uid>` only works from an Aqua login. Agent
# terminals, SSH, and some multiplexers return Error 5 (EIO) plus a
# "retry as root" hint — that hint is wrong for a per-user LaunchAgent.

LABEL="io.vetcoders.aicx.mcp"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG_DIR="$HOME/.aicx/logs"
PORT="${AICX_MCP_PORT:-8044}"
REFRESH_INTERVAL="${AICX_MCP_REFRESH_INTERVAL:-1800}"

note() { printf '  %s\n' "$*"; }

if [ "$(uname -s)" != "Darwin" ]; then
  note "mcp service: skipped (launchd-only; this host is $(uname -s))"
  exit 0
fi

gui_domain() { printf 'gui/%s' "$(id -u)"; }

if [ "${1:-}" = "--uninstall" ]; then
  launchctl bootout "$(gui_domain)/$LABEL" 2>/dev/null || true
  rm -f "$PLIST"
  note "mcp service: removed ($LABEL)"
  exit 0
fi

# Pin the service executable. User-facing service installs must never drift to
# a bare PATH or ~/.cargo/bin fallback.
AICX_MCP_BIN="${AICX_MCP_BIN:-$HOME/.local/bin/aicx-mcp}"
if [ ! -x "$AICX_MCP_BIN" ]; then
  note "mcp service: skipped ($AICX_MCP_BIN is not executable)"
  exit 0
fi
AICX_DIR="$(dirname "$AICX_MCP_BIN")"

# This LaunchAgent profile is deliberately local-only and unauthenticated.
# Remote exposure requires a separate, auth-enabled deployment profile.
HOST="127.0.0.1"

AICX_MCP_BIN_XML="$(printf '%s' "$AICX_MCP_BIN" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
HOST_XML="$(printf '%s' "$HOST" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
PORT_XML="$(printf '%s' "$PORT" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
REFRESH_INTERVAL_XML="$(printf '%s' "$REFRESH_INTERVAL" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
PATH_XML="$(printf '%s' "$AICX_DIR:/usr/bin:/bin:$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
# Default: tool-name audit + refresh ticks + rmcp session lifecycle.
# Not `info` globally — that floods hyper/h2. Not `--verbose` — extractor only.
RUST_LOG_VALUE="${AICX_MCP_RUST_LOG:-mcp.audit=info,mcp.refresh=debug,mcp.lifecycle=info,rmcp=info}"
RUST_LOG_XML="$(printf '%s' "$RUST_LOG_VALUE" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"

mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR"

cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$AICX_MCP_BIN_XML</string>
    <string>--transport</string>
    <string>http</string>
    <string>--host</string>
    <string>$HOST_XML</string>
    <string>--port</string>
    <string>$PORT_XML</string>
    <string>--no-require-auth</string>
    <string>--refresh-interval-seconds</string>
    <string>$REFRESH_INTERVAL_XML</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key>
    <string>$PATH_XML</string>
    <key>RUST_LOG</key>
    <string>$RUST_LOG_XML</string>
  </dict>
  <key>RunAtLoad</key>
  <true/>
  <key>KeepAlive</key>
  <true/>
  <key>StandardOutPath</key>
  <string>$LOG_DIR/aicx-serve-http.log</string>
  <key>StandardErrorPath</key>
  <string>$LOG_DIR/aicx-serve-http.log</string>
</dict>
</plist>
PLIST_EOF

plutil -lint "$PLIST" >/dev/null

# Point Claude/Codex/Gemini at this LaunchAgent URL (not stdio aicx-mcp).
# install.sh sets AICX_SKIP_MCP_CLIENTS=1 and applies the same helper once.
if [ "${AICX_SKIP_MCP_CLIENTS:-0}" != "1" ]; then
  CLIENTS_HELPER="$(cd "$(dirname "$0")" && pwd)/configure_mcp_clients.py"
  if [ -f "$CLIENTS_HELPER" ] && command -v python3 >/dev/null 2>&1; then
    python3 "$CLIENTS_HELPER" \
      --wire-defaults \
      --transport http \
      --plist "$PLIST" \
      --no-auth || \
      note "mcp clients: HTTP wiring failed (non-fatal)"
  fi
fi

service_loaded() {
  launchctl print "$(gui_domain)/$LABEL" >/dev/null 2>&1
}

# Capture bootstrap stderr so Apple's "retry as root" hint never reaches the
# operator as the last word. A failed bootstrap must not abort the installer
# (`set -e`): the plist is already written and will load at the next Aqua login.
try_bootstrap() {
  local err
  err="$(launchctl bootstrap "$(gui_domain)" "$PLIST" 2>&1)" || true
  if service_loaded; then
    return 0
  fi
  if [ -n "$err" ]; then
    note "mcp service: bootstrap: $err"
  fi
  return 1
}

MANAGER="$(launchctl managername 2>/dev/null || true)"
if [ "$MANAGER" != "Aqua" ]; then
  note "mcp service: plist written to $PLIST"
  note "mcp service: not loaded — this shell is not an Aqua login (launchctl managername=${MANAGER:-unknown})"
  note "mcp service: load from Terminal.app / a GUI session:"
  note "  launchctl bootstrap gui/$(id -u) $PLIST"
  note "mcp service: otherwise it loads at the next GUI login. Do not run this as root."
  exit 0
fi

# Idempotent refresh
launchctl bootout "$(gui_domain)/$LABEL" 2>/dev/null || true
if ! try_bootstrap; then
  sleep 1
  try_bootstrap || true
fi

if service_loaded; then
  # The HTTP server owns bounded catalog/index refresh now. Retire the former
  # second writer when upgrading an existing installation.
  launchctl bootout "$(gui_domain)/io.vetcoders.aicx.reindex" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/io.vetcoders.aicx.reindex.plist"
  note "mcp service: running on http://$HOST:$PORT/mcp via $LABEL"
  note "index refresh: owned by the MCP server (every ${REFRESH_INTERVAL}s)"
  note "mcp service logs: $LOG_DIR/aicx-serve-http.log"
else
  note "mcp service: plist written to $PLIST (bootstrap failed in this Aqua session)"
  note "mcp service: retry: launchctl bootstrap gui/$(id -u) $PLIST"
  note "mcp service: do not run as root — this is a per-user LaunchAgent"
fi
