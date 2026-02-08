# AI Contexters

> Memory extraction tools for AI agent sessions
> Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

## ai-contexters

Extract timeline and decisions from AI agent session files:
- **Claude Code**: `~/.claude/projects/*/*.jsonl`
- **Codex**: `~/.codex/history.jsonl`
- **Gemini CLI**: `~/.gemini/tmp/<hash>/chats/session-*.json`

### Installation

```bash
cargo install --path .
```

### Quickstart (init)

```bash
# Interactive init (creates .ai-context and runs an agent)
ai-contexters init

# Non-interactive agent selection (skip confirmation)
ai-contexters init --agent codex --no-confirm

# Build context/prompt only (no agent run)
ai-contexters init --no-run

# Provide a focused action for the agent
ai-contexters init --no-confirm --action "Fix the login flow regressions"
```

`init` creates a single per-repo folder: `.ai-context/`

```
.ai-context/
  share/
    artifacts/
      SUMMARY.md    # curated, append-only summary (trimmed to 500 lines)
      TIMELINE.md   # full append-only timeline
      TRIAGE.md     # unfinished implementations + P0/P1/P2
      prompts/      # task prompts ("Emil Kurier" format)
  local/
    context/        # loct + extracted memories
    prompts/        # built prompts
    logs/
    runs/
    state/
    memex/
    config/
```

Only `share/artifacts/SUMMARY.md` and `share/artifacts/TIMELINE.md` are meant to be committed by default.  
`TRIAGE.md` and `prompts/` are optional to share.

### Usage (classic extractors)

```bash
# List all projects/sessions
ai-contexters list

# Extract Claude Code sessions (last 48h)
ai-contexters claude -p CodeScribe -H 48 -o ./reports

# Extract Codex history (last 48h)
ai-contexters codex -p codescribe -H 48 -o ./reports

# Extract all agents
ai-contexters all -p codescribe -H 168 -o ./reports  # Last 7 days
```

### Integration-friendly stdout

By default, extractors print **store chunk paths** to stdout (one per line).

If you want a single JSON payload on stdout (for piping into other tools), use:

```bash
ai-contexters codex -p codescribe -H 48 --emit json | jq .
```

To write to the store but print nothing to stdout:

```bash
ai-contexters all -p codescribe -H 48 --emit none
```

### Output Formats

- **Markdown** (`*_memory_*.md`): Human-readable timeline with emoji badges
- **JSON** (`*_memory_*.json`): Queryable format for automation

### Example Output

```markdown
## 2026-01-17

### 03:18:01 👤 [Codex] `019bc9f5`

> # Plan: Unix Socket IPC Architecture
> - ✅ Jeden tray (CLI)
> - ✅ GUI jako thin client
```

### Filtering

- `-p <project>`: Filter by project name (case-insensitive substring match)
- `-H <hours>`: Look back period (default: 48)
- `-o <dir>`: Output directory (default: current)
- `-f <format>`: Output format: `md`, `json`, or `both` (default: both)

### Notes

- `init` requires `loct` available in PATH (or set `LOCT_BIN` to a full path).
- Claude streaming output uses `jq` (and `awk` for passthrough).
- `--model` is optional; if omitted, the agent uses its default model.

---

*Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders*
