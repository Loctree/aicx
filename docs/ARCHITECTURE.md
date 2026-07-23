# Architecture

`aicx` is the operator front door for agent session logs. It is a store-first
system with retrieval surfaces layered on top:

1. **Canonical corpus** (layer 1, `~/.aicx/`): read local agent session logs,
   normalize into a single timeline schema, deduplicate, chunk into steerable
   markdown with frontmatter metadata. This is ground truth.
2. **Retrieval surfaces**: filesystem search, steering metadata, MCP tools, and
   the reusable native embedding library in `crates/aicx-embeddings`.

`aicx` owns the canonical corpus and portable local embedding foundation.
Roost/rust-memex owns the advanced retrieval/operator plane.
See `docs/ORACLE_CORPUS.md` for the operator contract: raw/canonical corpus is
truth; indexes are derived, rebuildable views that must disclose fallback,
freshness, and Loctree scope safety.

The pipeline exposes chunks through CLI, MCP, dashboard search surfaces, and an
adjacent Vibecrafted artifact explorer for workflow/marbles reports.

```mermaid
flowchart TD
  CLI[aicx CLI] --> SRC[sources.rs: extract_*]
  SRC --> DEDUP[state.rs: dedup + watermark]
  DEDUP --> RED[redact.rs: redact_secrets]
  RED --> STORE[store.rs: write_context_chunked]
  STORE --> EMIT[stdout: --emit paths/json/none]
  RED --> LOCAL[output.rs: write_report (-o)]
  STORE --> STEER[steer_index.rs: sync_steer_index]
  STORE --> MCP[mcp.rs: search/rank/steer tools]
```

## Module Map (Codebase Mapping)

Library modules (see `src/lib.rs`). Parser-owned entries live under
`crates/aicx-parser`; the old root-crate module locations are retired.

- `src/extraction/`: source discovery + multi-agent extraction orchestration
- `src/sessions.rs` + `src/session_catalog.rs`: bounded session discovery (locate-before-parse)
- `src/state/`: dedup hashes + incremental watermarks
- `src/store.rs` (+ `src/store/*`): canonical store under `$AICX_HOME`, projection stages, identity migration, atomic writes
- `src/vector_index.rs`: committed semantic index + hybrid generation materialization (one dense payload per generation)
- `src/search_engine.rs`: hybrid/semantic search dispatch with typed `RetrievalOutcome` status ownership
- `src/oracle.rs`: CLI/MCP/evidence oracle status derivation from retrieval evidence (never invents healthy)
- `src/mcp.rs`: MCP stdio/HTTP tools (`aicx_search`, `aicx_read`, `aicx_rank`, `aicx_steer`, `aicx_intents`, `aicx_index_status`)
- `src/intents/`: structured intent/decision/outcome/task extraction
- `src/doctor/`: bounded fast `health` (8192 fs-ops budget) vs deep forensic doctor
- `src/cli_config.rs`: `aicx config show|init|inspect` — runtime inspection schema `aicx.runtime_inspection.v1`
- `src/overlay.rs`: overlay-intent-v1 join against Loctree anchors (exact project identity, fail-closed)
- `crates/aicx-parser`: deterministic session engine + five agent adapters; TB is differential oracle only
- `crates/aicx-retrieve`: exact mmap dense adapter, fusion, manifests
- `crates/aicx-embeddings`: reusable local GGUF embedding provider library
- `src/redact.rs` + `crates/aicx-parser/src/sanitize.rs`: secret redaction + path allowlisting

Binary orchestration:
- `src/main.rs` / `src/bin/*`: clap CLI + `aicx-mcp`, wires flows, handles stdout emission (`--emit`).

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
5. On corpus-building commands, redact secrets by default via `src/redact.rs`
   unless `--no-redact-secrets`.
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
9. Steer index refresh: sidecar metadata is available to CLI/MCP steering retrieval.

Note on heavy retrieval:
- AICX no longer exposes a `memex-sync` CLI command.
- Roost/rust-memex remains the advanced retrieval plane and can consume the
  canonical store externally.
- AICX native embeddings are exposed as a reusable library, not as an automatic
  background indexing daemon.

Dashboard cross-search still has one legacy external boundary:
`/api/search/cross` shells out to the resolved absolute `rust-memex` or
`rmcp-memex` binary. The spawn environment is intentionally cleared, then only
`HOME`, `XDG_CONFIG_HOME`, and `XDG_DATA_HOME` are passed through for config-dir
resolution. `PATH` is never passed; the binary must be resolved before
`env_clear()` so a user-controlled `PATH` cannot change what gets executed.
If the rust-memex CLI becomes fully self-contained, these variables may be absent and
the child process must still fail with a normal command error, not by inheriting
the parent environment.

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
4. Refresh sidecar/steering metadata surfaces.

## MCP Surface (`src/mcp.rs`)

The MCP server exposes six tools via stdio and streamable HTTP transports:

- `aicx_search` — semantic-first search over the canonical corpus. Ready indexes return `hybrid_rrf` oracle status and are safe for Loctree scope narrowing. Missing semantic preconditions degrade to filesystem fuzzy with an explicit `semantic_fallback` payload; callers that need fail-fast semantics pass `strict_semantic = true`.
- `aicx_read` — read one canonical chunk by path, file name, or compact reference; this is the direct re-entry step after search, refs, steer, or dashboard discovery.
- `aicx_rank` — rank chunks by signal density for a project as compact JSON.
- `aicx_steer` — retrieve chunks by steering metadata (run_id, prompt_id, agent, kind, project, date) using sidecar data; returns `oracle_status` for the rebuildable metadata index and is safe for Loctree metadata narrowing only when source paths verify.
- `aicx_intents` — extract intent/outcome/decision/task records from canonical chunks.
- `aicx_index_status` — report the sessions -> chunks -> semantic-index pipeline for a project bucket, including readiness, backend, row count, and artifact paths.

CLI and MCP share one project-identity resolver (exact by default, fail-closed on
ambiguous bare names) and one retrieval-status owner (`RetrievalOutcome` →
`search_oracle_status_from_retrieval`). `aicx config inspect --json` reports
install-channel drift, resolved `AICX_HOME`, embedder identity (secrets
redacted), and `_all` hybrid generation without mutating anything.

Recency filtering in `aicx_search` and `aicx_steer` uses canonical chunk dates from the store layout, not filesystem `mtime` accidents.

The streamable HTTP transport binds to `127.0.0.1` by default and keeps rmcp's
loopback-only `Host` validation. Operators can explicitly pass `--host <IP>` to
listen on another interface and `--allowed-host <HOST>` for each remote
hostname/IP clients will use. `--allow-any-host` disables that DNS-rebinding
guard and is intended only for trusted networks.

## Security Model (Pragmatic)

Two mechanisms protect your machine and your data:
- Path validation (read/write) in `crates/aicx-parser/src/sanitize.rs`.
- Best-effort secret redaction in `src/redact.rs` (enabled by default).

Redaction is conservative by design: it’s OK to over-redact sometimes; it’s not OK to leak tokens into committed artifacts. The flag lives only on corpus-building commands that create or rewrite artifacts, not on read-only search and steering surfaces.
