#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Smoke-test an existing AICX streamable HTTP MCP endpoint.

Required:
  AICX_MCP_URL      MCP endpoint, e.g. http://100.75.30.90:8067/mcp

Usually required for non-loopback:
  AICX_MCP_TOKEN    Bearer token for /mcp

Optional assertions:
  AICX_MCP_EXPECT_ROWS              exact semantic_index_rows value
  AICX_MCP_EXPECT_READINESS         exact index readiness value
  AICX_MCP_EXPECT_BACKEND           expected search backend (default: hybrid_rrf)
  AICX_MCP_EXPECT_SOURCE_CONTAINS   substring that must appear in search output
  AICX_MCP_QUERY                    search query (default: po co Silverowi model embeddingowy)
  AICX_MCP_TIMEOUT                  curl max-time seconds (default: 8)
  AICX_MCP_SKIP_NOAUTH_CHECK=1      skip unauthenticated /mcp rejection check

Example:
  AICX_MCP_URL=http://100.75.30.90:8067/mcp \
  AICX_MCP_TOKEN="$AICX_HTTP_AUTH_TOKEN" \
  AICX_MCP_EXPECT_ROWS=3918 \
  AICX_MCP_EXPECT_BACKEND=hybrid_rrf \
  tools/mcp-http-smoke.sh
EOF
}

if [[ "${1:-}" == "-h" || "${1:-}" == "--help" ]]; then
  usage
  exit 0
fi

if [[ -z "${AICX_MCP_URL:-}" ]]; then
  usage >&2
  exit 2
fi

command -v curl >/dev/null || {
  echo "ERROR: curl not found" >&2
  exit 2
}
command -v python3 >/dev/null || {
  echo "ERROR: python3 not found" >&2
  exit 2
}

url="${AICX_MCP_URL}"
token="${AICX_MCP_TOKEN:-}"
timeout="${AICX_MCP_TIMEOUT:-8}"
query="${AICX_MCP_QUERY:-po co Silverowi model embeddingowy}"
expect_rows="${AICX_MCP_EXPECT_ROWS:-}"
expect_readiness="${AICX_MCP_EXPECT_READINESS:-}"
expect_backend="${AICX_MCP_EXPECT_BACKEND:-hybrid_rrf}"
expect_source="${AICX_MCP_EXPECT_SOURCE_CONTAINS:-}"
skip_noauth="${AICX_MCP_SKIP_NOAUTH_CHECK:-0}"

if [[ "$url" == */mcp ]]; then
  health_url="${AICX_MCP_HEALTH_URL:-${url%/mcp}/health}"
else
  health_url="${AICX_MCP_HEALTH_URL:-}"
fi

if [[ -z "$health_url" ]]; then
  echo "ERROR: AICX_MCP_HEALTH_URL is required when AICX_MCP_URL does not end with /mcp" >&2
  exit 2
fi

workdir="$(mktemp -d "${TMPDIR:-/tmp}/aicx-mcp-smoke.XXXXXX")"
trap 'rm -rf "$workdir"' EXIT

status_code() {
  awk 'tolower($1) ~ /^http/ { code=$2 } END { print code }' "$1" | tr -d '\r'
}

session_id_from_headers() {
  awk 'tolower($1) == "mcp-session-id:" { print $2; exit }' "$1" | tr -d '\r'
}

json_request() {
  python3 - "$@" <<'PY'
import json
import sys

kind = sys.argv[1]
if kind == "initialize":
    payload = {
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {"name": "aicx-mcp-smoke", "version": "0"},
        },
    }
elif kind == "initialized":
    payload = {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}}
elif kind == "tools_list":
    payload = {"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}
elif kind == "tool_call":
    name = sys.argv[2]
    args = json.loads(sys.argv[3])
    payload = {
        "jsonrpc": "2.0",
        "id": 3,
        "method": "tools/call",
        "params": {"name": name, "arguments": args},
    }
else:
    raise SystemExit(f"unknown request kind: {kind}")

print(json.dumps(payload, ensure_ascii=False))
PY
}

extract_mcp_text() {
  python3 - "$1" <<'PY'
import json
import sys
from pathlib import Path

body = Path(sys.argv[1]).read_text(encoding="utf-8", errors="replace")
last = None
for raw in body.splitlines():
    if not raw.startswith("data:"):
        continue
    payload = raw[5:].strip()
    if not payload or not payload.startswith("{"):
        continue
    msg = json.loads(payload)
    last = msg
    if "error" in msg:
        print(json.dumps(msg["error"], ensure_ascii=False), file=sys.stderr)
        raise SystemExit(1)
    result = msg.get("result")
    if not isinstance(result, dict):
        continue
    content = result.get("content")
    if isinstance(content, list):
        texts = [
            item.get("text", "")
            for item in content
            if isinstance(item, dict) and item.get("type") == "text"
        ]
        if texts:
            print("\n".join(texts))
            raise SystemExit(0)
    print(json.dumps(result, ensure_ascii=False))
    raise SystemExit(0)

if last is None:
    raise SystemExit("no JSON-RPC data event found in MCP response")
raise SystemExit("no result payload found in MCP response")
PY
}

post_mcp() {
  local request="$1"
  local headers="$2"
  local body="$3"
  shift 3
  curl -sS --max-time "$timeout" \
    -D "$headers" \
    -o "$body" \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    "$@" \
    --data "$request" \
    "$url"
}

assert_http_status() {
  local name="$1"
  local headers="$2"
  local expected="$3"
  local actual
  actual="$(status_code "$headers")"
  if [[ "$actual" != "$expected" ]]; then
    echo "ERROR: $name expected HTTP $expected, got ${actual:-<missing>}" >&2
    echo "--- headers ---" >&2
    sed -n '1,40p' "$headers" >&2
    exit 1
  fi
}

echo "AICX MCP smoke"
echo "  endpoint: $url"
echo "  health:   $health_url"

echo "== health =="
curl -sS --max-time "$timeout" -D "$workdir/health.headers" -o "$workdir/health.body" "$health_url"
assert_http_status "health" "$workdir/health.headers" "200"
echo "health: PASS"

if [[ -n "$token" && "$skip_noauth" != "1" ]]; then
  echo "== unauthenticated /mcp rejection =="
  if post_mcp "$(json_request initialize)" "$workdir/noauth.headers" "$workdir/noauth.body"; then
    noauth_status="$(status_code "$workdir/noauth.headers")"
    if [[ "$noauth_status" != "401" && "$noauth_status" != "403" ]]; then
      echo "ERROR: unauthenticated /mcp expected 401/403, got ${noauth_status:-<missing>}" >&2
      exit 1
    fi
  else
    echo "ERROR: unauthenticated /mcp request failed at transport level" >&2
    exit 1
  fi
  echo "noauth rejection: PASS (${noauth_status})"
fi

auth_args=()
if [[ -n "$token" ]]; then
  auth_args=(-H "Authorization: Bearer $token")
fi

echo "== initialize =="
post_mcp "$(json_request initialize)" "$workdir/init.headers" "$workdir/init.body" "${auth_args[@]}"
assert_http_status "initialize" "$workdir/init.headers" "200"
session_id="$(session_id_from_headers "$workdir/init.headers")"
if [[ -z "$session_id" ]]; then
  echo "ERROR: initialize response did not include mcp-session-id" >&2
  exit 1
fi
echo "initialize: PASS (session $session_id)"

echo "== initialized notification =="
post_mcp "$(json_request initialized)" "$workdir/initialized.headers" "$workdir/initialized.body" \
  "${auth_args[@]}" \
  -H "Mcp-Session-Id: $session_id"
initialized_status="$(status_code "$workdir/initialized.headers")"
if [[ "$initialized_status" != "202" && "$initialized_status" != "200" ]]; then
  echo "ERROR: initialized notification expected HTTP 202/200, got ${initialized_status:-<missing>}" >&2
  exit 1
fi
echo "initialized: PASS (${initialized_status})"

echo "== tools/list =="
post_mcp "$(json_request tools_list)" "$workdir/tools.headers" "$workdir/tools.body" \
  "${auth_args[@]}" \
  -H "Mcp-Session-Id: $session_id"
assert_http_status "tools/list" "$workdir/tools.headers" "200"
if ! grep -q "aicx_search" "$workdir/tools.body"; then
  echo "ERROR: tools/list did not include aicx_search" >&2
  exit 1
fi
echo "tools/list: PASS"

echo "== aicx_index_status =="
post_mcp "$(json_request tool_call aicx_index_status '{}')" \
  "$workdir/status.headers" \
  "$workdir/status.body" \
  "${auth_args[@]}" \
  -H "Mcp-Session-Id: $session_id"
assert_http_status "aicx_index_status" "$workdir/status.headers" "200"
extract_mcp_text "$workdir/status.body" > "$workdir/status.text"
python3 - "$workdir/status.text" "$expect_rows" "$expect_readiness" <<'PY'
import json
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
expect_rows = sys.argv[2]
expect_readiness = sys.argv[3]
data = json.loads(text)
rows = int(data.get("semantic_index_rows") or 0)
readiness = str(data.get("readiness") or "")
if rows <= 0:
    raise SystemExit(f"semantic_index_rows must be > 0, got {rows}")
if expect_rows and rows != int(expect_rows):
    raise SystemExit(f"semantic_index_rows expected {expect_rows}, got {rows}")
if expect_readiness and readiness != expect_readiness:
    raise SystemExit(f"readiness expected {expect_readiness}, got {readiness}")
print(f"index_status: PASS semantic_index_rows={rows} readiness={readiness}")
PY

echo "== aicx_search =="
search_args="$(python3 - "$query" <<'PY'
import json
import sys

print(json.dumps({"query": sys.argv[1], "limit": 1, "slim": True}, ensure_ascii=False))
PY
)"
post_mcp "$(json_request tool_call aicx_search "$search_args")" \
  "$workdir/search.headers" \
  "$workdir/search.body" \
  "${auth_args[@]}" \
  -H "Mcp-Session-Id: $session_id"
assert_http_status "aicx_search" "$workdir/search.headers" "200"
extract_mcp_text "$workdir/search.body" > "$workdir/search.text"
python3 - "$workdir/search.text" "$expect_backend" "$expect_source" <<'PY'
import json
import sys
from pathlib import Path

text = Path(sys.argv[1]).read_text(encoding="utf-8")
expect_backend = sys.argv[2]
expect_source = sys.argv[3]

if "filesystem_fuzzy" in text:
    raise SystemExit("search output contains filesystem_fuzzy")
if "semantic_unavailable" in text:
    raise SystemExit("search output contains semantic_unavailable")

data = json.loads(text)

def values_for_key(value, key):
    if isinstance(value, dict):
        for k, v in value.items():
            if k == key:
                yield v
            yield from values_for_key(v, key)
    elif isinstance(value, list):
        for item in value:
            yield from values_for_key(item, key)

backends = [str(v) for v in values_for_key(data, "backend")]
if expect_backend and expect_backend not in backends:
    raise SystemExit(f"backend expected {expect_backend}, got {backends or '<none>'}")
if expect_source and expect_source not in text:
    raise SystemExit(f"search output does not contain expected source substring: {expect_source}")

result_count = len(data.get("results") or data.get("items") or [])
if result_count <= 0:
    raise SystemExit("search returned zero results")
print(f"search: PASS backend={expect_backend or (backends[0] if backends else '<unknown>')} results={result_count}")
PY

echo "PASS: AICX MCP HTTP endpoint is usable by agents"
