---
name: ai-contexters
version: "1.0"
description: >-
  This skill should be used when the user asks to "extract context",
  "recover session", "get timeline", "bootstrap agent context",
  "init ai-context", "sync to memex", "list sessions", "find stored contexts",
  "check dedup state", "extract from claude", "extract from codex",
  "extract from gemini", "run aicx", "get context from previous session",
  "what was I working on", "resume previous session",
  or needs to recover plans, decisions, and work history from AI agent sessions
  (Claude Code, Codex, Gemini CLI). Covers the full aicx CLI:
  extraction, storage, vector sync, state management, and repo bootstrapping.
---

# ai-contexters — Context Ops Pipeline for AI Agent Sessions

A Rust CLI that extracts, deduplicates, stores, and syncs timelines from
Claude Code, Codex, and Gemini CLI sessions. Turns raw JSONL/JSON logs into
clean, agent-readable markdown chunks with signal extraction.

**Binary location**: `aicx` (in PATH via cargo install)
**Central store**: `~/.ai-contexters/`
**Repo workspace**: `.ai-context/` (created by `init`)

## Core Workflows

### 1. Daily Extraction (most common)

Extract from all agents, incremental, store centrally:

```bash
aicx all -H 24 --incremental
```

Extract from specific project, last 7 days:

```bash
aicx claude -p CodeScribe -H 168 --incremental
```

Extract and sync to vector memory:

```bash
aicx all -H 48 --incremental --memex
```

### 2. Session Recovery (before resuming work)

Find the session file and extract a readable report:

```bash
aicx extract --format claude ~/.claude/projects/<proj>/<uuid>.jsonl -o /tmp/report.md
```

For Codex sessions:

```bash
aicx extract --format codex ~/.codex/history.jsonl -o /tmp/codex.md
```

For Gemini sessions:

```bash
aicx extract --format gemini ~/.gemini/tmp/<hash>/chats/session-*.json -o /tmp/gemini.md
```

### 3. Repo Bootstrap (init)

Bootstrap `.ai-context/` workspace with full context and optionally launch agent:

```bash
aicx init --agent codex --no-confirm --action "Audit memory and propose next steps"
```

Build context only (no agent launch):

```bash
aicx init --no-run --action "Review and refactor auth module"
```

### 4. Reference Lookup

Find stored context files from last 3 days:

```bash
aicx refs -H 72
```

Filter by project:

```bash
aicx refs -H 168 -p CodeScribe
```

### 5. State Management

Check dedup statistics:

```bash
aicx state --info
```

Reset dedup for fresh re-extraction:

```bash
aicx state --reset -p CodeScribe
```

## Command Reference (Quick)

| Command | Purpose | Key Flags |
|---------|---------|-----------|
| `all` | Extract from all agents | `-H`, `--incremental`, `--memex`, `-p` |
| `claude` | Extract Claude Code sessions | `-H`, `-p`, `--incremental`, `--loctree` |
| `codex` | Extract Codex sessions | `-H`, `-p`, `--incremental` |
| `extract` | One-shot file extraction | `--format <claude\|codex\|gemini>`, `-o` |
| `store` | Store + optional memex sync | `-p`, `-a`, `-H`, `--memex` |
| `memex-sync` | Sync chunks to vector memory | `-n`, `--per-chunk`, `--db-path` |
| `list` | Discover available sessions | (no flags) |
| `refs` | List stored context files | `-H`, `-p` |
| `state` | Manage dedup/watermarks | `--info`, `--reset`, `-p` |
| `init` | Bootstrap .ai-context/ workspace | `--agent`, `--action`, `--no-run` |

For detailed flag reference, consult `references/commands.md`.

## Output Modes

Control stdout output with `--emit`:
- **paths** (default): One file path per line, pipe-friendly
- **json**: Structured JSON with metadata, entries, store paths
- **none**: Silent (store only)

Local output with `-o <DIR>`:
- `-f md` — Markdown report
- `-f json` — JSON report
- `-f both` — Both formats (default for `all`)

## Key Concepts

### Store-First Architecture
Every extraction writes to `~/.ai-contexters/<project>/<date>/` before optional
local output. Central history builds automatically.

### Incremental Processing
`--incremental` tracks watermarks per agent+project. Re-runs skip already-processed
entries. Essential for cron / scheduled workflows.

### Deduplication (Two-Level)
- **Exact**: `(agent, timestamp, message)` hash
- **Overlap**: `(timestamp_bucket_60s, message)` across agents — catches same prompt
  sent to multiple agents simultaneously

### Signal Extraction
Chunks include `[signals]...[/signals]` blocks highlighting:
- TODO items (`- [ ]`, `- [x]`)
- Intent/result lines
- Tag vicinity: `Ultrathink`, `Insight`, plan mode markers
- Keywords: `Decision:`, `TODO:`, `Plan:`

### Secret Redaction
Enabled by default. Redacts API keys, tokens, PEM blocks, auth headers.
Disable with `--no-redact-secrets` (not recommended).

## Init Command Deep Dive

`init` is the most complex command — bootstraps a full agent workspace:

1. Detect repo root (git root)
2. Run `loct auto` + `loct --for-ai` (codebase snapshot)
3. Extract context from stored sessions (memories)
4. Build composite prompt (context + loctree + action + agent-prompt)
5. Optionally dispatch agent (Terminal.app on macOS, subprocess on Linux)

**Created structure:** `.ai-context/` with `share/artifacts/` (git-tracked) and `local/` (git-ignored). See `references/architecture.md` for full layout.

**Custom prompting:**
```bash
aicx init --agent codex --agent-prompt "Focus only on Rust modules" --action "Refactor VAD"
aicx init --agent claude --agent-prompt-file ./my-prompt.md --no-confirm
```

**Requires**: `loct` in PATH (or `LOCT_BIN` env var).

## Environment Variables

| Variable | Purpose |
|----------|---------|
| `LOCT_BIN` | Override path to `loct` binary (used by `init`) |

## Common Patterns

### Pipe to downstream tools

```bash
# Feed stored paths to another agent
aicx claude -p CodeScribe -H 24 --emit paths | xargs cat > /tmp/full-context.md

# JSON for automation
aicx all -H 48 --emit json | jq '.store_paths[]'
```

### Cron job (daily extraction)

```bash
# In crontab: extract all, incremental, sync to memex
0 9 * * * aicx all -H 24 --incremental --memex 2>> ~/.ai-contexters/cron.log
```

### Trim large sessions

```bash
aicx extract --format claude /path/to/huge.jsonl -o /tmp/report.md --max-message-chars 8000
aicx extract --format claude /path/to/huge.jsonl -o /tmp/report.md --user-only
```

## Additional Resources

### Reference Files

For detailed command flags and architecture:
- **`references/commands.md`** — Complete flag reference for all 11 subcommands
- **`references/architecture.md`** — Module map, data flow, storage layout

---

*Created by M&K (c)2026 VetCoders*
