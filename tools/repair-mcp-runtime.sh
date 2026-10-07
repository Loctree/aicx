#!/usr/bin/env bash
set -euo pipefail
# Repair an existing macOS MCP LaunchAgent without resetting its bind address,
# port, allowed-host list, logging, or other operator-owned configuration.

LABEL="com.loctree.aicx.mcp"
LEGACY_LABEL="io.vetcoders.aicx.mcp"
CANONICAL_PLIST="$HOME/Library/LaunchAgents/$LABEL.plist"
LEGACY_PLIST="$HOME/Library/LaunchAgents/$LEGACY_LABEL.plist"

note() { printf '  %s\n' "$*"; }
gui_domain() { printf 'gui/%s' "$(id -u)"; }

# launchd removes a booted-out job asynchronously; an immediate bootstrap of
# the same label races that removal and fails with "Bootstrap failed: 5:
# Input/output error". Wait for the label to disappear, and retry bootstrap.
BOOTSTRAP_ATTEMPTS="${AICX_RUNTIME_BOOTSTRAP_ATTEMPTS:-5}"

wait_until_unloaded() {
  local label="$1"
  local tries=20
  while launchctl print "$(gui_domain)/$label" >/dev/null 2>&1; do
    tries=$((tries - 1))
    if [ "$tries" -le 0 ]; then
      return 1
    fi
    sleep 0.25
  done
  return 0
}

bootout_and_wait() {
  local label="$1"
  launchctl bootout "$(gui_domain)/$label" 2>/dev/null || true
  wait_until_unloaded "$label" || true
}

bootstrap_with_retry() {
  local plist="$1"
  local label="$2"
  local attempt=1
  local err=""
  while :; do
    err="$(launchctl bootstrap "$(gui_domain)" "$plist" 2>&1)" || true
    if launchctl print "$(gui_domain)/$label" >/dev/null 2>&1; then
      return 0
    fi
    if [ "$attempt" -ge "$BOOTSTRAP_ATTEMPTS" ]; then
      break
    fi
    attempt=$((attempt + 1))
    sleep 0.5
  done
  if [ -n "$err" ]; then
    printf '%s\n' "$err" >&2
  fi
  return 1
}

if [ "$(uname -s)" != "Darwin" ]; then
  note "runtime repair: skipped (launchd-only; this host is $(uname -s))"
  exit 0
fi

MANAGER="$(launchctl managername 2>/dev/null || true)"
if [ "$MANAGER" != "Aqua" ]; then
  echo "Error: runtime repair must run from a macOS GUI login (launchctl managername=${MANAGER:-unknown}); no service changes made" >&2
  exit 1
fi

AICX_BIN="${AICX_BIN:-$(command -v aicx || true)}"
if [ -z "$AICX_BIN" ] || [ ! -x "$AICX_BIN" ]; then
  echo "Error: runtime repair could not resolve an executable aicx launcher" >&2
  exit 1
fi

SOURCE_PLIST="$CANONICAL_PLIST"
if [ ! -f "$SOURCE_PLIST" ]; then
  SOURCE_PLIST="$LEGACY_PLIST"
fi
if [ ! -f "$SOURCE_PLIST" ]; then
  echo "Error: no AICX MCP LaunchAgent found; install the service before repairing it" >&2
  exit 1
fi
CANONICAL_WAS_LOADED=0
LEGACY_WAS_LOADED=0
if launchctl print "$(gui_domain)/$LABEL" >/dev/null 2>&1; then
  CANONICAL_WAS_LOADED=1
fi
if launchctl print "$(gui_domain)/$LEGACY_LABEL" >/dev/null 2>&1; then
  LEGACY_WAS_LOADED=1
fi

mkdir -p "$HOME/Library/LaunchAgents"
BACKUP_PLIST="$(mktemp "${TMPDIR:-/tmp}/aicx-mcp-plist.XXXXXX")"
cp "$SOURCE_PLIST" "$BACKUP_PLIST"
CANONICAL_EXISTED=1
if [ "$SOURCE_PLIST" = "$LEGACY_PLIST" ]; then
  CANONICAL_EXISTED=0
  cp "$LEGACY_PLIST" "$CANONICAL_PLIST"
fi
SERVICE_STOPPED=0
LEGACY_ARCHIVE=""
restore_plist() {
  if [ "$CANONICAL_EXISTED" = "1" ]; then
    cp "$BACKUP_PLIST" "$CANONICAL_PLIST"
  else
    rm -f "$CANONICAL_PLIST"
  fi
}
rollback_on_exit() {
  status=$?
  trap - EXIT
  if [ "$status" -ne 0 ]; then
    restore_plist
    if [ "$SERVICE_STOPPED" = "1" ]; then
      bootout_and_wait "$LABEL"
      bootout_and_wait "$LEGACY_LABEL"
      if [ -n "$LEGACY_ARCHIVE" ] && [ -f "$LEGACY_ARCHIVE" ]; then
        if [ -f "$LEGACY_PLIST" ]; then
          rm -f "$LEGACY_ARCHIVE"
        else
          mv -f "$LEGACY_ARCHIVE" "$LEGACY_PLIST"
        fi
      fi
      reload_failed=0
      if [ "$CANONICAL_WAS_LOADED" = "1" ]; then
        if ! bootstrap_with_retry "$CANONICAL_PLIST" "$LABEL"; then
          reload_failed=1
          echo "Recover manually: launchctl bootstrap $(gui_domain) $CANONICAL_PLIST" >&2
        fi
      fi
      if [ "$LEGACY_WAS_LOADED" = "1" ]; then
        if ! bootstrap_with_retry "$LEGACY_PLIST" "$LEGACY_LABEL"; then
          reload_failed=1
          echo "Recover manually: launchctl bootstrap $(gui_domain) $LEGACY_PLIST" >&2
        fi
      fi
      if [ "$CANONICAL_WAS_LOADED" = "0" ] && [ "$LEGACY_WAS_LOADED" = "0" ]; then
        note "rollback: previous MCP plist restored; previous services remained unloaded"
      elif [ "$reload_failed" = "0" ]; then
        note "rollback: previous MCP plist and loaded state restored"
      else
        echo "Warning: previous MCP plist restored, but a prior loaded service could not be reloaded" >&2
      fi
    fi
  fi
  rm -f "$BACKUP_PLIST"
  exit "$status"
}
trap rollback_on_exit EXIT

# Preserve every operator-owned argument, while normalizing the executable,
# native subcommand, and the one flag that keeps the long-lived server a reader.
/usr/libexec/PlistBuddy -c "Set :Label $LABEL" "$CANONICAL_PLIST"
NORMALIZED_ARGS="$({
  plutil -extract ProgramArguments json -o - "$CANONICAL_PLIST"
} | python3 -c '
import json
import sys

args = json.load(sys.stdin)
if not isinstance(args, list) or not args or not all(isinstance(arg, str) for arg in args):
    raise SystemExit("ProgramArguments must be a non-empty string array")

tail = args[1:]
if tail and tail[0] == "serve":
    tail = tail[1:]
tail = [
    arg for arg in tail
    if arg not in {"--experimental-auto-refresh", "--no-auto-refresh"}
]
print(json.dumps([sys.argv[1], "serve", *tail, "--no-auto-refresh"]))
' "$AICX_BIN")"
plutil -replace ProgramArguments -json "$NORMALIZED_ARGS" "$CANONICAL_PLIST"
plutil -lint "$CANONICAL_PLIST" >/dev/null

HEALTH_URL="$(plutil -extract ProgramArguments json -o - "$CANONICAL_PLIST" | python3 -c '
import json
import sys

args = json.load(sys.stdin)
def option(name, default):
    try:
        return args[args.index(name) + 1]
    except (ValueError, IndexError):
        return default

host = option("--host", "127.0.0.1")
port = option("--port", "8044")
if host in {"0.0.0.0", "::"}:
    host = "127.0.0.1"
if ":" in host and not host.startswith("["):
    host = f"[{host}]"
print(f"http://{host}:{port}/health")
')"
MCP_PORT="$(plutil -extract ProgramArguments json -o - "$CANONICAL_PLIST" | python3 -c '
import json
import sys

args = json.load(sys.stdin)
try:
    print(args[args.index("--port") + 1])
except (ValueError, IndexError):
    print("8044")
')"
MCP_HOST="$(plutil -extract ProgramArguments json -o - "$CANONICAL_PLIST" | python3 -c '
import json
import sys

args = json.load(sys.stdin)
try:
    print(args[args.index("--host") + 1])
except (ValueError, IndexError):
    print("127.0.0.1")
')"
HEALTH_ATTEMPTS="${AICX_RUNTIME_HEALTH_ATTEMPTS:-10}"
case "$HEALTH_ATTEMPTS" in
  ''|*[!0-9]*|0)
    echo "Error: AICX_RUNTIME_HEALTH_ATTEMPTS must be a positive integer" >&2
    exit 1
    ;;
esac
case "$BOOTSTRAP_ATTEMPTS" in
  ''|*[!0-9]*|0)
    echo "Error: AICX_RUNTIME_BOOTSTRAP_ATTEMPTS must be a positive integer" >&2
    exit 1
    ;;
esac

launchd_job_pid() {
  launchctl print "$(gui_domain)/$LABEL" 2>/dev/null | awk '
    $1 == "pid" && $2 == "=" && $3 ~ /^[0-9]+$/ { print $3; exit }
  '
}

pid_descends_from() {
  local candidate="$1"
  local ancestor="$2"
  local depth=0
  local parent

  while [ "$depth" -lt 8 ]; do
    if [ "$candidate" = "$ancestor" ]; then
      return 0
    fi
    parent="$(ps -o ppid= -p "$candidate" 2>/dev/null | tr -d '[:space:]')"
    case "$parent" in
      ''|*[!0-9]*) return 1 ;;
    esac
    if [ "$parent" -le 1 ] || [ "$parent" = "$candidate" ]; then
      return 1
    fi
    candidate="$parent"
    depth=$((depth + 1))
  done
  return 1
}

socket_owned_by_launchd_job() {
  local job_pid
  local lsof_host
  local lsof_selector
  local listener_pid
  local listener_pids

  job_pid="$(launchd_job_pid)"
  case "$job_pid" in
    ''|*[!0-9]*) return 1 ;;
  esac
  lsof_host="$MCP_HOST"
  case "$lsof_host" in
    0.0.0.0|::) lsof_selector="-iTCP:${MCP_PORT}" ;;
    *:*) lsof_selector="-iTCP@[${lsof_host}]:${MCP_PORT}" ;;
    *) lsof_selector="-iTCP@${lsof_host}:${MCP_PORT}" ;;
  esac
  listener_pids="$(lsof -nP -a "$lsof_selector" -sTCP:LISTEN -Fp 2>/dev/null | sed -n 's/^p\([0-9][0-9]*\)$/\1/p')"
  for listener_pid in $listener_pids; do
    if pid_descends_from "$listener_pid" "$job_pid"; then
      return 0
    fi
  done
  return 1
}

SERVICE_STOPPED=1
bootout_and_wait "$LEGACY_LABEL"
bootout_and_wait "$LABEL"
if ! bootstrap_with_retry "$CANONICAL_PLIST" "$LABEL"; then
  echo "Error: repaired MCP service did not load; rolling back" >&2
  exit 1
fi

ready=0
attempt=1
while [ "$attempt" -le "$HEALTH_ATTEMPTS" ]; do
  HTTP_STATUS="$(curl --silent --show-error --noproxy '*' \
      --connect-timeout 1 --max-time 2 --output /dev/null \
      --write-out '%{http_code}' "$HEALTH_URL" || true)"
  if [ "$HTTP_STATUS" = "200" ] && socket_owned_by_launchd_job; then
    ready=1
    break
  fi
  if [ "$attempt" -lt "$HEALTH_ATTEMPTS" ]; then
    sleep 0.25
  fi
  attempt=$((attempt + 1))
done
if [ "$ready" != "1" ]; then
  echo "Error: repaired MCP service did not own the listening socket and return HTTP 200 at $HEALTH_URL; rolling back" >&2
  exit 1
fi

if [ -f "$LEGACY_PLIST" ]; then
  LEGACY_ARCHIVE="$(mktemp "${LEGACY_PLIST}.migrated.$(date -u +%Y%m%dT%H%M%SZ).XXXXXX")"
  mv -f "$LEGACY_PLIST" "$LEGACY_ARCHIVE"
fi

trap - EXIT
rm -f "$BACKUP_PLIST"

note "mcp runtime: $LABEL is ready at $HEALTH_URL via $AICX_BIN serve"
note "mcp refresh: disabled in the long-lived reader (--no-auto-refresh)"
if [ -n "$LEGACY_ARCHIVE" ]; then
  note "legacy MCP plist archived: $LEGACY_ARCHIVE"
fi
