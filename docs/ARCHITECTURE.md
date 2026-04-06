# Architecture

`aicx` is the ledger and control surface for AI agent session history. It:
- reads local agent session logs,
- normalizes them into a single timeline schema,
- deduplicates and chunks the timeline into “agent-readable” context files,
- attaches steering metadata (frontmatter) for selective re-entry by orchestration,
- optionally syncs those chunks into a semantic index (memex) for semantic retrieval,
- exposes those chunks through CLI, MCP, and dashboard search surfaces.

```mermaid
flowchart TD
  CLI[ai-contexters glue: CLI / MCP / dashboard] --> PARSER[aicx-parser: canonical extract/store/read]
  PARSER --> STORE[store.rs: write_context_chunked]
  STORE --> EMIT[stdout: --emit paths/json/none]
  CLI --> LOCAL[output.rs / intents.rs: operator surfaces]
  STORE --> MEMEX[aicx-memex: steer + memex sync/search]
```

## Workspace Package Boundaries

The repository still lives in one shared tree, but ownership is now explicit at
the Cargo-package boundary:

- `crates/aicx-parser`: canonical extraction, chunking, store, steering metadata parsing, ranking, and parser-side heuristics reused above the core.
- `crates/aicx-memex`: steer index, semantic materialization, fast memex search, and the background daemon.
- root `ai-contexters`: glue/orchestration and operator UX (`aicx`, `aicx-mcp`, `aicx-memex`, MCP server, dashboard server, local output, intents, static dashboard generation).

The split is now physical as well as conceptual: parser and memex modules live
under their own crate trees, while the root package only re-exports those
boundaries and hosts the orchestration-specific modules. That means parser work
no longer needs to link the memex stack unless it crosses the indexer boundary
on purpose, and root-only UX edits do not belong in parser-core.

Boundary law:

- Parser side does not link `rmcp-memex`.
- Indexer side may consume parser-side canonical contracts (`store`, `chunker`, `rank`, `sanitize`).
- Glue code composes both packages and should stay thin.

## Module Map (Codebase Mapping)

Parser package (`crates/aicx-parser`, re-exported through `src/lib.rs`):

- `crates/aicx-parser/src/sources.rs`: source discovery + extraction
- `crates/aicx-parser/src/state.rs`: dedup hashes + incremental watermarks
- `crates/aicx-parser/src/store.rs`: central store layout under `~/.aicx/` + `index.json`
- `crates/aicx-parser/src/chunker.rs`: semantic windowing chunker (token heuristic + overlap + highlight extraction)
- `crates/aicx-parser/src/redact.rs`: secret redaction (regex engine)
- `crates/aicx-parser/src/sanitize.rs`: path validation for reads/writes (defense against traversal)
- `crates/aicx-parser/src/frontmatter.rs`: frontmatter parsing for steering metadata
- `crates/aicx-parser/src/rank.rs`: fuzzy ranking and store search helpers
- `crates/aicx-parser/src/types.rs`: shared parser-side contracts and enums

Indexer package (`crates/aicx-memex`, re-exported through `src/lib.rs`):

- `crates/aicx-memex/src/memex.rs`: memex sync/search + runtime truth
- `crates/aicx-memex/src/steer_index.rs`: fast metadata index for steering-aware retrieval
- `crates/aicx-memex/src/daemon.rs`: background refresh/sync control plane

Glue/orchestration package (root `ai-contexters`):

- `src/main.rs`: clap CLI, wires flows together, handles stdout emission (`--emit`).
- `src/mcp.rs`: MCP server surface that composes parser search/rank with memex-backed acceleration when available.
- `src/dashboard_server.rs`: live dashboard/search API bridge over parser + memex packages.
- `src/output.rs`: local report writer (`-o`) + optional loctree snapshot inclusion.
- `src/intents.rs`: operator-facing intention extraction built on parser heuristics.
- `src/dashboard.rs`: static dashboard payload + HTML generation.

## Data Flow: Extractors (`claude`, `codex`, `all`)

High-level sequence (see `src/main.rs::run_extraction`):

1. Parse flags and build an `ExtractionConfig` (`src/sources.rs`).
2. Read session sources and parse events:
   - Claude: `~/.claude/projects/*/*.jsonl`
   - Codex: `~/.codex/history.jsonl`
   - Gemini: `~/.gemini/tmp/<hash>/chats/session-*.json`
   - Gemini Antigravity direct extract: `~/.gemini/antigravity/conversations/<uuid>.pb` or `~/.gemini/antigravity/brain/<uuid>/`
3. Normalize into timeline entries.
4. Deduplicate:
   - exact hash: `(agent, timestamp, message)`
   - overlap hash: `(timestamp_bucket_60s, message)` across agents
5. Redact secrets (default) via `src/redact.rs` unless `--no-redact-secrets`.
6. Store-first chunking:
   - group by `(repo-from-cwd, agent, date)`
   - chunk per group (~1500 tokens, overlap), write canonical `.md` chunks into `~/.aicx/store/` or `~/.aicx/non-repository-contexts/`
7. Stdout emission:
   - `--emit none` prints nothing (default for extractors and `store`)
   - `--emit paths` prints stored chunk paths, one per line
   - `--emit json` prints a single JSON payload including `store_paths`
   - `--emit none` prints nothing
8. Optional local output (`-o`): write a report to the given directory.
9. Optional memex sync (`--memex`): sync the canonical chunks into memex (see note below).

Note on memex sync:
- `--memex` reads from the same canonical chunk + sidecar store that the CLI, MCP, and dashboard use.
- Batch import and per-chunk upsert share the same metadata contract from `.meta.json` sidecars.
- Memex is an add-on semantic index layered on top of the file store — not primary storage.

Framework note:
- Repo-local `.ai-context/` artifacts are now owned by higher-level workflow tooling such as `/vc-init`, not by the retired `aicx init` flow.

## Frontmatter Steering Contract

Report files and chunk sidecars can include frontmatter metadata used for **steering** — targeted retrieval and selective re-entry by orchestration frameworks:

```yaml
---
agent: codex
run_id: mrbl-001
prompt_id: api-redesign_20260327
model: claude-3-5-sonnet
started_at: “2026-03-24T10:00:00Z”
completed_at: “2026-03-24T10:30:00Z”
token_usage: 125000
findings_count: 3
---
```

These fields are parsed by `src/frontmatter.rs`, applied during chunking, and persisted as `.meta.json` sidecars alongside each chunk file. The `steer` command (CLI), `aicx_steer` tool (MCP), and `/api/search/steer` endpoint (dashboard) allow retrieval by these fields without filesystem grep.

Frontmatter is not just telemetry — it is part of the steering and selective re-entry contract. Orchestration can use `run_id` to retrieve all chunks from a specific agent run, `prompt_id` to find outputs from a specific prompt, or combine filters to narrow scope precisely.

## Data Flow: `store`

`store` is the “centralize older history into the store” command (see `src/main.rs::run_store`):

1. Extract selected agents + projects for a lookback window.
2. Redact secrets (default).
3. Chunk and write into the canonical `~/.aicx/` store.
4. Optional memex sync (`--memex`).

## MCP Surface (`src/mcp.rs`)

The MCP server exposes three tools via stdio and streamable HTTP transports:

- `aicx_search` — fuzzy text search across stored chunks with quality scoring; returns compact JSON using the same rich fields as CLI `aicx search --json`
- `aicx_rank` — rank chunks by signal density for a project as compact JSON
- `aicx_steer` — retrieve chunks by steering metadata (run_id, prompt_id, agent, kind, project, date) using sidecar data; the primary metadata-aware retrieval path for orchestration

Recency filtering in `aicx_search` and `aicx_steer` uses canonical chunk dates from the store layout, not filesystem `mtime` accidents.

## Security Model (Pragmatic)

Two mechanisms protect your machine and your data:
- Path validation (read/write) in `src/sanitize.rs`.
- Best-effort secret redaction in `src/redact.rs` (enabled by default).

Redaction is conservative by design: it’s OK to over-redact sometimes; it’s not OK to leak tokens into committed artifacts.
