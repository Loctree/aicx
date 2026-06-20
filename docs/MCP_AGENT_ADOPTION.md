# AICX MCP Agent Adoption Runbook

This runbook turns a one-off streamable HTTP success into a repeatable agent
runtime. It deliberately separates four gates:

1. the Sztudio server is running the intended binary and AICX home,
2. the HTTP MCP transport is reachable and token-gated,
3. the MCP tool flow sees the expected semantic index,
4. agent clients are explicitly configured to use that endpoint.

Do not skip gate 2/3. A TCP connect or `/health` alone does not prove that
agents are using the intended corpus.

## Server Contract

Run the server on the host that owns the semantic index:

```bash
export AICX_HOME="/Users/silver/.cache/aicx-experiments/tb14d-anchor-v4-20260619-121428/aicx-home"
export AICX_EMBEDDER_CONFIG="/Users/silver/.cache/aicx-experiments/tb14d-anchor-v4-20260619-121428/.aicx/config.toml"
export AICX_HTTP_AUTH_TOKEN="<token>"

aicx-mcp --transport http \
  --host 0.0.0.0 \
  --port 8067 \
  --auth-token "$AICX_HTTP_AUTH_TOKEN"
```

For a permanent service, put the same binary path, environment, host, port, and
token source into the Sztudio launchd unit. Keep logs visible. The launchd unit
is operator-owned because it contains host paths and token policy.

Security contract:

- `127.0.0.1 --no-require-auth` is acceptable for local-only operator smoke.
- non-loopback `--no-require-auth` must refuse startup.
- non-loopback `/mcp` requires Bearer auth.
- `/health` is public liveness only; it does not prove the MCP tool plane.

macOS note: first run of a new binary may trigger the Application Firewall
prompt. If non-loopback curl hangs with `CLOSE_WAIT` and no request logs, check
the host UI/firewall before debugging MCP routing.

## Stable Sztudio Runtime

The proof server may be started manually during diagnosis, but the durable
operator runtime should not live in `/tmp`. The stable Sztudio service layout is:

```text
/Users/silver/.local/share/aicx/sztudio-v4/
  bin/aicx-mcp
  bin/aicx-sztudio-v4-service
  log/aicx-mcp.log
  run/aicx-mcp.pid
```

The service script is tracked in this repository:

```text
tools/aicx-sztudio-v4-service.sh
```

It pins the V4 runtime explicitly:

```text
AICX_HOME=/Users/silver/.cache/aicx-experiments/tb14d-anchor-v4-20260619-121428/aicx-home
AICX_EMBEDDER_CONFIG=/Users/silver/.cache/aicx-experiments/tb14d-anchor-v4-20260619-121428/.aicx/config.toml
host=0.0.0.0
port=8069
```

The server token lives at:

```text
/Users/silver/.cache/aicx-experiments/tb14d-anchor-v4-20260619-121428/aicx-home/auth-token
```

It must be mode `0600`. The server loads it through the normal AICX auth
resolution because the service pins `AICX_HOME`; the token is not passed on the
command line.

Useful Sztudio commands:

```bash
/Users/silver/.local/share/aicx/sztudio-v4/bin/aicx-sztudio-v4-service status
/Users/silver/.local/share/aicx/sztudio-v4/bin/aicx-sztudio-v4-service health
/Users/silver/.local/share/aicx/sztudio-v4/bin/aicx-sztudio-v4-service restart
/Users/silver/.local/share/aicx/sztudio-v4/bin/aicx-sztudio-v4-service smoke-hint
```

From Silver, the matching client token should be stored with `0600` at:

```text
/Users/silver/.local/share/aicx/sztudio-v4/client-token
```

Then smoke after every restart:

```bash
cd /Users/silver/Git/aicx

AICX_MCP_URL="http://100.75.30.90:8069/mcp" \
AICX_MCP_TOKEN="$(cat /Users/silver/.local/share/aicx/sztudio-v4/client-token)" \
AICX_MCP_EXPECT_ROWS=3918 \
AICX_MCP_EXPECT_BACKEND=hybrid_rrf \
AICX_MCP_EXPECT_SOURCE_CONTAINS="/aicx-home/store/tb14d-anchor-v4" \
tools/mcp-http-smoke.sh
```

### Restart

```bash
ssh sztudio '/Users/silver/.local/share/aicx/sztudio-v4/bin/aicx-sztudio-v4-service restart'
```

Then run the Silver smoke above. Do not declare the runtime healthy from
`/health` alone.

### Token Rotation

1. Stop the service on Sztudio.
2. Move the old server token aside instead of deleting it.
3. Start the service; it will create a fresh `AICX_HOME/auth-token` with mode
   `0600`.
4. Copy the new token to Silver's client-token file with mode `0600`.
5. Re-add/update `aicx-sztudio` in Claude Code because Claude stores the
   Authorization header in its config.
6. Ensure Codex sessions get `AICX_MCP_TOKEN` from the client-token file.
7. Run `tools/mcp-http-smoke.sh`.
8. Run one fresh Claude or Codex agent proof.

Do not rotate the token silently while agent sessions are actively using the
remote server.

## Smoke Before Agent Adoption

From the client machine, run:

```bash
export AICX_MCP_TOKEN="<same token>"

AICX_MCP_URL="http://100.75.30.90:8067/mcp" \
AICX_MCP_TOKEN="$AICX_MCP_TOKEN" \
AICX_MCP_EXPECT_ROWS=3918 \
AICX_MCP_EXPECT_BACKEND=hybrid_rrf \
AICX_MCP_EXPECT_SOURCE_CONTAINS="/aicx-home/store/tb14d-anchor-v4" \
tools/mcp-http-smoke.sh
```

The smoke checks:

- `/health` returns `200`,
- unauthenticated `/mcp` returns `401` or `403`,
- authenticated `initialize` returns `200` and a session id,
- `notifications/initialized` returns `202` or `200`,
- `tools/list` includes `aicx_search`,
- `aicx_index_status` reports `semantic_index_rows > 0` and optionally the
  expected row count,
- `aicx_search` returns the expected backend and does not contain
  `filesystem_fuzzy` or `semantic_unavailable`.

For a non-V4 runtime, omit or change the `AICX_MCP_EXPECT_*` values. Keep
`AICX_MCP_EXPECT_BACKEND=hybrid_rrf` when testing semantic/hybrid quality.

## Agent Client Configuration

Add the remote server first as a separate name, for example `aicx-sztudio`.
Leave the existing local stdio `aicx` entry in place until the remote path has
survived real agent use.

### Claude Code

Claude Code supports streamable HTTP MCP and headers:

```bash
claude mcp add \
  --scope user \
  --transport http \
  --header "Authorization: Bearer ${AICX_MCP_TOKEN}" \
  aicx-sztudio \
  http://100.75.30.90:8067/mcp

claude mcp get aicx-sztudio
```

If the token is expanded into a local config file, treat that file as secret
material and rotate the token before sharing configs.

### Codex

Codex supports streamable HTTP MCP with a bearer-token environment variable, but
non-interactive `codex exec` also needs the tools to be explicitly approved.
Without the per-tool approval config, Codex can discover `aicx-sztudio` and then
cancel the MCP call with `user cancelled MCP tool call`.

```bash
export AICX_MCP_TOKEN="<same token>"

codex mcp add aicx-sztudio \
  --url http://100.75.30.90:8067/mcp \
  --bearer-token-env-var AICX_MCP_TOKEN

codex mcp get aicx-sztudio
```

The environment variable must be present in the shell or app environment that
starts Codex. If Codex runs from the desktop app, verify the app receives the
token before declaring adoption complete.

For non-interactive proof runs, create a Codex profile with the full server
definition plus the exact tools that may run without a manual approval prompt:

```toml
# ~/.codex/aicx-sztudio-smoke.config.toml
[mcp_servers.aicx-sztudio]
url = "http://100.75.30.90:8067/mcp"
bearer_token_env_var = "AICX_MCP_TOKEN"

[mcp_servers.aicx-sztudio.tools.aicx_index_status]
approval_mode = "approve"

[mcp_servers.aicx-sztudio.tools.aicx_search]
approval_mode = "approve"
```

Then run a fresh proof session:

```bash
AICX_MCP_TOKEN="<same token>" codex exec \
  -p aicx-sztudio-smoke \
  -C /Users/silver/Git/aicx \
  -s read-only \
  --ephemeral \
  'Use MCP server aicx-sztudio. Call aicx_index_status with project omitted, then call aicx_search with query "po co Silverowi model embeddingowy", limit 1, slim true. Do not use shell. Return proof: server name, semantic_index_rows, readiness, backend, top project, top source path.'
```

Expected proof shape:

```text
mcp: aicx-sztudio/aicx_index_status (completed)
mcp: aicx-sztudio/aicx_search (completed)
semantic_index_rows: 3918
search backend: hybrid_rrf
top source path: .../tb14d-anchor-v4-.../aicx-home/store/tb14d-anchor-v4/...
```

## Agent-Level Proof

After adding the server, start a fresh agent session and ask it to call:

1. `aicx_index_status`
2. `aicx_search` with a known V4 query, for example:
   `po co Silverowi model embeddingowy`

The agent must report:

- MCP server name (`aicx-sztudio` during rollout),
- backend class (`hybrid_rrf`, not `filesystem_fuzzy`),
- `semantic_index_rows`,
- top result project/path under the intended store.

If an agent only says "search found something" without backend/index details,
the adoption proof is incomplete.

## Rollback

Remove only the remote entry:

```bash
claude mcp remove aicx-sztudio
codex mcp remove aicx-sztudio
```

The existing local stdio entry can remain untouched during rollout.
