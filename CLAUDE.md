# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Build Commands

```bash
# Build release binary
cargo build --release

# Install locally
cargo install --path .

# Run directly
cargo run -- <subcommand>

# Check/lint (use before committing)
cargo check
cargo clippy --all-features --all-targets -- -D warnings

# Format
cargo fmt
```

## Architecture

Single-binary Rust CLI (`aicx`) that extracts timeline data from AI agent session files.

**Supported agents:**
- Claude Code: `~/.claude/projects/*/*.jsonl`
- Codex: `~/.codex/history.jsonl`

**Core flow:**
1. CLI parsing via clap (`Commands` enum)
2. Agent-specific extraction (`extract_claude`, `extract_codex`)
3. JSONL parsing into `TimelineEntry` structs
4. Output generation (Markdown + JSON)

**Key structures in `src/main.rs`:**
- `TimelineEntry` - unified format for both agents
- `ClaudeEntry` / `CodexEntry` - raw JSONL schemas
- `Report` - output container with metadata

## CLI Usage

```bash
aicx init                           # Interactive init (creates .ai-context and runs an agent)
aicx init --agent codex --no-confirm # Non-interactive agent selection
aicx init --no-run                  # Build context/prompt only
aicx init --no-confirm --action "Fix the login flow regressions"

aicx list                           # List available sessions
aicx claude -p <project> -H 48      # Extract Claude sessions (last 48h)
aicx codex -p <project> -H 48       # Extract Codex history
aicx all -p <project> -H 168        # Extract all (7 days)

# Integration: emit one JSON payload to stdout
aicx codex -p <project> -H 48 --emit json | jq .
```

Flags: `-p` project filter, `-H` hours back, `-o` output dir, `-f` format (md/json/both)

## Init artifacts

`aicx init` creates `.ai-context/` in repo root.

```
.ai-context/
  share/
    artifacts/
      SUMMARY.md    # curated, append-only summary (trimmed to 500 lines)
      TIMELINE.md   # full append-only timeline
      TRIAGE.md     # unfinished implementations + P0/P1/P2
      prompts/      # task prompts ("Emil Kurier" format)
  local/
    context/
    prompts/
    logs/
    runs/
    state/
    memex/
    config/
```

Only `share/artifacts/SUMMARY.md` and `share/artifacts/TIMELINE.md` are intended to be committed by default.
`TRIAGE.md` and `prompts/` are optional to share.

---

Notes:
- `init` requires `loct` available in PATH (or set `LOCT_BIN` to a full path).
- Claude streaming output uses `jq` (and `awk` for passthrough).

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
