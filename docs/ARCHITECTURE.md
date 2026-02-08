# Architecture

`ai-contexters` is a single Rust CLI that:
- reads local agent session logs,
- normalizes them into a single timeline schema,
- deduplicates and chunks the timeline into “agent-readable” context files,
- optionally syncs those chunks into a vector store (memex),
- optionally bootstraps a repo-local `.ai-context/` workspace for multi-agent workflows.

```mermaid
flowchart TD
  CLI[ai-contexters CLI] --> SRC[sources.rs: extract_*]
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
- `src/store.rs`: central store layout under `~/.ai-contexters/` + `index.json`
- `src/chunker.rs`: semantic windowing chunker (token heuristic + overlap + highlight extraction)
- `src/output.rs`: local report writer (`-o`) + optional loctree snapshot inclusion
- `src/memex.rs`: memex sync (`rmcp-memex index/upsert`) + sync state
- `src/redact.rs`: secret redaction (regex engine)
- `src/sanitize.rs`: path validation for reads/writes (defense against traversal)
- `src/init.rs`: `.ai-context/` bootstrap + agent dispatch

Binary orchestration:
- `src/main.rs`: clap CLI, wires flows together, handles stdout emission (`--emit`).

## Data Flow: Extractors (`claude`, `codex`, `all`)

High-level sequence (see `src/main.rs::run_extraction`):

1. Parse flags and build an `ExtractionConfig` (`src/sources.rs`).
2. Read session sources and parse events:
   - Claude: `~/.claude/projects/*/*.jsonl`
   - Codex: `~/.codex/history.jsonl`
   - Gemini: `~/.gemini/tmp/<hash>/chats/session-*.json`
3. Normalize into timeline entries.
4. Deduplicate:
   - exact hash: `(agent, timestamp, message)`
   - overlap hash: `(timestamp_bucket_60s, message)` across agents
5. Redact secrets (default) via `src/redact.rs` unless `--no-redact-secrets`.
6. Store-first chunking:
   - group by `(repo-from-cwd, agent, date)`
   - chunk per group (~1500 tokens, overlap), write `.md` chunks into `~/.ai-contexters/`
7. Stdout emission:
   - `--emit paths` prints stored chunk paths, one per line
   - `--emit json` prints a single JSON payload including `store_paths`
   - `--emit none` prints nothing
8. Optional local output (`-o`): write a report to the given directory.
9. Optional memex sync (`--memex`): chunk again and push into memex (see note below).

Note on memex sync:
- `--memex` in extractors currently creates chunk files in `~/.ai-contexters/memex/chunks/` and then calls memex sync.
- These are separate from the “store-first” chunks. This is intentional separation: store chunks are for humans/agents to read; memex chunks are for vector indexing.

## Data Flow: `store`

`store` is the “centralize older history into the store” command (see `src/main.rs::run_store`):

1. Extract selected agents + projects for a lookback window.
2. Redact secrets (default).
3. Chunk and write into `~/.ai-contexters/`.
4. Optional memex sync (`--memex`).

## Data Flow: `init`

`init` creates `.ai-context/` in the current repo and optionally runs an agent (see `src/init.rs`):

1. Detect repo root (git root).
2. Build local context:
   - extracted memories (via ai-contexters store)
   - loctree snapshot (requires `loct` in `PATH` or `LOCT_BIN`)
3. Write `.ai-context/share/artifacts/*`:
   - `SUMMARY.md` (curated append-only)
   - `TIMELINE.md` (full append-only)
   - `TRIAGE.md` (P0/P1/P2)
   - `prompts/` (task prompts in “Emil Kurier” format)
4. Optionally dispatch an agent run:
   - Terminal mode (macOS) or subprocess mode, depending on environment.

## Security Model (Pragmatic)

Two mechanisms protect your machine and your data:
- Path validation (read/write) in `src/sanitize.rs`.
- Best-effort secret redaction in `src/redact.rs` (enabled by default).

Redaction is conservative by design: it’s OK to over-redact sometimes; it’s not OK to leak tokens into committed artifacts.
