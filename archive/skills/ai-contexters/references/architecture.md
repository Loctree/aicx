# ai-contexters — Architecture Reference

## Data Flow

```
Agent Sessions                    ai-contexters                     Outputs
─────────────                    ──────────────                    ───────
~/.claude/projects/  ─┐
~/.codex/history.jsonl ├──▶ Extract ──▶ Normalize ──▶ Dedup ──▶ Redact ──▶ Chunk ──▶ Store
~/.gemini/tmp/       ─┘          │                                             │
                                 │                                             ├──▶ ~/.ai-contexters/ (central)
                                 │                                             ├──▶ local -o (optional)
                                 │                                             ├──▶ stdout --emit (paths/json)
                                 │                                             └──▶ memex (optional)
                                 │
                                 └──▶ extract (one-shot, bypasses store)
```

## Module Map (src/)

| Module | LOC | Purpose | Key Exports |
|--------|-----|---------|-------------|
| `main.rs` | ~350 | CLI entry (clap), command dispatch | `Commands` enum, `main()` |
| `lib.rs` | ~30 | Public library re-exports | All module re-exports |
| `sources.rs` | ~600 | Per-agent extraction logic | `extract_claude()`, `extract_codex()`, `extract_gemini()`, `TimelineEntry`, `ExtractionConfig` |
| `state.rs` | ~250 | Dedup hashes + watermarks + run history | `StateManager`, `RunRecord` |
| `store.rs` | ~300 | Central store layout + indexing | `write_context_chunked()`, `load_index()`, `save_index()` |
| `chunker.rs` | ~400 | Semantic windowing + signal extraction | `Chunk`, `ChunkerConfig`, `chunk_entries()`, `extract_signals()` |
| `output.rs` | ~200 | Local report writing + loctree integration | `write_report()`, `write_markdown_report_to_path()` |
| `memex.rs` | ~150 | rmcp-memex vector sync (shells out) | `sync_new_chunks()`, `MemexConfig`, `MemexSyncState` |
| `redact.rs` | ~100 | Secret redaction (regex engine) | `redact_secrets()` |
| `sanitize.rs` | ~50 | Path traversal prevention | `validate_read_path()`, `validate_write_path()` |
| `init.rs` | ~500 | .ai-context/ bootstrap + agent dispatch | `run_init()`, `InitOptions` |

## Core Data Types

### TimelineEntry (normalized schema)

```rust
struct TimelineEntry {
    timestamp: DateTime<Utc>,
    role: String,           // "user", "assistant", "system"
    agent: String,          // "claude", "codex", "gemini"
    message: String,        // text content (tool calls stripped)
    session_id: String,
    git_branch: Option<String>,
    cwd: Option<String>,
    project: Option<String>,
}
```

All agent-specific formats normalize to this schema during extraction.

### Chunk (output unit)

```rust
struct Chunk {
    id: String,
    project: String,
    agent: String,
    date: NaiveDate,
    entries: Vec<TimelineEntry>,
    signals: Vec<String>,       // extracted highlights
    token_estimate: usize,      // ceil(chars / 4)
}
```

Target: ~1500 tokens per chunk, 2-message overlap between chunks.

### StateManager

```rust
struct StateManager {
    dedup_hashes: HashMap<String, HashSet<u64>>,     // per-project exact hashes
    overlap_hashes: HashMap<String, HashSet<u64>>,   // per-project 60s-bucket hashes
    watermarks: HashMap<String, DateTime<Utc>>,      // per-agent+project last_processed
    run_history: Vec<RunRecord>,                     // audit trail
}
```

Persisted to `~/.ai-contexters/state.json`.

## Input Format Parsing

### Claude Code JSONL

Each line = one message exchange. Fields:
- `message` — the text content
- `timestamp` — ISO 8601
- `type` — `"user"`, `"assistant"`, `"tool_use"`, `"tool_result"`
- `gitBranch`, `cwd`, `sessionId` — metadata
- Only `type == "text"` blocks rendered (tool calls intentionally stripped)

### Codex JSONL

`~/.codex/history.jsonl` — one line per entry:
- `session_id`, `text`, `ts`, `role`, `cwd`
- Per-session grouping during extraction

### Gemini JSON

`~/.gemini/tmp/<hash>/chats/session-*.json` — array format:
- `messages[].type` — `"user"`, `"model"`
- `messages[].content` — text
- `messages[].timestamp`
- `messages[].thoughts[]` — reasoning (if available)

## Signal Extraction Rules

Signals highlight important content in chunks:

| Pattern | Type | Example |
|---------|------|---------|
| `- [ ]`, `- [x]` | TODO | Task checklist items |
| `Decision:` | Decision | Architectural decisions |
| `TODO:`, `FIXME:` | Action | Action items |
| `Plan:` | Plan | Plan descriptions |
| `Ultrathink` | Tag | Extended reasoning blocks |
| `Insight` | Tag | Educational insight blocks |
| `SMOKE TEST PASSED` | Result | Verification outcomes |
| Plan mode markers | Context | Plan mode entry/exit |

Signals appear in `[signals]...[/signals]` blocks at chunk top.

## Deduplication Strategy

**Level 1 — Exact dedup:**
- Hash: `blake3(agent + timestamp_iso + message_first_500_chars)`
- Per-project segregation
- Prevents same message from appearing twice

**Level 2 — Overlap dedup:**
- Hash: `blake3(timestamp_bucket_60s + message_first_200_chars)`
- Cross-agent: catches same prompt sent to Claude AND Codex simultaneously
- 60-second bucket = messages within same minute are overlap candidates

## .ai-context/ Workspace Structure (init)

```
.ai-context/
  share/                          # Git-tracked, shared across team
    artifacts/
      SUMMARY.md                  # Curated, append-only, trimmed to 500 lines
      TIMELINE.md                 # Full append-only timeline
      TRIAGE.md                   # P0/P1/P2 task triage
      prompts/                    # Task prompts in "Emil Kurier" format
  local/                          # Git-ignored, per-developer
    context/                      # Loctree snapshots, raw memories
    prompts/                      # Generated prompts for agent dispatch
    logs/                         # Run logs
    runs/                         # Run metadata
    state/                        # Local state
    config/                       # Local config overrides
    memex/                        # Local memex data
```

`share/` is intended for version control. `local/` is added to `.gitignore`.

## Dependencies

- **clap v4** (derive) — CLI argument parsing
- **chrono** — DateTime handling
- **serde / serde_json** — JSON serialization
- **tracing** — Structured logging
- **regex** — Secret redaction + pattern matching
- **dirs** — Platform-appropriate home directory

No async runtime. Single-threaded synchronous execution.

---

*Created by M&K (c)2026 VetCoders*
