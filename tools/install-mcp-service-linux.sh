#!/usr/bin/env bash
# systemd --user unit for the AICX loopback listener.
# Same reader-only contract as the macOS LaunchAgent: 127.0.0.1, no bearer.
set -euo pipefail

if [ "$(uname -s)" != "Linux" ]; then
  echo "mcp service: systemd --user is Linux-only; this host is $(uname -s)" >&2
  exit 1
fi

PORT="${AICX_MCP_PORT:-8044}"
if ! [[ "$PORT" =~ ^[0-9]{1,5}$ ]] || (( 10#$PORT < 1 || 10#$PORT > 65535 )); then
  echo "mcp service: AICX_MCP_PORT must be between 1 and 65535" >&2
  exit 1
fi
AICX_BIN="${AICX_BIN:-}"
if [ -z "$AICX_BIN" ]; then
  if [ -x "$HOME/.local/bin/aicx" ]; then
    AICX_BIN="$HOME/.local/bin/aicx"
  else
    AICX_BIN="$(command -v aicx || true)"
  fi
fi
if [ -z "$AICX_BIN" ] || [ ! -x "$AICX_BIN" ]; then
  echo "mcp service: aicx not found" >&2
  exit 1
fi
if [[ "$AICX_BIN" != /* ]]; then
  AICX_BIN="$(cd "$(dirname "$AICX_BIN")" && pwd)/$(basename "$AICX_BIN")"
fi

# ExecStart uses systemd syntax, not shell syntax. env execs the quoted path
# because systemd forbids quotes/backslashes in its executable (argv[0]) field.
# Escape specifiers/environment expansion and reject control characters.
if [[ "$AICX_BIN" == *$'\n'* || "$AICX_BIN" == *$'\r'* || "$AICX_BIN" == *$'\t'* ]]; then
  echo "mcp service: executable path contains unsupported control characters" >&2
  exit 1
fi
EXEC_BIN="${AICX_BIN//\\/\\\\}"
EXEC_BIN="${EXEC_BIN//\"/\\\"}"
EXEC_BIN="${EXEC_BIN//%/%%}"
EXEC_BIN="${EXEC_BIN//\$/\$\$}"

UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$UNIT_DIR"
UNIT="$UNIT_DIR/aicx-mcp.service"
cat > "$UNIT" <<UNIT
[Unit]
Description=AICX dashboard and MCP

[Service]
ExecStart=/usr/bin/env "$EXEC_BIN" serve --transport http --host 127.0.0.1 --port $PORT --no-require-auth --no-auto-refresh
Restart=on-failure

[Install]
WantedBy=default.target
UNIT

systemctl --user daemon-reload
systemctl --user enable --now aicx-mcp.service
echo "mcp service: dashboard http://127.0.0.1:$PORT/ and MCP http://127.0.0.1:$PORT/mcp"
