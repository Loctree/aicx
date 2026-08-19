# Vetcoders HTTP MCP Services Runbook

**Version:** 1.0.0 (August 2026)
**Status:** Canonical Multi-Agent & Multi-Machine Architecture
**Scope:** `aicx-mcp` (Memory & Intent Search) + `loctree-mcp` / `loct watch --http` (Structural Code Perception)

---

## 1. Why Streamable HTTP instead of Stdio?

In multi-agent environments (fleet of 10–50 agents: Codex, Claude, Gemini, Antigravity, subagents):

| Feature | Stdio Mode | Streamable HTTP Mode (Recommended) |
| :--- | :--- | :--- |
| **Number of processes** | 1 process per agent (50 agents = 50 processes in `ps aux`) | **1 central daemon per host** |
| **RAM usage** | Memory duplication for each process (50 × 50MB+) | **Shared RAM (~30–60MB per process)** |
| **Cache management** | Disk contention over `.cache` files | **Single in-memory `Arc<Snapshot>` in RAM** |
| **Query speed** | Binary startup + cold read per call | **< 1–5 ms (instant RAM graph lookup)** |
| **Remote access** | Only locally on the working machine | **Network / Tailscale access (`dragon`, `div0`, `sztudio`)** |
| **Scalability** | Overwhelms CPU and file descriptor limits | **Tokio + Axum easily handles thousands of concurrent requests** |

---

## 2. Service 1: `aicx-mcp` (Memory & Intent Search Engine)

The installed service binds to `127.0.0.1` by default. Tailnet exposure is an
explicit operator choice:

```bash
AICX_MCP_HOST="$(tailscale ip -4)" \
  AICX_MCP_ALLOWED_HOSTS="$(hostname -s),$(tailscale ip -4),localhost,127.0.0.1" \
  make install-service
```

Keep client URLs on loopback unless the client actually runs on another trusted
Tailnet machine. Never commit the generated token value; use the token file or a
client-specific environment/header mechanism.

### 2.1. Live Refresh Behavior
* **Is `aicx serve` live?** **Yes.** `aicx serve` does not freeze a stale in-memory copy of the index. Every tool invocation (`aicx_search`, `aicx_steer`, `aicx_intents`) reads the live Tantivy and vector store state from disk (`~/.aicx/`).
* **Ingestion cadence:** HTTP mode owns a bounded async refresh loop: every 5 minutes it refreshes the hot 48-hour catalog window and incrementally publishes the lexical index. The blocking filesystem/index work runs outside Tokio request workers. Use `--refresh-interval-seconds` to tune it or `--no-auto-refresh` only when another explicit writer owns freshness.

### 2.2. Installation & Makefile Targets
* **Standard install (with wizard):**
  ```bash
  cd /Volumes/vc-workspace/Loctree/aicx
  make install
  ```
  *(Runs `install.sh`, installs binaries, registers `io.vetcoders.aicx.mcp`, and writes Claude/Codex/Gemini `mcpServers.aicx` as `{"url":"http://127.0.0.1:8044/mcp"}` — not stdio `command`. That server owns index refresh. `AICX_SKIP_MCP_SERVICE=1` keeps stdio.)*

* **Explicit service management:**
  ```bash
  make install-service    # Installs and starts io.vetcoders.aicx.mcp LaunchAgent
  make uninstall-service  # Stops and unregisters io.vetcoders.aicx.mcp LaunchAgent
  make install-schedule   # Legacy: standalone refresh only when no HTTP server is installed
  ```

### 2.3. Smoke Test
```bash
# 1. Healthcheck
curl -s http://127.0.0.1:8044/health && echo " (Health: OK)"

# 2. Handshake MCP
curl -s \
  -H "Authorization: Bearer $(cat ~/.aicx/auth-token)" \
  -H "Accept: application/json, text/event-stream" \
  -H "Content-Type: application/json" \
  -X POST http://127.0.0.1:8044/mcp \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke-test","version":"1.0"}}}'
```

---

## 3. Service 2: `loctree-mcp` (Universal Code Perception)

### 3.1. Two Models of Operation

#### Mode A: Universal Central Server (Shared across all repositories)
One server on port `5174` handles all repositories. Every tool call (`slice`, `impact`, `find`, `focus`, `repo-view`, `tree`) accepts the `project: "/path/to/repo"` parameter.
* **Capacity:** Runs with `--snapshot-cache-capacity 20` to keep up to 20 repository snapshots in RAM concurrently.

#### Mode B: Dedicated per-repo Watcher (`loct watch --http`)
In the active project directory:
```bash
loct watch --http --port 5174 &
```
* Watches files for modifications and recalculates graph snapshots on save.
* Supervises the child `loctree-mcp` process on `127.0.0.1:5174/mcp`.

### 3.2. Installation & Makefile Targets
* **Standard install in `loctree-suite`:**
  ```bash
  cd /Volumes/vc-workspace/Loctree/loctree-suite
  make install-all
  ```
  *(Compiles `loct`, `loctree`, `loctree-mcp`, `loctree-lsp`, codesigns, and registers the `io.vetcoders.loctree.mcp` launchd service).*

* **Explicit service management:**
  ```bash
  make install-service    # Installs and starts io.vetcoders.loctree.mcp LaunchAgent
  make uninstall-service  # Stops and unregisters io.vetcoders.loctree.mcp LaunchAgent
  ```

### 3.3. Smoke Test
```bash
curl -s \
  -H "Host: 127.0.0.1:5174" \
  -H "Accept: application/json, text/event-stream" \
  -H "Content-Type: application/json" \
  -X POST http://127.0.0.1:5174/mcp \
  -d '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"smoke-test","version":"1.0"}}}'
```

---

## 4. Client Configuration Matrix

### 4.1. Antigravity / Gemini IDE (`~/.gemini/config/mcp_config.json`)

```json
{
  "mcpServers": {
    "aicx-http": {
      "type": "streamable-http",
      "url": "http://127.0.0.1:8044/mcp",
      "headers": {
        "Authorization": "Bearer <AICX_LOCAL_TOKEN>"
      }
    },
    "loctree-http": {
      "type": "streamable-http",
      "url": "http://127.0.0.1:5174/mcp"
    }
  }
}
```

### 4.2. Claude Code (`~/.claude/mcp.json` or project `.mcp.json`)

```json
{
  "mcpServers": {
    "aicx": {
      "url": "http://127.0.0.1:8044/mcp",
      "headers": {
        "Authorization": "Bearer <AICX_LOCAL_TOKEN>"
      }
    },
    "loctree": {
      "url": "http://127.0.0.1:5174/mcp"
    }
  }
}
```

### 4.3. Codex (`~/.codex/config.toml`)

```toml
[mcp_servers.aicx]
url = "http://127.0.0.1:8044/mcp"
bearer_token_env_var = "AICX_MCP_TOKEN"

[mcp_servers.aicx.tools.aicx_search]
approval_mode = "approve"

[mcp_servers.aicx.tools.aicx_steer]
approval_mode = "approve"

[mcp_servers.aicx.tools.aicx_rank]
approval_mode = "approve"

[mcp_servers.loctree]
url = "http://127.0.0.1:5174/mcp"
```

---

## 5. macOS launchd LaunchAgents

The services are configured as LaunchAgents in `~/Library/LaunchAgents/` with `RunAtLoad=true` and `KeepAlive=true`:

| Service Label | Command | Default Port | Log Destination |
| :--- | :--- | :--- | :--- |
| **`io.vetcoders.aicx.mcp`** | `aicx serve --transport http` | `8044` | `~/.aicx/logs/aicx-serve-http.log` |
| **`io.vetcoders.loctree.mcp`** | `loctree-mcp --transport http` | `5174` | `~/.loctree/logs/loctree-serve-http.log` |

The former `io.vetcoders.aicx.reindex` timer is removed when the HTTP service is installed, preventing two competing index writers.

---

## 6. Useful Aliases and Operational Helpers

Add the following aliases to `~/.zshrc`:

```bash
# Check status of listening MCP ports
alias mcp-status='lsof -nP -iTCP:8044,5174 -sTCP:LISTEN'

# View live MCP logs
alias mcp-logs='tail -f ~/.aicx/logs/aicx-serve-http.log ~/.loctree/logs/loctree-serve-http.log'

# Check launchd registration
alias mcp-launchd='launchctl list | grep vetcoders'

# Restart all launchd MCP services
alias mcp-restart='launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/io.vetcoders.aicx.mcp.plist 2>/dev/null; \
  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.vetcoders.aicx.mcp.plist; \
  launchctl bootout gui/$(id -u) ~/Library/LaunchAgents/io.vetcoders.loctree.mcp.plist 2>/dev/null; \
  launchctl bootstrap gui/$(id -u) ~/Library/LaunchAgents/io.vetcoders.loctree.mcp.plist; \
  echo "🚀 Services restarted under launchd"'
```
