#!/usr/bin/env bash
set -euo pipefail

SERVICE_NAME="aicx-sztudio-v4"
SERVICE_DIR="${AICX_SZTUDIO_SERVICE_DIR:-$HOME/.local/share/aicx/sztudio-v4}"
BIN="${AICX_SZTUDIO_BIN:-$SERVICE_DIR/bin/aicx-mcp}"
HOST="${AICX_SZTUDIO_HOST:-0.0.0.0}"
PORT="${AICX_SZTUDIO_PORT:-8069}"
TAILSCALE_HOST="${AICX_SZTUDIO_TAILSCALE_HOST:-100.75.30.90}"
AICX_HOME_PIN="${AICX_SZTUDIO_AICX_HOME:-/Users/silver/.cache/aicx-experiments/tb14d-anchor-v4-20260619-121428/aicx-home}"
AICX_EMBEDDER_CONFIG_PIN="${AICX_SZTUDIO_EMBEDDER_CONFIG:-/Users/silver/.cache/aicx-experiments/tb14d-anchor-v4-20260619-121428/.aicx/config.toml}"
TOKEN_FILE="${AICX_SZTUDIO_TOKEN_FILE:-$AICX_HOME_PIN/auth-token}"
LOG_DIR="${AICX_SZTUDIO_LOG_DIR:-$SERVICE_DIR/log}"
RUN_DIR="${AICX_SZTUDIO_RUN_DIR:-$SERVICE_DIR/run}"
LOG_FILE="$LOG_DIR/aicx-mcp.log"
PID_FILE="$RUN_DIR/aicx-mcp.pid"

usage() {
  cat <<EOF
Usage: $0 <command>

Commands:
  start        Start $SERVICE_NAME from the stable service directory
  stop         Stop the service by pid file, then by port fallback
  restart      Stop and start
  status       Show process, binary, env pins, token file mode, and log path
  health       Check local /health
  smoke-hint   Print the Silver-side smoke command

Environment overrides:
  AICX_SZTUDIO_SERVICE_DIR       default: $SERVICE_DIR
  AICX_SZTUDIO_BIN               default: $BIN
  AICX_SZTUDIO_HOST              default: $HOST
  AICX_SZTUDIO_PORT              default: $PORT
  AICX_SZTUDIO_TAILSCALE_HOST    default: $TAILSCALE_HOST
  AICX_SZTUDIO_AICX_HOME         default: $AICX_HOME_PIN
  AICX_SZTUDIO_EMBEDDER_CONFIG   default: $AICX_EMBEDDER_CONFIG_PIN
  AICX_SZTUDIO_TOKEN_FILE        default: $TOKEN_FILE
EOF
}

ensure_dirs() {
  mkdir -p "$SERVICE_DIR/bin" "$LOG_DIR" "$RUN_DIR" "$(dirname "$TOKEN_FILE")"
}

generate_token() {
  if command -v python3 >/dev/null 2>&1; then
    python3 - <<'PY'
import secrets
print(secrets.token_hex(32))
PY
    return
  fi
  openssl rand -hex 32
}

ensure_token() {
  ensure_dirs
  if [[ ! -s "$TOKEN_FILE" ]]; then
    umask 077
    generate_token > "$TOKEN_FILE"
  fi
  chmod 600 "$TOKEN_FILE"
  if [[ ! -s "$TOKEN_FILE" ]]; then
    echo "ERROR: token file is empty: $TOKEN_FILE" >&2
    exit 1
  fi
}

listener_pids() {
  lsof -tiTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true
}

pid_alive() {
  local pid="${1:-}"
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null
}

start() {
  ensure_token
  if [[ ! -x "$BIN" ]]; then
    echo "ERROR: binary is missing or not executable: $BIN" >&2
    exit 1
  fi

  local existing
  existing="$(listener_pids)"
  if [[ -n "$existing" ]]; then
    echo "ERROR: port $PORT already has listener pid(s): $existing" >&2
    echo "Run '$0 stop' or inspect the listener before starting." >&2
    exit 1
  fi

  export AICX_HOME="$AICX_HOME_PIN"
  export AICX_EMBEDDER_CONFIG="$AICX_EMBEDDER_CONFIG_PIN"

  {
    echo
    echo "===== $(date '+%Y-%m-%d %H:%M:%S %z') starting $SERVICE_NAME ====="
    echo "binary: $BIN"
    "$BIN" --version
    shasum -a 256 "$BIN" 2>/dev/null || true
    echo "AICX_HOME: $AICX_HOME"
    echo "AICX_EMBEDDER_CONFIG: $AICX_EMBEDDER_CONFIG"
    echo "token_file: $TOKEN_FILE"
  } >> "$LOG_FILE" 2>&1

  nohup "$BIN" --transport http --host "$HOST" --port "$PORT" >> "$LOG_FILE" 2>&1 &
  echo "$!" > "$PID_FILE"
  sleep 1

  local pid
  pid="$(cat "$PID_FILE")"
  if ! pid_alive "$pid"; then
    echo "ERROR: service failed to stay running; recent log:" >&2
    tail -n 80 "$LOG_FILE" >&2 || true
    exit 1
  fi

  health
  status
}

stop() {
  local pid=""
  if [[ -f "$PID_FILE" ]]; then
    pid="$(cat "$PID_FILE" || true)"
  fi

  if pid_alive "$pid"; then
    kill "$pid" || true
    for _ in {1..20}; do
      pid_alive "$pid" || break
      sleep 0.2
    done
    if pid_alive "$pid"; then
      kill -9 "$pid" || true
    fi
  fi

  local extra
  extra="$(listener_pids)"
  if [[ -n "$extra" ]]; then
    kill $extra || true
  fi
  rm -f "$PID_FILE"
  echo "stopped $SERVICE_NAME on port $PORT"
}

status() {
  echo "service: $SERVICE_NAME"
  echo "service_dir: $SERVICE_DIR"
  echo "binary: $BIN"
  if [[ -x "$BIN" ]]; then
    "$BIN" --version
    shasum -a 256 "$BIN" 2>/dev/null || true
  else
    echo "binary_status: missing"
  fi
  echo "host: $HOST"
  echo "port: $PORT"
  echo "tailnet_endpoint: http://$TAILSCALE_HOST:$PORT/mcp"
  echo "AICX_HOME: $AICX_HOME_PIN"
  echo "AICX_EMBEDDER_CONFIG: $AICX_EMBEDDER_CONFIG_PIN"
  echo "token_file: $TOKEN_FILE"
  if [[ -f "$TOKEN_FILE" ]]; then
    stat -f "token_mode: %Lp" "$TOKEN_FILE" 2>/dev/null || stat -c "token_mode: %a" "$TOKEN_FILE" 2>/dev/null || true
  else
    echo "token_mode: missing"
  fi
  echo "log_file: $LOG_FILE"
  echo "pid_file: $PID_FILE"
  echo "listener:"
  lsof -nP -iTCP:"$PORT" -sTCP:LISTEN 2>/dev/null || true
}

health() {
  curl -fsS --max-time 5 "http://127.0.0.1:$PORT/health" >/dev/null
  echo "health: PASS http://127.0.0.1:$PORT/health"
}

smoke_hint() {
  cat <<EOF
From Silver:

  cd /Users/silver/Git/aicx
  AICX_MCP_URL="http://$TAILSCALE_HOST:$PORT/mcp" \\
  AICX_MCP_TOKEN="\$(cat /Users/silver/.local/share/aicx/sztudio-v4/client-token)" \\
  AICX_MCP_EXPECT_ROWS=3918 \\
  AICX_MCP_EXPECT_BACKEND=hybrid_rrf \\
  AICX_MCP_EXPECT_SOURCE_CONTAINS="/aicx-home/store/tb14d-anchor-v4" \\
  tools/mcp-http-smoke.sh
EOF
}

case "${1:-}" in
  start) start ;;
  stop) stop ;;
  restart) stop; start ;;
  status) status ;;
  health) health ;;
  smoke-hint) smoke_hint ;;
  -h|--help|help|"") usage ;;
  *)
    echo "ERROR: unknown command: $1" >&2
    usage >&2
    exit 2
    ;;
esac
