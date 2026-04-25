# Commands

`aicx` is the operator front door for agent session logs. It is store-first and
operator-driven: nothing mutates your corpus unless you run a command.

| Layer | What | Command surface |
|-------|------|-----------------|
| **1 — Canonical corpus** | Extract, deduplicate, chunk agent logs into steerable markdown at `~/.aicx/`. This is ground truth. | `claude`, `codex`, `all`, `store`, `extract` |
| **2 — Retrieval surfaces** | Query the corpus through filesystem search, steering metadata, MCP tools, and the reusable native embedding library. | `search`, `steer`, `serve`, `aicx-embeddings` |

`aicx` owns the canonical corpus and portable local embedding foundation.
Roost/rust-memex owns the advanced retrieval/operator plane.

For the shortest “it works” path, see `README.md`.

## Defaults Worth Knowing

- **Layer 1 commands** (`claude`, `codex`, `all`, `store`) write to the canonical store and print nothing to stdout unless you pass `--emit`.
- `-p/--project` on extractors and `store` is a source-side discovery filter, not a promise that output will land in only one canonical repo bucket.
- `refs` is the active CLI inventory command for canonical chunks. It prints a compact summary by default; use `--emit paths` for raw file paths.
- There is currently no `aicx rank` CLI subcommand. Ranking stays on the MCP surface as `aicx_rank`.
- `init` is retired; framework bootstrap now lives in `/vc-init`.
- `claude`, `codex`, `all`, and `store` all use watermark-tracked incremental refresh by default. Use `--full-rescan` when you intentionally want a backfill that ignores the stored watermark.

## Redaction Scope

Secret redaction is enabled by default on corpus-building commands that read raw
session logs or emit fresh artifacts: `claude`, `codex`, `all`, `extract`, and
`store`.

Use `--no-redact-secrets` only on those commands when you intentionally want to
disable redaction.

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
- `-p, --project <PROJECT>...` source cwd/project filter(s)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `--no-redact-secrets` disable secret redaction for this run
- `-o, --output <DIR>` write local report files (omit to only write to store)
- `-f, --format <md|json|both>` local output format (default: `both`)
- `--append-to <FILE>` append local output to a single file
- `--rotate <N>` keep only last N local output files (default: `0` = unlimited)
- `--full-rescan` ignore the stored watermark and rescan the full lookback window
- `--user-only` exclude assistant + reasoning messages (default: assistant included)
- `--loctree` include loctree snapshot in local output
- `--project-root <DIR>` project root for loctree snapshot (defaults to cwd)
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
  "requested_source_filters": ["CodeScribe"],
  "resolved_repositories": ["VetCoders/CodeScribe"],
  "includes_non_repository_contexts": false,
  "resolved_store_buckets": {
    "VetCoders/CodeScribe": { "claude": 123 }
  },
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
- `all` still supports `--no-redact-secrets` when you intentionally want raw output.

Examples:

```bash
# Everything, last 7 days, incremental by default
aicx all -H 168 --emit none

# Same run, but print raw store chunk paths too
aicx all -H 168 --emit paths

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
- `--no-redact-secrets` disable secret redaction for this one-off extract
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
markdown. Like `claude`, `codex`, and `all`, it uses per-source watermarks by
default so repeat runs stay incremental. Use `--full-rescan` for backfills and
targeted re-extraction when you need to ignore the watermark.

```bash
aicx store [OPTIONS]
```

Options:
- `-p, --project <PROJECT>...` source cwd/project filter(s)
- `-a, --agent <AGENT>` `claude`, `codex`, `gemini` (default: all)
- `-H, --hours <HOURS>` lookback window (default: `48`)
- `--full-rescan` ignore the stored watermark and rescan the full lookback window
- `--no-redact-secrets` disable secret redaction for this corpus build
- `--user-only` exclude assistant + reasoning messages (default: assistant included)
- `--emit <paths|json|none>` stdout mode (default: `none`)

Notes:
- `store` is store-first, but still watermark-driven by default.
- For a deliberate backfill, use `aicx store --full-rescan`.
- `--emit json` distinguishes requested source filters from resolved canonical output buckets with `requested_source_filters`, `resolved_repositories`, and `resolved_store_buckets`.

Example:

```bash
aicx store -p CodeScribe --agent claude -H 720 --emit paths
```

## `aicx search`

Fuzzy search across the canonical corpus (layer 1, filesystem-only).

Searches chunk content and frontmatter directly in `~/.aicx/` — works
immediately, no semantic index needed. For semantic retrieval through MCP
tools, use `aicx serve`; the MCP layer widens through available runtime search
providers and otherwise falls back to canonical-store fuzzy search.

```bash
aicx search [OPTIONS] <QUERY>
```

Options:
- `<QUERY>` search query string
- `-p, --project <PROJECT>` repo or store-bucket filter (case-insensitive substring)
- `-H, --hours <HOURS>` lookback window (`0` = all time)
- `-d, --date <DATE>` filter by date (single day, range, or open-ended)
- `-l, --limit <N>` max results (default: `10`)
- `-s, --score <SCORE>` minimum quality threshold (`0..=100`)
- `-j, --json` emit compact JSON instead of plain text

Examples:

```bash
# Fuzzy content search across canonical chunks (no memex needed)
aicx search "auth middleware regression"

# Scoped to a repo or store bucket and date range
aicx search "refactor" -p ai-contexters --date 2026-03-20..2026-03-28

# Compact JSON for agents or scripts
aicx search "dashboard" -p ai-contexters --score 60 --json

# Search for a specific day mentioned in query
aicx search "decisions march 2026"
```

## `aicx steer`

Retrieve chunks by steering metadata (frontmatter sidecar fields). Filters by `run_id`, `prompt_id`, agent, kind, repo/store bucket, and/or date range using sidecar metadata — no filesystem grep needed.

```bash
aicx steer [OPTIONS]
```

Options:
- `--run-id <RUN_ID>` filter by run_id (exact match)
- `--prompt-id <PROMPT_ID>` filter by prompt_id (exact match)
- `-a, --agent <AGENT>` filter by agent: claude, codex, gemini
- `-k, --kind <KIND>` filter by kind: conversations, plans, reports, other
- `-p, --project <PROJECT>` filter by repo or store bucket (case-insensitive substring)
- `-d, --date <DATE>` filter by date: single day, range, or open-ended
- `-l, --limit <N>` max results (default: `20`)

Examples:

```bash
# All chunks from a specific run
aicx steer --run-id mrbl-001

# Reports for a repo or store bucket on a specific date
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
- `--no-intent-schema` skip the post-migration intent schema scan on the canonical store

Example:

```bash
aicx migrate --dry-run

# Full legacy -> canonical migration plus intent-schema pass from home directory
aicx migrate
```

## `aicx migrate-intent-schema`

Classify canonical chunks into the intent schema report. By default it scans the entire canonical store, so it can be launched from `~` just like `aicx migrate` or `aicx store`.

```bash
aicx migrate-intent-schema [OPTIONS]
```

Options:
- `-p, --project <PROJECT>` optional repo/store-bucket filter (case-insensitive substring)
- `--store-root <DIR>` override AICX root (default: `~/.aicx`)
- `--dry-run` show counts without writing changes

Examples:

```bash
# Scan every migrated project in the canonical store
aicx migrate-intent-schema

# Restrict the report to one project bucket
aicx migrate-intent-schema --project ai-contexters
```

## Native Embeddings

Native embeddings are a library/runtime surface, not a CLI indexing command.

Use `install.sh --pick-embedder` or edit `~/.aicx/embedder.toml`:

```toml
[native_embedder]
backend = "gguf"
profile = "base"
repo = "mradermacher/F2LLM-v2-0.6B-GGUF"
filename = "F2LLM-v2-0.6B.Q4_K_M.gguf"
prefer_embedded = false
max_length = 512
```

Hydrate manually when needed:

```bash
hf download mradermacher/F2LLM-v2-0.6B-GGUF F2LLM-v2-0.6B.Q4_K_M.gguf
```

See `docs/EMBEDDINGS.md` for the reusable `aicx-embeddings` API, GGUF profile
table, and the split between AICX local embeddings and Roost/rust-memex heavy
retrieval.

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
- `-p, --project <PROJECT>` filter by repo or store bucket
- `--emit <summary|paths>` stdout mode (default: `summary`)
- `--strict` filter out low-signal noise (<15 lines, task-notifications only)

Example:

```bash
aicx refs -H 72 -p CodeScribe
```

## `aicx rank`

There is currently no `aicx rank` CLI subcommand.

`refs` is not deprecated; it remains the canonical CLI inventory/readiness surface.
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

Generate a searchable HTML dashboard from the canonical store (layer 1), or serve it locally.

```bash
aicx dashboard [OPTIONS]
```

Options:
- `--store-root <DIR>` override store root
- `-p, --project <PROJECT>` narrow the dataset to project/store buckets containing this string
- `-H, --hours <HOURS>` narrow the dataset to the last N hours (omit for all time)
- `--serve` run the live local HTTP dashboard instead of generating static HTML
- `--generate-html` generate a standalone HTML file (default when no mode is passed)
- `-o, --output <OUTPUT>` output HTML path (default: `~/.aicx/aicx-dashboard.html`, generate mode only)
- `--host <HOST>` bind host (server mode only, default: `127.0.0.1`)
- `--port <PORT>` bind TCP port (server mode only, default: `9478`)
- `--bg` detach the server into the background (`--serve` implies `--no-open`)
- `--allow-cors-origins <PRESET|URL>` CORS policy for server mode: `local` (default), `tailscale`, `all`, or an explicit URL
- `--no-open` suppress automatic browser open on startup (server mode only)
- `--title <TITLE>` document title
- `--preview-chars <N>` max preview characters per record (`0` = no truncation)

Example:

```bash
aicx dashboard --generate-html -p ai-contexters -H 24 -o ./aicx-dashboard.html
aicx dashboard --serve -p ai-contexters -H 24 --port 9478
aicx dashboard --serve --host 0.0.0.0 --allow-cors-origins tailscale --bg
```

## `aicx reports`

Extract Vibecrafted workflow and marbles artifacts into a standalone HTML explorer.

The explorer embeds the selected report slice directly and also supports
client-side JSON bundle import/export from inside the HTML.

```bash
aicx reports [OPTIONS]
```

Options:
- `--artifacts-root <DIR>` override the Vibecrafted artifact root (default: `~/.vibecrafted/artifacts`)
- `--org <ORG>` artifact organization bucket (default: `VetCoders`)
- `--repo <REPO>` repo bucket (defaults to current directory name)
- `--workflow <FILTER>` case-insensitive filter across workflow label, skill code, run/prompt IDs, lane, and title
- `--date-from <YYYY-MM-DD|YYYY_MMDD>` inclusive start date
- `--date-to <YYYY-MM-DD|YYYY_MMDD>` inclusive end date
- `-o, --output <OUTPUT>` output HTML path (default: `~/.aicx/aicx-reports.html`)
- `--bundle-output <OUTPUT>` optional JSON bundle path for later import/merge
- `--title <TITLE>` document title
- `--preview-chars <N>` max preview characters per record (`0` = no truncation)

Example:

```bash
aicx reports \
  --repo ai-contexters \
  --workflow marbles \
  --date-from 2026-04-10 \
  --date-to 2026-04-12 \
  -o ~/.aicx/aicx-reports.html \
  --bundle-output ~/.aicx/aicx-reports.bundle.json
```

Compatibility note:
The hidden legacy aliases `aicx dashboard-serve` and `aicx reports-extractor`
still parse, but the supported operator surface is `aicx dashboard` and
`aicx reports`.

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

Run `aicx` as an MCP server (stdio or streamable HTTP transport).

Exposes search, steer, and rank tools over MCP for agent retrieval.
`aicx_steer` and `aicx_rank` query the canonical corpus on disk.
`aicx_search` uses canonical-store fuzzy search today; semantic widening belongs
to configured downstream retrieval providers and must fall back cleanly to the
canonical store.

```bash
aicx serve [OPTIONS]
```

Options:
- `--transport <stdio|http>` transport (default: `stdio`; legacy alias `sse` is still accepted)
- `--port <PORT>` streamable HTTP port (default: `8044`)

Example:

```bash
aicx serve --transport http --port 8044
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
