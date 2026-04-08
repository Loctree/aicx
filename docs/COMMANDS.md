# Commands

`aicx` is the operator front door for agent session history. It orchestrates a
two-layer pipeline — both layers are operator-driven, nothing happens automatically:

| Layer | What | Command surface |
|-------|------|-----------------|
| **1 — Canonical corpus** | Extract, deduplicate, chunk agent logs into steerable markdown at `~/.aicx/`. This is ground truth. | `claude`, `codex`, `all`, `store`, `extract` |
| **2 — Semantic materialization** | Embed the canonical corpus into a vector + BM25 index (memex) for retrieval by agents and MCP tools. | `memex-sync`, or `--memex` on any extractor |

`aicx` is the orchestrator; memex is the retrieval kernel.

For the shortest “it works” path, see `README.md`.

## Defaults Worth Knowing

- **Layer 1 commands** (`claude`, `codex`, `all`, `store`) write to the canonical store and print nothing to stdout unless you pass `--emit`.
- **Layer 2** never runs automatically — you either call `memex-sync` explicitly or add `--memex` to an extractor.
- `refs` prints a compact summary by default; use `--emit paths` for raw file paths.
- `all --incremental` is the daily-driver watermark-tracked refresh path. `store` is store-first with no watermarks — best for backfills and targeted re-extraction.

## Global Options

`--no-redact-secrets`
- Default behavior is redaction enabled.
- Passing this flag disables redaction (not recommended unless you fully trust inputs and outputs).

## `aicx list`

List raw agent session sources on disk (pre-extraction inputs).

Shows Claude Code, Codex, and Gemini log paths with session counts and sizes.
This is what extractors will read from — use `refs` to see what is already in
the canonical store after extraction.

```bash
aicx list
```

## `aicx claude`

Extract + store Claude Code sessions into the canonical corpus (layer 1).

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
- `--memex` also materialize new chunks into the memex retrieval kernel (layer 2)
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

Extract + store Codex sessions into the canonical corpus (layer 1).

```bash
aicx codex [OPTIONS]
```

Same as `claude`, including `--emit <paths|json|none>` with default `none`, and assistant messages by default. Use `--user-only` if you want a user-only view.

Example:

```bash
aicx codex -p CodeScribe -H 48 --loctree --emit json | jq .
```

## `aicx all`

Extract + store from all agents (Claude + Codex + Gemini) into the canonical corpus (layer 1).

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

Extract a single session file and write to a specific output path (layer 1, direct).

Bypasses the canonical store — useful for one-off inspection or piping.

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

Build the canonical corpus in `~/.aicx/` from agent logs (layer 1).

Store-first corpus builder: extracts, deduplicates, chunks, and writes steerable
markdown. Unlike `all --incremental`, does not use watermarks — re-processes the
full lookback window every time. Best for backfills and targeted re-extraction.
Add `--memex` to also materialize new chunks into the memex retrieval kernel
(layer 2) — a shortcut for running `memex-sync` separately.

```bash
aicx store [OPTIONS]
```

Options:
- `-p, --project <PROJECT>...` project name(s)
- `-a, --agent <AGENT>` `claude`, `codex`, `gemini` (default: all)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `--user-only` exclude assistant + reasoning messages (default: assistant included)
- `--memex` also materialize new chunks into the memex retrieval kernel (layer 2)
- `--emit <paths|json|none>` stdout mode (default: `none`)

Notes:
- `store` is store-first, not watermark-driven.
- For incremental refreshes, use `aicx all --incremental --emit none`.

Example:

```bash
aicx store -p CodeScribe --agent claude -H 720 --emit paths
```

## `aicx search`

Fuzzy search across the canonical corpus (layer 1, filesystem-only).

Searches chunk content and frontmatter directly in `~/.aicx/` — works
immediately, no memex index needed. For embedding-aware semantic retrieval,
materialize the index with `memex-sync` first, then use MCP tools via
`aicx serve`.

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
# Fuzzy content search across canonical chunks (no memex needed)
aicx search "auth middleware regression"

# Scoped to a project and date range
aicx search "refactor" -p ai-contexters --date 2026-03-20..2026-03-28

# Compact JSON for agents or scripts
aicx search "dashboard" -p ai-contexters --score 60 --json

# Search for a specific day mentioned in query
aicx search "decisions march 2026"
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

Materialize the canonical corpus into the memex retrieval kernel (layer 2).

Reads chunks from `~/.aicx/`, embeds them, and upserts into the rmcp-memex
vector + BM25 index. Materialization is always operator-driven — nothing
syncs automatically. You either run this command explicitly, or use `--memex`
on any extractor as a one-shot shortcut.

```bash
aicx memex-sync [OPTIONS]
```

Options:
- `-n, --namespace <NAMESPACE>` vector namespace (default: `ai-contexts`)
- `--per-chunk` use per-chunk library writes instead of batch store (slower, more granular)
- `--db-path <DB_PATH>` override LanceDB path
- `--reindex` wipe the memex index and re-embed the entire canonical corpus; use after an embedding model or dimension change, or when the index has drifted from the canonical store

Typical flows:

```bash
# First build: embed all unsynced canonical chunks into the memex index
aicx memex-sync

# Incremental: only new chunks since last sync (same command, watermark-tracked)
aicx memex-sync

# Full rebuild: wipe index, re-embed everything
aicx memex-sync --reindex

# One-shot shortcut: extract + materialize in a single pass
aicx all -H 48 --memex
```

Notes:
- Default batch materialization embeds and upserts chunks in-process via the `rmcp-memex` library, preserving `project`, `agent`, `date`, `session_id`, and `kind` metadata for semantic filtering.
- The canonical store's nested structure is traversed automatically during materialization.
- If `~/.aicx/.aicxignore` exists, matching chunk paths are excluded before materialization and the final summary reports how many were ignored.
- On interactive terminals, `memex-sync` emits live scan/embed/index progress to stderr so large reindexes do not look hung.

## `aicx refs`

List chunks in the canonical store (layer 1 inventory).

Shows what extractors have already written to `~/.aicx/`. Use this to verify
corpus contents after extraction — `refs` operates on canonical chunks, not
raw agent logs (see `list` for raw source discovery).

```bash
aicx refs [OPTIONS]
```

Options:
- `-H, --hours <HOURS>` filter by canonical chunk date (default: `48`)
- `-p, --project <PROJECT>` filter by project
- `--emit <summary|paths>` stdout mode (default: `summary`)
- `--strict` filter out low-signal noise (<15 lines, task-notifications only)

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

Extract structured intents and decisions from the canonical store (layer 1).

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

Generate a searchable HTML dashboard from the canonical store (layer 1).

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

Manage extraction dedup state (watermarks and hashes).

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

Exposes search, steer, and rank tools over MCP for agent retrieval.
Layer 1 tools (`aicx_steer`, `aicx_search`) work immediately — they query the
canonical corpus on disk. Layer 2 (`aicx_search` with embedding mode) requires a
materialized memex index — run `aicx memex-sync` first to embed the corpus.

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
