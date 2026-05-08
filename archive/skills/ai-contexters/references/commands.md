# aicx — Complete Command Reference

## Global Flags

| Flag | Default | Description |
|------|---------|-------------|
| `--no-redact-secrets` | off (secrets ARE redacted) | Disable automatic secret redaction |

Redacted patterns: PEM blocks, `Authorization: Bearer`, env vars with `*_API_KEY`/`*_TOKEN`/`*_SECRET`/`*_PASSWORD` suffixes, OpenAI `sk-*`, GitHub `ghp_*`/`github_pat_*`, Slack `xox*`, AWS `AKIA*`, Google `AIza*`.

---

## aicx claude

Extract timelines from Claude Code sessions (`~/.claude/projects/*/*.jsonl`).

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--project <NAME>...` | `-p` | all | Filter by project directory name(s) |
| `--hours <N>` | `-H` | 48 | Lookback window in hours |
| `--output <DIR>` | `-o` | none | Write local report files to directory |
| `--format <md\|json\|both>` | `-f` | both | Local output format |
| `--append-to <FILE>` | | none | Append to single timeline file |
| `--rotate <N>` | | 0 (unlimited) | Keep only last N local files |
| `--incremental` | | off | Skip already-processed entries (watermark) |
| `--user-only` | | off | Exclude assistant + reasoning messages |
| `--loctree` | | off | Include loctree snapshot in output |
| `--project-root <DIR>` | | cwd | Project root for loctree |
| `--memex` | | off | Chunk and sync to memex after extraction |
| `--force` | | off | Ignore dedup hashes (full re-extraction) |
| `--emit <paths\|json\|none>` | | paths | Stdout output mode |

**Example:**
```bash
aicx claude -p CodeScribe -H 72 --incremental --loctree --emit json
```

---

## aicx codex

Extract from Codex history (`~/.codex/history.jsonl`).

Same flags as `claude`. Treats Codex per-session, per-message entries.

**Example:**
```bash
aicx codex -p loctree-plugin -H 24 --incremental
```

---

## aicx all

Extract from all agents (Claude + Codex + Gemini) simultaneously.

Same flags as `claude` except `--format` is hardcoded to `both` for local output.

**Example:**
```bash
aicx all -H 168 --incremental --memex
```

---

## aicx extract

Direct one-shot file extraction. No agent discovery, no store, no dedup.

| Flag | Short | Required | Description |
|------|-------|----------|-------------|
| `--format <claude\|codex\|gemini>` | | yes | Input file format |
| `input` (positional) | | yes | Input file path (JSONL or JSON) |
| `--output <PATH>` | `-o` | yes | Output file (.md or .json, auto-detected) |
| `--user-only` | | no | Exclude assistant messages |
| `--max-message-chars <N>` | | no | Truncate messages (0 = no truncation) |

**Supported inputs:**
- Claude: `*.jsonl` session files, `*.output` task files
- Codex: `history.jsonl`, session JSONL files
- Gemini: `session-*.json` files

**Examples:**
```bash
aicx extract --format claude ~/.claude/projects/proj/uuid.jsonl -o /tmp/report.md
aicx extract --format codex ~/.codex/history.jsonl -o /tmp/codex.md --user-only
aicx extract --format gemini ~/.gemini/tmp/hash/chats/session-1.json -o /tmp/gemini.md
aicx extract --format claude /path/to/huge.jsonl -o /tmp/short.md --max-message-chars 8000
```

---

## aicx store

Store extracted contexts centrally and optionally sync to memex.

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--project <NAME>...` | `-p` | all | Project filter(s) |
| `--agent <AGENT>` | `-a` | all | Agent filter: `claude`, `codex`, `gemini` |
| `--hours <N>` | `-H` | 48 | Lookback window |
| `--user-only` | | off | Exclude assistant messages |
| `--memex` | | off | Chunk and sync to memex after storage |

**Output:** Chunked markdown in `~/.ai-contexters/<project>/<date>/`, paths to stdout.

**Example:**
```bash
aicx store -p CodeScribe --agent claude -H 720 --memex
```

---

## aicx memex-sync

Sync stored chunks from `~/.ai-contexters/memex/chunks/` to rmcp-memex vector memory.

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--namespace <NS>` | `-n` | ai-contexts | Vector namespace |
| `--per-chunk` | | off | Per-chunk upsert instead of batch index |
| `--db-path <PATH>` | | default | Override LanceDB path |

**Requires:** `rmcp-memex` binary in PATH.

**Example:**
```bash
aicx memex-sync --namespace ai-contexts
aicx memex-sync --per-chunk --namespace codescribe-sessions
```

---

## aicx list

Discover available AI agent session sources on this machine. No flags.

**Output:**
```
[claude] ~/.claude/projects (N sessions, X.X MB)
[codex]  ~/.codex (N sessions, X.X MB)
[gemini] ~/.gemini/tmp (N sessions, X.X MB)
```

---

## aicx refs

List stored context files from central store, filtered by recency.

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--hours <N>` | `-H` | 48 | File mtime filter |
| `--project <NAME>` | `-p` | all | Project filter |

**Output:** One file path per line.

**Example:**
```bash
aicx refs -H 72 -p CodeScribe
```

---

## aicx state

Manage dedup hashes, watermarks, and run history (`~/.ai-contexters/state.json`).

| Flag | Short | Description |
|------|-------|-------------|
| `--info` | | Show state statistics |
| `--reset` | | Reset dedup hashes (all or per-project) |
| `--project <NAME>` | `-p` | Scope reset to specific project |

**Examples:**
```bash
aicx state --info
aicx state --reset -p CodeScribe
aicx state --reset    # reset all
```

---

## aicx init

Bootstrap `.ai-context/` workspace and optionally launch agent.

| Flag | Short | Default | Description |
|------|-------|---------|-------------|
| `--project <NAME>` | `-p` | auto-detected | Project name override |
| `--agent <claude\|codex>` | `-a` | interactive | Agent selection |
| `--model <MODEL>` | | agent default | Model override |
| `--hours <N>` | `-H` | 4800 (~200 days) | Context horizon |
| `--max-lines <N>` | | 1200 | Max lines per section |
| `--user-only` | | off | Exclude assistant messages from context |
| `--action <TEXT>` | | none | Action/focus appended to prompt |
| `--agent-prompt <TEXT>` | | none | Additional prompt text (verbatim) |
| `--agent-prompt-file <PATH>` | | none | Load additional prompt from file |
| `--no-run` | | off | Build context/prompt only, don't launch agent |
| `--no-confirm` | | off | Skip interactive confirmation |
| `--no-gitignore` | | off | Don't auto-modify .gitignore |

**Pipeline steps:**
1. Detect git root
2. `loct auto` (indexing)
3. `loct --for-ai` (snapshot)
4. Extract context (memories from sessions)
5. Build composite prompt
6. Dispatch agent (if not `--no-run`)

**Requires:** `loct` in PATH (or `LOCT_BIN` env var).

**Examples:**
```bash
aicx init --agent codex --no-confirm --action "Audit memory and propose next steps"
aicx init --no-run --action "Review auth module"
aicx init --agent claude --agent-prompt-file ./custom-rules.md --no-confirm
aicx init -p CodeScribe --agent codex -H 720 --action "Full refactor plan"
```

---

For storage layout details, see `references/architecture.md`.

---

*Created by M&K (c)2026 VetCoders*
