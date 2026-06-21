# Commands

`aicx` is the operator front door for agent session logs. It is store-first and
operator-driven: nothing mutates your corpus unless you run a command.

| Layer | What | Command surface |
|-------|------|-----------------|
| **1 — Canonical corpus** | Extract, deduplicate, chunk agent logs into steerable markdown at `~/.aicx/`. This is ground truth. | `claude`, `codex`, `all`, `store`, `extract` |
| **1b — Context corpus** | Append-only retention for `loct-context-pack` prism artifacts at `~/.aicx/context-corpus/`. Excluded from live-truth retrieval and `aicx intents`; materializes into a separate `context-corpus.embeddings.ndjson` namespace. See [`CONTEXT_CORPUS.md`](./CONTEXT_CORPUS.md). | `ingest --source loct-context-pack <PACK_DIR>`, `doctor --check-dedup` |
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

Shows Claude Code, Codex, Gemini, and Junie log paths with session counts and sizes.
This is what extractors will read from — use `refs` to see what is already in
the canonical store after extraction.
It also reports source-protection status: existing local `.git` protection,
remote presence, or an explicit unprotected-source warning. This command is
read-only and never initializes git.

```bash
aicx list
```

## `aicx sources protect`

Opt in to local source-root protection. Dry run is the default:

```bash
aicx sources protect --root "$HOME/.codex" --backend git-local
```

Apply explicitly:

```bash
aicx sources protect --root "$HOME/.codex" --backend git-local --apply
```

The `git-local` backend creates `.git` only under the requested root, adds safe
`.gitignore` suggestions unless `--no-gitignore` is passed, and never configures
a remote by default. Use `--initial-snapshot` only when retaining current
source contents in local git history is intended. See
`docs/SOURCE_PROTECTION.md` for the privacy and team-sharing policy.

## `aicx claude`

Extract + store Claude Code sessions into the canonical corpus.

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

Extract + store Codex sessions into the canonical corpus.

```bash
aicx codex [OPTIONS]
```

Same as `claude`, including `--emit <paths|json|none>` with default `none`, and assistant messages by default. Use `--user-only` if you want a user-only view.

Example:

```bash
aicx codex -p CodeScribe -H 48 --loctree --emit json | jq .
```

## `aicx all`

Extract + store from all agents (Claude + Codex + Gemini + Junie) into the canonical corpus.

```bash
aicx all [OPTIONS]
```

Options are similar to `claude`, with two important details:
- `all` does not expose `--format` because local report writing is hardcoded to `both`.
- `all` defaults to `--emit none`, so stdout stays quiet unless you opt in.
- `all` still supports `--no-redact-secrets` when you intentionally want raw output.
- `-H 0` means all time, matching the retrieval/MCP contract.

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

Extract a single session file and write to a specific output path.

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

Build the canonical corpus in `~/.aicx/` from agent logs.

Store-first corpus builder: extracts, deduplicates, chunks, and writes steerable
markdown. Like `claude`, `codex`, and `all`, it uses per-source watermarks by
default so repeat runs stay incremental. Use `--full-rescan` for backfills and
targeted re-extraction when you need to ignore the watermark.

```bash
aicx store [OPTIONS]
```

Options:
- `-p, --project <PROJECT>...` source cwd/project filter(s)
- `-a, --agent <AGENT>` `claude`, `codex`, `gemini`, `junie` (default: all)
- `-H, --hours <HOURS>` lookback window (default: `48`, `0` = all time)
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

## `aicx corpus`

Audit and deterministically repair derived markdown corpora. Raw JSONL and log
files remain provenance; this surface is for cleaned-but-faithful markdown that
feeds retrieval.

```bash
aicx corpus audit [OPTIONS]
aicx corpus repair [OPTIONS]
```

Options:
- `--root <DIR>...` corpus roots to scan (default: `$HOME/.aicx`, `$HOME/.ai-contexters`, optional `$HOME/.xcia`)
- `--emit <text|json>` output format
- `--dry-run` preview repair candidates without modifying markdown (repair default unless `--apply` is passed)
- `--apply` rewrite derived markdown deterministically
- `--backup` write backups before applying repairs
- `--manifest <FILE>` write a repair manifest, including dry-run previews

Examples:

```bash
aicx corpus audit --root "$HOME/.aicx" --emit json
aicx corpus repair --root "$HOME/.aicx/store/Loctree/aicx/2026_0502" --dry-run --manifest /tmp/aicx-repair-preview.json
aicx corpus repair --root "$HOME/.aicx/store/Loctree/aicx/2026_0502" --apply --backup
```

## `aicx search`

Semantic search across the canonical corpus.

Search uses the materialized semantic index by default. When the embedder or
index is unavailable, it automatically falls back to filesystem-fuzzy and
surfaces the typed semantic failure as `semantic_fallback` in JSON or as a
stderr note in text mode. Use `--no-semantic` only when you intentionally want
to skip the semantic attempt.

```bash
aicx search [OPTIONS] <QUERY>
```

Options:
- `<QUERY>` search query string
- `-p, --project <PROJECT>...` repo or store-bucket filter(s); omit to search all projects
- `-H, --hours <HOURS>` lookback window (`0` = all time)
- `-d, --date <DATE>` filter by date (single day, range, or open-ended)
- `--limit <N>` max results (default: `10`)
- `--score <SCORE>` minimum quality threshold (`0..=100`)
- `--no-semantic` skip semantic search and run filesystem-fuzzy directly
- `-j, --json` emit compact JSON instead of plain text

Examples:

```bash
# Semantic content search across the materialized index
aicx search "auth middleware regression"

# Scoped to several repo/store buckets and date range
aicx search "refactor" -p ai-contexters loctree-suite --date 2026-03-20..2026-03-28

# Explicit fuzzy escape hatch, clearly marked as not semantic oracle truth
aicx search "dashboard" -p ai-contexters --score 60 --no-semantic --json

# Search for a specific day mentioned in query
aicx search "decisions march 2026"
```

## `aicx read`

Read one canonical chunk after a discover step.

Accepts an absolute path, a path relative to `~/.aicx/`, a chunk file name,
or a compact reference in the form
`<project>|<date>|<kind>|<agent>|<session_id>|<chunk>`.

```bash
aicx read [OPTIONS] <REFERENCE>
```

Options:
- `<REFERENCE>` chunk path, file name, or compact reference
- `--max-chars <N>` truncate content to `N` UTF-8 characters
- `-j, --json` emit compact JSON instead of readable text

Examples:

```bash
aicx refs -H 24 --emit paths
aicx read /Users/user/.aicx/store/VetCoders/aicx/2026_0502/reports/codex/2026_0502_codex_sess_001.md
aicx read store/VetCoders/aicx/2026_0502/reports/codex/2026_0502_codex_sess_001.md --max-chars 4000 --json
```

## `aicx steer`

Retrieve chunks by steering metadata. JSON output includes `oracle_status` for
the rebuildable metadata index.

```bash
aicx steer [OPTIONS]
```

Options:
- `--run-id <RUN_ID>` filter by run_id (exact match)
- `--prompt-id <PROMPT_ID>` filter by prompt_id (exact match)
- `--agent <AGENT>` filter by agent: claude, codex, gemini, junie, codescribe
- `-k, --kind <KIND>` filter by kind: conversations, plans, reports, other
- `-p, --project <PROJECT>...` repo or store-bucket filter(s); omit to search all projects
- `-d, --date <DATE>` filter by date: single day, range, or open-ended
- `--limit <N>` max results (default: `10`)
- `-j, --json` emit JSON with `oracle_status`

Examples:

```bash
# All chunks from a specific run
aicx steer --run-id mrbl-001

# Reports across several repo/store buckets on a specific date
aicx steer -p ai-contexters loctree-suite --kind reports --date 2026-03-28

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

## `aicx index`

Build the semantic index used by `aicx search`.

By default, `aicx index` materializes the index for all projects. Use `--dry-run`
only for preview/probe mode. Project filters can be repeated, comma-separated,
or supplied as a space list.

```bash
aicx index
aicx index --dry-run
aicx index -p loctree-suite aicx -p vc-operator
```

Options:
- `-p, --project <PROJECT>...` repo/store-bucket filter(s); omit to index all projects
- `--sample <N>` cap indexed chunks for tests/probes (`0` = all discovered chunks)
- `--dry-run` preview only; omit to materialize
- `-j, --json` emit JSON stats

## Native Embeddings

Native embeddings back semantic indexing/search.

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

List chunks in the canonical store inventory.

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

Extract structured intents and decisions from the canonical store.
JSON output includes `oracle_status` and is canonical corpus evidence, not
semantic oracle output.

```bash
aicx intents [OPTIONS]
```

Options:
- `-p, --project <PROJECT>...` repo or store-bucket filter(s); omit to scan all projects
- `-H, --hours <HOURS>` lookback window (default: `720`)
- `--limit <N>` max results (default: unlimited, so a full roadmap is never silently clipped; an explicit `--limit 10` means 10)
- `--emit <markdown|json>` output format (default: `markdown`; `json` includes `oracle_status`)
- `--strict` only show high-confidence intents
- `--kind <decision|intent|outcome|task>` filter by kind

Example:

```bash
aicx intents -p CodeScribe loctree-suite --strict --kind decision
```

## Truth-pipeline lanes (sessions / claims / results / clarify)

`aicx` separates five epistemic lanes so agent self-report can never become
truth by repetition:

1. **Intents belong to humans** — `aicx intents` (user-only frames by default).
2. **Claims belong to agents** — `aicx claims extract`; always `unverified` at birth.
3. **Results belong to evidence** — `aicx results collect`; no evidence, no result.
4. **Fractures are promise-vs-runtime gaps** — surfaced by `aicx clarify`.
5. **Clarify belongs to unresolved human decisions** — at most 5 A/B/C questions.

Every machine-readable lane export is wrapped in the `aicx.lanes.v1` envelope:
`schema_version`, absolute `generated_at` (RFC3339, full date+year),
`source_time_coverage`, `source_files`, `extraction_mode`, `role_filter`,
`timezone_assumptions`, and `warnings` (non-empty whenever any timestamp is
partial or inferred — partial time is never silently presented as full).

Privacy note: lane exports embed raw text fragments of agent messages — each
`claim_text` is the leading line of the source message verbatim. Paths under
the user's home directory in artifact-evidence excerpts are redacted to `~`.

## `aicx sessions`

Discover and list agent sessions on disk (claude + codex + gemini + junie),
newest first, with absolute RFC3339 timestamps.

```bash
aicx sessions list [--cwd] [--agent <claude|codex|gemini|junie>] [--since YYYY-MM-DD]
                   [--all] [--limit <N>] [--format table|json]
aicx sessions current [--json]
aicx sessions show <session_id> [--format markdown|json]   # alias: aicx session show
aicx sessions report <session_id> [--agent <a>] [--hours <H>] [--repo <path>]
                     [--max <N>] [--format markdown|json]
```

- `current` prints the current agent session id for commit trailers and handoffs.
  It prefers runtime env such as `CODEX_THREAD_ID`, then falls back to the newest
  recent session associated with the current cwd.
- `--cwd` infers the repo from the current directory and lists only its sessions.
- `--agent` accepts exactly `claude`, `codex`, `gemini`, or `junie`; anything
  else is a CLI error, never a silently empty list.
- Table output is the operator copy/paste surface: full `SESSION`, canonical
  repo `PROJECT`, compact source `PATH`, minute-precision `UPDATED (TZ)`,
  combined `MSGS`/user count, and source-path `USR` derived from
  `/Users/<user>/...`.
- Sessions without a parseable timestamp are still listed — the table shows
  them with an explicit `(no timestamp)` marker (and they survive the
  `--since` window) instead of being silently dated out.

`aicx sessions report` is the unified per-session truth surface: all five
lanes in one rendering — classified human-intent lines (Lane 1, strict user
allowlist), agent claims with their evidence-folded verification statuses
(Lanes 2-3), contract fractures (Lane 4), and at most 5 clarify decision
questions (Lane 5) — plus an explicit "fake-complete candidates" list
(contradicted or high-risk-unverified claims). JSON output wraps the payload
in the `aicx.lanes.v1` envelope with `role_filter: all` (the report reads both
user and agent rows; the claim-only lanes stay `agent_only`).

## `aicx claims`

Lane 2: extract agent claims (audit targets, never truth) from one session.

```bash
aicx claims extract --session <id-or-prefix> [--agent <a>] [--hours <H>] [--format json|summary]
```

Every claim carries the absolute source-message timestamp, a
`timestamp_partial` marker, an `extracted_at` stamp, and is born
`unverified`. Applause verdicts (`ready_to_push` / `shippable` /
`no_blockers`) are flagged `high_risk_unverified_claim`.

## `aicx results`

Lane 3: collect repo evidence for a session's claims and fold it into
verification statuses. Read-only — nothing is executed.

```bash
aicx results collect --session <id-or-prefix> [--agent <a>] [--hours <H>]
                     [--repo <path>] [--format json|summary]
```

For every checkable file path a claim names, artifact existence yields a
`pass` result and absence a `fail`; `verify_claims` then promotes/demotes:
pass → `verified`, fail → `contradicted`, mixed → `partial`, no evidence →
stays `unverified` (never promoted).

## `aicx clarify`

Lane 5: turn verified gaps into at most 5 A/B/C **decision** questions —
never fact questions the system can answer itself.

```bash
aicx clarify --session <id-or-prefix> [--agent <a>] [--hours <H>] [--repo <path>]
             [--max <1-5>] [--format markdown|json]
```

Questions come from contract fractures (Lane 4): contradicted claims and
unbacked applause verdicts, severest first, each with a default
recommendation and the cost of not deciding.

## `aicx dashboard`

Generate a searchable HTML dashboard from the canonical store, or serve it locally.

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

Exposes search, read, steer, intents, and rank tools over MCP for agent retrieval.
`aicx_search` is semantic and fails fast when the index is not ready.
`aicx_steer`, `aicx_intents`, and `aicx_rank` query the canonical corpus on disk
and return grounded source paths or chunk references.
`aicx_read` pulls the actual chunk content by path, file name, or compact
reference after a discover step.

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
