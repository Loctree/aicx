# Commands

This is the current CLI surface for `aicx`.

For the shortest “it works” path, see `README.md`.

## Defaults Worth Knowing

- `claude`, `codex`, `all`, and `store` write to the central store and print nothing to stdout unless you pass `--emit`.
- `refs` prints a compact summary by default; use `--emit paths` for raw file paths.
- `all --incremental` is the watermark-driven refresh path. `store` is store-first and non-incremental.

## Global Options

`--no-redact-secrets`
- Default behavior is redaction enabled.
- Passing this flag disables redaction (not recommended unless you fully trust inputs and outputs).

## `aicx` (no subcommand)

Guided front door for the whole operator surface.

```bash
aicx [OPTIONS]
```

What it does:
- builds a doctor-style summary of sources, canonical store health, and daemon state
- suggests the next few actions instead of dumping the full command list first
- points to the shortest follow-up paths: `doctor`, `dashboard`, `latest`, and `search`

Examples:

```bash
# Default guided front door
aicx

# Same front door, scoped to one repo
aicx --project ai-contexters

# Same front door, but with a wider recent-activity window
aicx -H 168
```

Use `aicx --help` when you want the full command catalog instead of the guided start.

## `aicx doctor`

Human-friendly readiness summary for the whole operator surface.

```bash
aicx doctor [OPTIONS]
```

What it checks:
- raw local sources (`~/.claude`, `~/.codex`, `~/.gemini`)
- recent canonical store activity in `~/.aicx/`
- memex daemon reachability or last known snapshot
- recommended next steps based on what is missing

Options:
- `-H, --hours <HOURS>` recent activity window for the canonical store summary (default: `72`)
- `-p, --project <PROJECT>` project filter for canonical store checks
- `-j, --json` emit compact JSON instead of the human-readable summary
- `--fix` repair what can be repaired automatically, then rerun the check

Examples:

```bash
# One obvious "what is ready?" check
aicx doctor

# Same check, scoped to one repo
aicx doctor --project ai-contexters

# Let doctor refresh/store/start background indexing when it can
aicx doctor --fix

# Agent-friendly JSON output
aicx doctor -H 168 --json
```

## `aicx dashboard`

Generate a browser-friendly HTML snapshot from the canonical store.

```bash
aicx dashboard [OPTIONS]
```

Options:
- `--store-root <STORE_ROOT>` override the canonical store root (default: `~/.aicx`)
- `-o, --output <OUTPUT>` output HTML path (default: `aicx-dashboard.html`)
- `--title <TITLE>` page title (default: `AI Contexters Dashboard`)
- `--preview-chars <N>` preview length per record (default: `320`)
- `--open` open the generated snapshot in your default browser

Examples:

```bash
# Write a local snapshot in the current directory
aicx dashboard

# Generate and open the snapshot immediately
aicx dashboard --open

# Keep one project-focused snapshot elsewhere
aicx dashboard --project ai-contexters --output ~/Desktop/aicx-ai-contexters.html --open
```

Use this when you want a shareable or offline-friendly browser surface without leaving a long-running process behind.

## `aicx dashboard-serve`

Run the live local dashboard UI with API-backed regeneration and search helpers.

```bash
aicx dashboard-serve [OPTIONS]
```

Options:
- `--store-root <STORE_ROOT>` override the canonical store root (default: `~/.aicx`)
- `--host <HOST>` loopback host to bind (default: `127.0.0.1`)
- `--port <PORT>` TCP port for the local UI (default: `8033`)
- `--artifact <ARTIFACT>` legacy compatibility path retained for status surfaces
- `--title <TITLE>` page title (default: `AI Contexters Dashboard`)
- `--preview-chars <N>` preview length per record (default: `320`)
- `--open` open the local dashboard URL in your default browser

Examples:

```bash
# Start the live local dashboard
aicx dashboard-serve

# Start it and open the browser automatically
aicx dashboard-serve --open

# Run a project-scoped local UI on a different port
aicx dashboard-serve --project ai-contexters --port 8034 --open
```

Use this when you want the lowest-friction browser front door for a less technical operator while keeping the richer live search and regenerate surface.

## `aicx latest`

Fastest re-entry view for the newest stored chunks.

```bash
aicx latest [OPTIONS]
```

What it does:
- reads the newest canonical chunks from `~/.aicx/`
- orders them by canonical event time when sidecar telemetry is present, falling back to the canonical chunk date
- returns chainable refs plus a short preview so you can decide what to open next

Options:
- `-H, --hours <HOURS>` lookback window by canonical chunk date (default: `168`)
- `-p, --project <PROJECT>` project filter (substring match)
- `-l, --limit <LIMIT>` max chunks to show (`0` = unlimited, default: `5`)
- `--strict` filter out low-signal task-notification noise
- `-j, --json` emit compact JSON instead of the human-readable summary

Examples:

```bash
# Show the newest five chunks across everything visible
aicx latest

# Re-enter one repo quickly
aicx latest --project ai-contexters

# Keep the list tight and noise-filtered
aicx latest --project ai-contexters --strict --limit 3

# Agent-friendly JSON for scripting
aicx latest --project ai-contexters --json
```

The `store_ref` values returned by `latest` are chainable into `aicx read`.

## `aicx list`

List available local sources and their sizes.

```bash
aicx list
```

## `aicx claude`

Extract timeline from Claude Code sessions.

```bash
aicx claude [OPTIONS]
```

Common options:
- `-p, --project <PROJECT>...` project directory filter(s)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `-o, --output <DIR>` write local report files (omit to only write to store)
- `-f, --format <md|json|both>` local output format (default: `both`)
- `--append-to <FILE>` append local output to a single file
- `--rotate <N>` keep only last N local output files (default: `0` = unlimited)
- `--incremental` incremental mode using a per-source watermark
- `--user-only` exclude assistant + reasoning messages (default: assistant included)
- `--loctree` include loctree snapshot in local output
- `--project-root <DIR>` project root for loctree snapshot (defaults to cwd)
- `--memex` also chunk + sync to memex after extraction
- `--force` ignore dedup hashes for this run
- `--emit <paths|json|none>` stdout mode (default: `none`)

Examples:

```bash
# Last 24h, store-first chunks, keep stdout quiet
aicx claude -p CodeScribe -H 24

# Print chunk paths explicitly
aicx claude -p CodeScribe -H 24 --emit paths

# Also write a local JSON report
aicx claude -p CodeScribe -H 24 -o ./reports -f json

# Automation-friendly JSON payload on stdout
aicx claude -p CodeScribe -H 24 --emit json | jq .
```

`--emit json` payload shape (stable fields):

```json
{
  "generated_at": "2026-02-08T03:12:34Z",
  "project_filter": "CodeScribe",
  "hours_back": 24,
  "total_entries": 123,
  "sessions": ["..."],
  "entries": [{ "...": "..." }],
  "store_paths": ["~/.aicx/..."]
}
```

## `aicx codex`

Extract timeline from Codex history.

```bash
aicx codex [OPTIONS]
```

Same as `claude`, including `--emit <paths|json|none>` with default `none`, and assistant messages by default. Use `--user-only` if you want a user-only view.

Example:

```bash
aicx codex -p CodeScribe -H 48 --loctree --emit json | jq .
```

## `aicx all`

Extract from all supported agents (Claude + Codex + Gemini).

```bash
aicx all [OPTIONS]
```

Options are similar to `claude`, with two important details:
- `all` does not expose `--format` because local report writing is hardcoded to `both`.
- `all` defaults to `--emit none`, so stdout stays quiet unless you opt in.

Examples:

```bash
# Everything, last 7 days, incremental
aicx all -H 168 --incremental --emit none

# Same run, but print raw store chunk paths too
aicx all -H 168 --incremental --emit paths

# User-only mode (exclude assistant + reasoning)
aicx all -H 48 --user-only
```

## `aicx extract`

Extract timeline from a single agent session file (direct path).

```bash
aicx extract --format <claude|codex|gemini|gemini-antigravity> --output <FILE> <INPUT>
```

Options:
- `--format <FORMAT>` input format / agent
- `gemini` reads classic Gemini CLI JSON sessions from `~/.gemini/tmp/.../session-*.json`
- `gemini-antigravity` resolves either `conversations/<uuid>.pb` or `brain/<uuid>/`, prefers readable conversation artifacts inside `brain/<uuid>/`, and explicitly falls back to `.system_generated/steps/*/output.txt` when no chat-grade artifact is readable
- `-o, --output <OUTPUT>` output file path
- `--user-only` exclude assistant + reasoning messages
- `--max-message-chars <N>` truncate huge messages in markdown (`0` = no truncation)

Example:

```bash
aicx extract --format claude /path/to/session.jsonl -o /tmp/report.md
aicx extract --format gemini-antigravity ~/.gemini/antigravity/conversations/<uuid>.pb -o /tmp/report.md
```

## `aicx store`

Write chunked contexts into the global store (`~/.aicx/`) and optionally sync to memex.

```bash
aicx store [OPTIONS]
```

Options:
- `-p, --project <PROJECT>...` project name(s)
- `-a, --agent <AGENT>` `claude`, `codex`, `gemini` (default: all)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `--user-only` exclude assistant + reasoning messages (default: assistant included)
- `--memex` also chunk + sync to memex
- `--emit <paths|json|none>` stdout mode (default: `none`)

Notes:
- `store` is store-first, not watermark-driven.
- For incremental refreshes, use `aicx all --incremental --emit none`.

Example:

```bash
aicx store -p CodeScribe --agent claude -H 720 --emit paths
```

## `aicx search`

Ad-hoc terminal fuzzy search across the `aicx` store. Uses `rmcp-memex` fast index (LanceDB + BM25) if available, falling back to sequential file scans.

```bash
aicx search [OPTIONS] <QUERY>
```

Options:
- `<QUERY>` search query string
- `-p, --project <PROJECT>` project filter (substring match)
- `-H, --hours <HOURS>` lookback window (`0` = all time)
- `-d, --date <DATE>` filter by date (single day, range, or open-ended)
- `-l, --limit <N>` max results (default: `10`)
- `-s, --score <SCORE>` minimum quality threshold (`0..=100`)
- `-j, --json` emit compact JSON instead of plain text

Examples:

```bash
# Fast semantic search
aicx search "auth middleware regression"

# Scoped to a project and date range
aicx search "refactor" -p ai-contexters --date 2026-03-20..2026-03-28

# Compact JSON for agents or scripts
aicx search "dashboard" -p ai-contexters --score 60 --json

# Search for a specific day mentioned in query
aicx search "decisions march 2026"
```

After `search`, open one promising chunk directly with `aicx read <ref-or-path>`.

## `aicx read`

Open one stored chunk by AICX ref or absolute path. This is the selective re-entry step after `search`, `refs`, or `steer`.

```bash
aicx read [OPTIONS] <TARGET>
```

Options:
- `<TARGET>` store-relative ref under `~/.aicx/` or absolute chunk path
- `--max-chars <N>` truncate content after N UTF-8 characters (`0` = full chunk)
- `--max-lines <N>` truncate content after N lines (`0` = full chunk)
- `-j, --json` emit compact JSON instead of the human-readable view

Examples:

```bash
# Read by store-relative ref
aicx read store/VetCoders/ai-contexters/2026_0331/reports/codex/2026_0331_codex_sess-read01_001.md

# Read by absolute path copied from another command
aicx read /Users/you/.aicx/store/VetCoders/ai-contexters/2026_0331/reports/codex/2026_0331_codex_sess-read01_001.md

# Keep the payload short for scripting or agent handoff
aicx read store/VetCoders/ai-contexters/.../chunk.md --max-lines 40 --json
```

## `aicx steer`

Retrieve chunks by steering metadata (frontmatter sidecar fields). Filters by `run_id`, `prompt_id`, agent, kind, project, and/or date range using sidecar metadata — no filesystem grep needed.

```bash
aicx steer [OPTIONS]
```

Options:
- `--run-id <RUN_ID>` filter by run_id (exact match)
- `--prompt-id <PROMPT_ID>` filter by prompt_id (exact match)
- `-a, --agent <AGENT>` filter by agent: claude, codex, gemini
- `-k, --kind <KIND>` filter by kind: conversations, plans, reports, other
- `-p, --project <PROJECT>` filter by project (case-insensitive substring)
- `-d, --date <DATE>` filter by date: single day, range, or open-ended
- `-l, --limit <N>` max results (default: `20`)

Examples:

```bash
# All chunks from a specific run
aicx steer --run-id mrbl-001

# Reports for a project on a specific date
aicx steer --project ai-contexters --kind reports --date 2026-03-28

# All claude chunks in a date range
aicx steer --agent claude --date 2026-03-20..2026-03-28

# Chunks from a specific prompt
aicx steer --prompt-id api-redesign_20260327
```

Paths returned by `steer` are chainable into `aicx read`.

## `aicx migrate`

Truthfully rebuild legacy contexts into canonical AICX store or salvage them under legacy-store.

```bash
aicx migrate [OPTIONS]
```

Options:
- `--dry-run` show what would be moved without modifying files
- `--legacy-root <DIR>` override legacy input store root (default: `~/.ai-contexters`)
- `--store-root <DIR>` override AICX store root (default: `~/.aicx`)

Example:

```bash
aicx migrate --dry-run
```

## `aicx memex-sync`

Sync stored chunks to `rmcp-memex` semantic index.

```bash
aicx memex-sync [OPTIONS]
```

Options:
- `-n, --namespace <NAMESPACE>` vector namespace (default: `ai-contexts`)
- `--per-chunk` use per-chunk upsert instead of batch import; preserves structured metadata via sidecars
- `--db-path <DB_PATH>` override LanceDB path

Example:

```bash
aicx memex-sync --namespace ai-contexts
```

Notes:
- Default batch sync now uses a metadata-rich import via JSONL, ensuring `project`, `agent`, `date`, and `session_id` are preserved for semantic filtering without the overhead of per-file CLI calls.
- Recursive indexing is enabled by default to handle the nested canonical store structure.
- If `~/.aicx/.aicxignore` exists, matching chunk paths are excluded before memex materialization and the final summary reports how many were ignored.
- On interactive terminals, `memex-sync` emits live scan/embed/index progress to stderr so large reindexes do not look hung.
- For always-on upkeep, prefer `aicx-memex daemon` or `aicx daemon`; the daemon bootstraps once, then keeps canonical refresh, steer repair, and memex sync moving in the background.

## `aicx daemon`

Start the background indexer daemon on a Unix socket. `aicx-memex daemon` is the dedicated daemon-first surface for the same control plane.

```bash
aicx daemon [OPTIONS]
aicx-memex daemon [OPTIONS]
```

Options:
- `--socket-path <PATH>` custom Unix socket path
- `--foreground` keep the daemon in the current terminal instead of detaching
- `--poll-seconds <SECONDS>` poll interval between sync cycles (default: `300`)
- `--refresh-hours <HOURS>` lookback window for the incremental canonical refresh (default: `720`)
- `-p, --project <PROJECT>...` optional project filter(s) for the refresh loop
- `-n, --namespace <NAMESPACE>` semantic namespace (default: `ai-contexts`)
- `--db-path <DB_PATH>` override LanceDB path
- `--per-chunk` use per-chunk upserts instead of batch import
- `--no-bootstrap` skip the initial startup cycle

Examples:

```bash
# Daily-driver mode: detach into the background
aicx-memex daemon

# Keep logs in the current terminal
aicx daemon --foreground --project ai-contexters
```

Notes:
- Startup bootstrap runs one full refresh/repair/materialization pass unless you opt out with `--no-bootstrap`.
- The daemon persists status in `~/.aicx/daemon/aicx-memex.status.json`.
- If memex runtime truth drifts after an install/update, the daemon can automatically reset and rebuild the semantic index from canonical store outputs.
- `./install.sh` now starts this daemon when needed, or queues a fresh sync on an already-running instance, so upgrades get a background catch-up pass automatically.

## `aicx daemon-status`

Show live daemon status from the Unix-socket control plane, or the last known persisted snapshot when the daemon is offline. `aicx-memex status` is the daemon-first surface for the same endpoint.

```bash
aicx daemon-status [OPTIONS]
aicx-memex status [OPTIONS]
```

Options:
- `--socket-path <PATH>` custom Unix socket path
- `-j, --json` emit JSON instead of plain text

Example:

```bash
aicx daemon-status --json
```

## `aicx daemon-sync`

Queue an immediate sync cycle on the background daemon. `aicx-memex sync` is the daemon-first surface for the same endpoint.

```bash
aicx daemon-sync [OPTIONS]
aicx-memex sync [OPTIONS]
```

Options:
- `--socket-path <PATH>` custom Unix socket path

Example:

```bash
aicx daemon-sync
```

## `aicx daemon-stop`

Stop the background daemon cleanly. `aicx-memex stop` is the daemon-first surface for the same endpoint.

```bash
aicx daemon-stop [OPTIONS]
aicx-memex stop [OPTIONS]
```

Options:
- `--socket-path <PATH>` custom Unix socket path

Example:

```bash
aicx daemon-stop
```

## `aicx refs`

List reference context files from the global store.

```bash
aicx refs [OPTIONS]
```

Options:
- `-H, --hours <HOURS>` filter by file mtime (default: `48`)
- `-p, --project <PROJECT>` filter by project
- `--emit <summary|paths>` stdout mode (default: `summary`)
- `--strict` exclude low-signal noise artifacts

Example:

```bash
aicx refs -H 72 -p CodeScribe
```

## `aicx rank`

There is currently no `aicx rank` CLI subcommand.

Ranking is exposed through the MCP surface as `aicx_rank`. For terminal use,
prefer `aicx search`, `aicx refs --strict`, or the dashboard views until a CLI
rank surface is intentionally reintroduced.

## `aicx intents`

Extract structured intents and decisions from stored context.

```bash
aicx intents [OPTIONS] --project <PROJECT>
```

Options:
- `-p, --project <PROJECT>` project filter (required)
- `-H, --hours <HOURS>` lookback window (default: `720`)
- `--emit <markdown|json>` output format (default: `markdown`)
- `--strict` only show high-confidence intents
- `--kind <decision|intent|outcome|task>` filter by kind

Example:

```bash
aicx intents -p CodeScribe --strict --kind decision
```

## `aicx dashboard`

Generate a searchable HTML dashboard from the store.

```bash
aicx dashboard [OPTIONS]
```

Options:
- `--store-root <DIR>` override store root
- `-o, --output <OUTPUT>` output HTML path (default: `aicx-dashboard.html`)
- `--title <TITLE>` document title
- `--preview-chars <N>` max preview characters per record (`0` = no truncation)

Example:

```bash
aicx dashboard -p CodeScribe -H 168 -o ./aicx-dashboard.html
```

## `aicx dashboard-serve`

Run the dashboard HTTP server with on-demand regeneration endpoints.

```bash
aicx dashboard-serve [OPTIONS]
```

Options:
- `--store-root <DIR>` override store root
- `--host <HOST>` bind host (default: `127.0.0.1`)
- `--port <PORT>` bind TCP port (default: `8033`)
- `--artifact <ARTIFACT>` legacy compatibility path surfaced in status; not written in server mode
- `--title <TITLE>` document title
- `--preview-chars <N>` max preview characters per record

Example:

```bash
aicx dashboard-serve --port 8033
```

## `aicx state`

Manage dedup state.

```bash
aicx state [OPTIONS]
```

Options:
- `--info` show state statistics
- `--reset` reset dedup hashes
- `-p, --project <PROJECT>` project scope for reset

Example:

```bash
aicx state --info
```

## `aicx serve`

Run `aicx` as an MCP server (stdio or streamable HTTP/SSE transport).

```bash
aicx serve [OPTIONS]
```

Options:
- `--transport <stdio|sse>` transport (default: `stdio`)
- `--port <PORT>` SSE/HTTP port (default: `8044`)

Example:

```bash
aicx serve --transport sse --port 8044
```

## `aicx init` (Retired)

`aicx init` has been retired. Context initialisation is now handled by `/vc-init` inside Claude Code.

See: [vibecrafted.io](https://vibecrafted.io/)

```bash
# aicx init [OPTIONS] -- retired
```

## Exit Codes

- `0` on success.
- `1` on errors (invalid args, IO failures, runtime errors).
- `--help` and `--version` exit `0`.
