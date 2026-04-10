# Architecture

`aicx` is the operator front door for agent session logs. It orchestrates a
two-layer pipeline — canonical corpus first, optional semantic index second:

1. **Canonical corpus** (layer 1, `~/.aicx/`): read local agent session logs,
   normalize into a single timeline schema, deduplicate, chunk into steerable
   markdown with frontmatter metadata. This is ground truth.
2. **Optional semantic index** (layer 2, memex): embed the canonical corpus into
   a vector + BM25 index for semantic retrieval by agents and MCP tools. Always
   operator-driven — nothing syncs automatically.

`aicx` owns the canonical corpus; memex is an optional semantic index layered on top.

The pipeline exposes chunks through CLI, MCP, and dashboard search surfaces.

```mermaid
flowchart TD
  CLI[aicx CLI] --> SRC[sources.rs: extract_*]
  SRC --> DEDUP[state.rs: dedup + watermark]
  DEDUP --> RED[redact.rs: redact_secrets]
  RED --> STORE[store.rs: write_context_chunked]
  STORE --> EMIT[stdout: --emit paths/json/none]
  RED --> LOCAL[output.rs: write_report (-o)]
  STORE --> MEMEX[memex.rs: sync_new_chunks (--memex)]
```

## Module Map (Codebase Mapping)

Library modules (see `src/lib.rs`):

- `src/sources.rs`: source discovery + extraction
- `src/state.rs`: dedup hashes + incremental watermarks
- `src/store.rs`: canonical store layout under `~/.aicx/` + `index.json`
- `src/chunker.rs`: semantic windowing chunker (token heuristic + overlap + highlight extraction)
- `src/output.rs`: local report writer (`-o`) + optional loctree snapshot inclusion
- `src/memex.rs`: memex materialization (in-process via `rmcp-memex` library) + sync state
- `src/redact.rs`: secret redaction (regex engine)
- `src/sanitize.rs`: path validation for reads/writes (defense against traversal)
- `src/steer_index.rs`: fast metadata index for steering-aware retrieval

Binary orchestration:
- `src/main.rs`: clap CLI, wires flows together, handles stdout emission (`--emit`).

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
   - use the source-side `--project` filter only to narrow session discovery
   - then group the surviving entries by resolved repo identity `(repo-from-cwd, agent, date)`
   - chunk per group (~1500 tokens, overlap), write canonical `.md` chunks into `~/.aicx/store/` or `~/.aicx/non-repository-contexts/`
7. Stdout emission:
   - `--emit none` prints nothing (default for extractors and `store`)
   - `--emit paths` prints stored chunk paths, one per line
   - `--emit json` prints a single JSON payload including `store_paths`, `requested_source_filters`, and `resolved_store_buckets`
   - `--emit none` prints nothing
8. Optional local output (`-o`): write a report to the given directory.
9. Optional memex materialization (`--memex`): materialize canonical chunks into the optional memex semantic index (see note below).

Note on memex materialization:
- `--memex` reads from the same canonical chunk + sidecar store that the CLI, MCP, and dashboard use.
- Batch import and per-chunk upsert share the same metadata contract from `.meta.json` sidecars.
- Memex is an optional semantic index layered on top of the canonical store — not primary storage. Nothing materializes automatically.

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

`store` is the “build the canonical corpus from older history” command (see `src/main.rs::run_store`):

1. Extract selected agents + source filters for a lookback window.
2. Redact secrets (default).
3. Chunk and write into the canonical `~/.aicx/` store, which may resolve into multiple repo buckets plus `non-repository-contexts`.
4. Optional memex sync (`--memex`).

## MCP Surface (`src/mcp.rs`)

The MCP server exposes three tools via stdio and streamable HTTP transports:

- `aicx_search` — search stored chunks with quality scoring; widens with memex semantic retrieval when available and otherwise falls back to canonical-store fuzzy search
- `aicx_rank` — rank chunks by signal density for a project as compact JSON
- `aicx_steer` — retrieve chunks by steering metadata (run_id, prompt_id, agent, kind, project, date) using sidecar data; the primary metadata-aware retrieval path for orchestration

Recency filtering in `aicx_search` and `aicx_steer` uses canonical chunk dates from the store layout, not filesystem `mtime` accidents.

## Security Model (Pragmatic)

Two mechanisms protect your machine and your data:
- Path validation (read/write) in `src/sanitize.rs`.
- Best-effort secret redaction in `src/redact.rs` (enabled by default).

Redaction is conservative by design: it’s OK to over-redact sometimes; it’s not OK to leak tokens into committed artifacts.
