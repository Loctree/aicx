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

Supported install paths today are source-based.

From a local checkout:

```bash
./install.sh
```

`install.sh` installs `aicx` + `aicx-mcp` from the current checkout and configures Claude Code, Codex, and Gemini when their MCP settings directories already exist.

From an accessible GitHub repo:

```bash
cargo install --git https://github.com/VetCoders/ai-contexters --locked ai-contexters
```

Already installed the binaries?

```bash
./install.sh --skip-install
```

Manual fallback:

```bash
cargo install --path . --locked --bin aicx --bin aicx-mcp
./install.sh --skip-install
```

Public `crates.io` distribution is not live yet.

## Quickstart

Store recent context from the last 4 hours. Extractor/store commands are quiet on stdout by default (`--emit none`), but still write chunked context into `~/.ai-contexters/`.

```bash
aicx all -H 4 --incremental
```

See what landed in the store:

```bash
aicx refs -H 4
aicx refs -H 4 --emit paths
```

Pipe one JSON payload (handy for automation):

```bash
aicx all -H 4 --emit json | jq '.store_paths'
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

Daily “what changed?” with incremental refresh plus compact summary:

```bash
aicx all -H 24 --incremental --emit none
aicx refs -H 24
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
- `docs/RELEASES.md` (release/distribution workflow + maintainer checklist)

## Notes

- Secrets are redacted by default. Disable only if you know what you’re doing: `--no-redact-secrets`.
- `init` expects `loct` in `PATH` (or `LOCT_BIN=/full/path/to/loct`).

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
