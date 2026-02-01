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

Single-binary Rust CLI (`ai-contexters`) that extracts timeline data from AI agent session files.

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
ai-contexters init                           # Interactive init (creates .ai-context and runs an agent)
ai-contexters init --agent codex --no-confirm # Non-interactive agent selection
ai-contexters init --no-run                  # Build context/prompt only

ai-contexters list                           # List available sessions
ai-contexters claude -p <project> -H 48      # Extract Claude sessions (last 48h)
ai-contexters codex -p <project> -H 48       # Extract Codex history
ai-contexters all -p <project> -H 168        # Extract all (7 days)
```

Flags: `-p` project filter, `-H` hours back, `-o` output dir, `-f` format (md/json/both)

## Init artifacts

`ai-contexters init` creates `.ai-context/` in repo root.

```
.ai-context/
  share/
    summary.md      # curated, append-only summary (trimmed to 500 lines)
    timeline.md     # full append-only timeline
  local/
    context/
    prompts/
    logs/
    runs/
    state/
    memex/
    config/
```

Only `share/summary.md` and `share/timeline.md` are intended to be committed.

---

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
