# AI Contexters

Memory extraction + context distillation for AI agent sessions.

`aicx` turns local agent logs into:
- a clean, deduped timeline,
- chunked “agent-readable” context stored in `~/.ai-contexters/`,
- optional `.ai-context/` artifacts for repo-level “bring new agent up to speed” workflows,
- optional sync into memex (vector memory).

Supported sources:
- Claude Code: `~/.claude/projects/*/*.jsonl`
- Codex: `~/.codex/history.jsonl`
- Gemini CLI: `~/.gemini/tmp/<hash>/chats/session-*.json`

## Install

```bash
cargo install --path .
```

## Quickstart

Extract everything from the last 4 hours, store-first, and print the stored chunk paths:

```bash
aicx all -H 4
```

Pipe one JSON payload (handy for automation):

```bash
aicx all -H 4 --emit json | jq .
```

Bootstrap a repo context (`.ai-context/`) and run an agent:

```bash
aicx init --agent codex --no-confirm --action "Map the repo and propose next steps"
```

## What Gets Written Where

Central store (always, for extractors):
- `~/.ai-contexters/<project>/<date>/<time>_<agent>-<seq>.md`
- `~/.ai-contexters/index.json`
- `~/.ai-contexters/memex/chunks/` (when memex is used)

Repo-local init artifacts:
- `.ai-context/share/artifacts/SUMMARY.md`
- `.ai-context/share/artifacts/TIMELINE.md`
- `.ai-context/share/artifacts/TRIAGE.md`
- `.ai-context/share/artifacts/prompts/`

## Common Workflows

Daily “what changed?” (clean stdout, just store paths):

```bash
aicx claude -p CodeScribe -H 24 --emit paths
```

Incremental mode (watermark per source, avoids re-processing):

```bash
aicx all -H 168 --incremental
```

User-only mode (smaller output; excludes assistant + reasoning):

```bash
aicx claude -p CodeScribe -H 48 --user-only
```

Memex sync (vector memory):

```bash
aicx all -H 48 --memex
aicx memex-sync --namespace ai-contexts
```

## Docs

- `docs/ARCHITECTURE.md` (module map + data flows)
- `docs/COMMANDS.md` (exact CLI reference + examples)
- `docs/STORE_LAYOUT.md` (store + `.ai-context/` layouts)
- `docs/REDACTION.md` (secret redaction, regex engine notes)
- `docs/DISTILLATION.md` (chunking/distillation model + tuning ideas)

## Notes

- Secrets are redacted by default. Disable only if you know what you’re doing: `--no-redact-secrets`.
- `init` expects `loct` in `PATH` (or `LOCT_BIN=/full/path/to/loct`).

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
