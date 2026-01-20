# AI Contexters

> Memory extraction tools for AI agent sessions
> Created by M&K (c)2026 VetCoders

## ai-contexters

Extract timeline and decisions from AI agent session files:
- **Claude Code**: `~/.claude/projects/*/*.jsonl`
- **Codex**: `~/.codex/history.jsonl`

### Installation

```bash
cargo install --path .
```

### Usage

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

---

*Created by M&K (c)2026 VetCoders*
