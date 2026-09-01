#!/usr/bin/env bash
set -euo pipefail
# install-mcp-service.sh — background reader-only HTTP MCP server for AICX (macOS launchd).
#
# Installs a per-user LaunchAgent that runs
#   aicx serve --transport http --no-auto-refresh
# with KeepAlive=true, so the Streamable HTTP MCP reader stays available 24/7
# across machine restarts without taking ownership of catalog/index maintenance.
# Index maintenance is a separate process boundary; see install-reindex-schedule.sh
# for the baseline scheduler.
#
# Usage:
#   bash tools/install-mcp-service.sh              # install / refresh service
#   bash tools/install-mcp-service.sh --uninstall  # stop and remove service
#
# Env overrides:
#   AICX_MCP_PORT          port for HTTP transport (default 8044)
#   AICX_MCP_HOST          bind host (default: 127.0.0.1; set a Tailscale IP explicitly for remote access)
#   AICX_MCP_ALLOWED_HOSTS comma-separated allowed Host headers (default: loopback + bind host)
#   AICX_MCP_RUST_LOG      tracing filter for the LaunchAgent (default below).
#                          Do not use `aicx serve --verbose` — that flag only
#                          echoes per-file extractor warnings, not MCP/HTTP.
#   AICX_BIN               explicit aicx binary path (default: resolve from PATH / ~/.local/bin / ~/.cargo/bin)
#   AICX_SKIP_MCP_CLIENTS  set to 1 to install the LaunchAgent without rewriting
#                          Claude/Codex/Gemini settings (install.sh uses this)
#
# `launchctl bootstrap gui/<uid>` only works from an Aqua login. Agent
# terminals, SSH, and some multiplexers return Error 5 (EIO) plus a
# "retry as root" hint — that hint is wrong for a per-user LaunchAgent.

LABEL="com.loctree.aicx.mcp"
LEGACY_LABEL="io.vetcoders.aicx.mcp"
PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LOG_DIR="$HOME/.aicx/logs"
PORT="${AICX_MCP_PORT:-8044}"

note() { printf '  %s\n' "$*"; }

if [ "$(uname -s)" != "Darwin" ]; then
  note "mcp service: skipped (launchd-only; this host is $(uname -s))"
  exit 0
fi

gui_domain() { printf 'gui/%s' "$(id -u)"; }

# Boot out an MCP launchd label and delete its plist if present. Used for the
# canonical MCP label and to migrate machines still running the legacy
# io.vetcoders.aicx.mcp agent so install/uninstall never leave two readers.
# This helper is deliberately NOT used for reindex/maintenance labels.
retire_launchd_label() {
  local label="$1"
  launchctl bootout "$(gui_domain)/$label" 2>/dev/null || true
  rm -f "$HOME/Library/LaunchAgents/$label.plist"
}

if [ "${1:-}" = "--uninstall" ]; then
  retire_launchd_label "$LABEL"
  retire_launchd_label "$LEGACY_LABEL"
  note "mcp service: removed ($LABEL)"
  note "index maintenance: unchanged (owned separately from the MCP reader)"
  exit 0
fi

# Resolve binary
AICX_BIN="${AICX_BIN:-}"
if [ -z "$AICX_BIN" ]; then
  if [ -x "$HOME/.local/bin/aicx" ]; then
    AICX_BIN="$HOME/.local/bin/aicx"
  elif [ -x "$HOME/.cargo/bin/aicx" ]; then
    AICX_BIN="$HOME/.cargo/bin/aicx"
  else
    AICX_BIN="$(command -v aicx || true)"
  fi
fi

if [ -z "$AICX_BIN" ] || [ ! -x "$AICX_BIN" ]; then
  note "mcp service: skipped (aicx not found — build/install aicx first)"
  exit 0
fi

AICX_DIR="$(dirname "$AICX_BIN")"

# Local-only is the safe and portable default. Tailnet exposure is an explicit
# operator decision: AICX_MCP_HOST="$(tailscale ip -4)" make install-service.
HOST="${AICX_MCP_HOST:-127.0.0.1}"

# Build allowed-host arguments as plist entries, not a shell command string.
# This keeps operator-provided hostnames out of shell parsing entirely.
ALLOWED_ARGS_XML=""
SEEN_HOSTS="|"
IFS=',' read -ra HOSTS <<< "${AICX_MCP_ALLOWED_HOSTS:-localhost,127.0.0.1,::1,$HOST}"
for h in "${HOSTS[@]}"; do
  h_trimmed="$(echo "$h" | xargs)"
  if [ -n "$h_trimmed" ] && [[ "$SEEN_HOSTS" != *"|$h_trimmed|"* ]]; then
    SEEN_HOSTS="$SEEN_HOSTS$h_trimmed|"
    h_xml="$(printf '%s' "$h_trimmed" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
    ALLOWED_ARGS_XML="$ALLOWED_ARGS_XML
    <string>--allowed-host</string>
    <string>$h_xml</string>"
  fi
done

AICX_BIN_XML="$(printf '%s' "$AICX_BIN" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
HOST_XML="$(printf '%s' "$HOST" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
PORT_XML="$(printf '%s' "$PORT" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
PATH_XML="$(printf '%s' "$AICX_DIR:/usr/bin:/bin:$HOME/.local/bin:$HOME/.cargo/bin:/opt/homebrew/bin" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"
# Default: tool-name audit + rmcp session lifecycle. The reader does not own
# periodic refresh, so mcp.refresh logging is intentionally absent here.
# Not `info` globally — that floods hyper/h2. Not `--verbose` — extractor only.
RUST_LOG_VALUE="${AICX_MCP_RUST_LOG:-mcp.audit=info,mcp.lifecycle=info,rmcp=info}"
RUST_LOG_XML="$(printf '%s' "$RUST_LOG_VALUE" | sed -e 's/&/\&amp;/g' -e 's/</\&lt;/g' -e 's/>/\&gt;/g' -e 's/"/\&quot;/g' -e "s/'/\&apos;/g")"

mkdir -p "$HOME/Library/LaunchAgents" "$LOG_DIR"

# `--no-auto-refresh` is deliberately kept even though the binary's default is
# already reader-only: it keeps the LaunchAgent safe if it ever runs against an
# older aicx whose default embedded the periodic writer. Do not remove it as
# "redundant"; the flag is a compatibility no-op on current builds.
cat > "$PLIST" <<PLIST_EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key>
  <string>$LABEL</string>
  <key>ProgramArguments</key>
  <array>
    <string>$AICX_BIN_XML</string>
    <string>serve</string>
    <string>--transport</string>
    <string>http</string>
    <string>--no-auto-refresh</string>
    <string>--host</string>
    <string>$HOST_XML</string>
    <string>--port</string>
    <string>$PORT_XML</string>$ALLOWED_ARGS_XML
    <string>--no-auto-refresh</string>
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

# Keep expensive corpus publication outside the long-lived server. The helper
# also writes its plist when launchctl cannot bootstrap from the current shell.
REINDEX_INSTALLER="$(cd "$(dirname "$0")" && pwd)/install-reindex-schedule.sh"
if [ -f "$REINDEX_INSTALLER" ]; then
  AICX_BIN="$AICX_BIN" bash "$REINDEX_INSTALLER"
else
  note "index refresh: run 'aicx doctor --repair-runtime' once to install the 8640s scheduler"
fi

# Point Claude/Codex/Gemini at this LaunchAgent URL (not stdio aicx-mcp).
# install.sh sets AICX_SKIP_MCP_CLIENTS=1 and applies the same helper once.
if [ "${AICX_SKIP_MCP_CLIENTS:-0}" != "1" ]; then
  CLIENTS_HELPER="$(cd "$(dirname "$0")" && pwd)/configure_mcp_clients.py"
  if [ -f "$CLIENTS_HELPER" ] && command -v python3 >/dev/null 2>&1; then
    python3 "$CLIENTS_HELPER" \
      --wire-defaults \
      --transport http \
      --plist "$PLIST" \
      --token-file "${AICX_HOME:-$HOME/.aicx}/auth-token" || \
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
  note "index maintenance: separate; this reader never owns refresh"
  exit 0
fi

# Migrate machines still running the legacy io.vetcoders.aicx.mcp agent so
# install never leaves two HTTP MCP readers. Then idempotent refresh of the
# canonical label without deleting the plist we just wrote.
retire_launchd_label "$LEGACY_LABEL"
launchctl bootout "$(gui_domain)/$LABEL" 2>/dev/null || true
if ! try_bootstrap; then
  sleep 1
  try_bootstrap || true
fi

if service_loaded; then
  note "mcp service: running reader-only on http://$HOST:$PORT/mcp via $LABEL"
  note "index maintenance: separate process owner; MCP auto-refresh disabled"
  note "mcp service logs: $LOG_DIR/aicx-serve-http.log"
else
  note "mcp service: plist written to $PLIST (bootstrap failed in this Aqua session)"
  note "mcp service: retry: launchctl bootstrap gui/$(id -u) $PLIST"
  note "mcp service: do not run as root — this is a per-user LaunchAgent"
  note "index maintenance: separate; this reader never owns refresh"
fi

note "Note: the HTTP MCP service is a reader and does not refresh the index."
note "For periodic freshness install the separate maintenance schedule:"
note "  make install-schedule   # aicx catalog rebuild && aicx index on a cadence"
