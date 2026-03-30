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
  "store_paths": ["~/.ai-contexters/..."]
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

Write chunked contexts into the global store (`~/.ai-contexters/`) and optionally sync to memex.

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

## `aicx memex-sync`

Sync stored chunks to `rmcp-memex` semantic index.

```bash
aicx memex-sync [OPTIONS]
```

Options:
- `-n, --namespace <NAMESPACE>` vector namespace (default: `ai-contexts`)
- `--per-chunk` use per-chunk upsert instead of batch index; preserves structured metadata (`project`, `agent`, `date`, `session_id`, `kind`) via sidecars
- `--db-path <DB_PATH>` override LanceDB path

Example:

```bash
aicx memex-sync --namespace ai-contexts
```

Notes:
- Default batch sync now enables `rmcp-memex index --preprocess` to strip common boilerplate before embedding.
- `--per-chunk` is slower, but it keeps metadata-rich upserts ready for future project/agent/date-aware filtering.

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

Rank and filter artifacts by content quality.

```bash
aicx rank [OPTIONS] --project <PROJECT>
```

Options:
- `-p, --project <PROJECT>` project filter (required)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `--strict` only show chunks scoring >= 5
- `--top <N>` show only top N bundles

Example:

```bash
aicx rank -p CodeScribe --strict --top 10
```

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
- `--artifact <ARTIFACT>` artifact path written on startup and regeneration
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

## `aicx init`

Initialize repo context and run an agent.

```bash
aicx init [OPTIONS]
```

Options:
- `-p, --project <PROJECT>` project name override
- `-a, --agent <AGENT>` `claude` or `codex`
- `--model <MODEL>` model override
- `-H, --hours <HOURS>` context horizon (default: `4800`)
- `--max-lines <MAX_LINES>` max lines per section (default: `1200`)
- `--user-only` exclude assistant + reasoning messages from context (default: assistant included)
- `--action <ACTION>` append a focus/action to the prompt
- `--agent-prompt <PROMPT>` append additional prompt text after core rules (verbatim)
- `--agent-prompt-file <PATH>` append prompt text loaded from a file (verbatim)
- `--no-run` build context/prompt only
- `--no-confirm` skip interactive confirmation
- `--no-gitignore` do not auto-modify `.gitignore`

Example:

```bash
aicx init --agent codex --no-confirm --action "Audit memory and propose a plan"
```

## Exit Codes

- `0` on success.
- `1` on errors (invalid args, IO failures, runtime errors).
- `--help` and `--version` exit `0`.
