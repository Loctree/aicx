#!/usr/bin/env bash
# systemd --user unit for the AICX loopback listener.
# Same reader-only contract as the macOS LaunchAgent: 127.0.0.1, no bearer.
set -euo pipefail

if [ "$(uname -s)" != "Linux" ]; then
  echo "mcp service: systemd --user is Linux-only; this host is $(uname -s)" >&2
  exit 1
fi

PORT="${AICX_MCP_PORT:-8044}"
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

UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
mkdir -p "$UNIT_DIR"
UNIT="$UNIT_DIR/aicx-mcp.service"
cat > "$UNIT" <<UNIT
[Unit]
Description=AICX dashboard and MCP

[Service]
ExecStart=$AICX_BIN serve --transport http --host 127.0.0.1 --port $PORT --no-require-auth --no-auto-refresh
Restart=on-failure

[Install]
WantedBy=default.target
UNIT

systemctl --user daemon-reload
systemctl --user enable --now aicx-mcp.service
echo "mcp service: dashboard http://127.0.0.1:$PORT/ and MCP http://127.0.0.1:$PORT/mcp"
