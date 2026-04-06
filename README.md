# AI Contexters

Operator front door for agent session history.

`aicx` orchestrates a two-layer pipeline:

1. **Canonical corpus** (`~/.aicx/`) — extract, deduplicate, chunk, and store
   agent session logs as steerable markdown with frontmatter metadata.
   This is ground truth. Built by extractors (`claude`, `codex`, `all`) and `store`.

2. **Semantic materialization** (memex) — embed the canonical corpus into a
   vector + BM25 index for retrieval by agents and MCP tools.
   Built by `memex-sync`, or the `--memex` shortcut on any extractor.

`aicx` is the operator; memex is the retrieval kernel.

Supported sources:
- Claude Code: `~/.claude/projects/*/*.jsonl`
- Codex: `~/.codex/history.jsonl`
- Gemini CLI: `~/.gemini/tmp/<hash>/chats/session-*.json`
- Gemini Antigravity direct extract: `~/.gemini/antigravity/conversations/<uuid>.pb` or `~/.gemini/antigravity/brain/<uuid>/`

## Install

Public install from crates.io:

```bash
cargo install ai-contexters --locked
```

From a local checkout:

```bash
./install.sh
```

`install.sh` installs `aicx` + `aicx-mcp` from the current checkout and configures Claude Code, Codex, and Gemini when their MCP settings directories already exist.

From an accessible GitHub repo when you want unreleased source:

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

`install.sh` prefers the local checkout when one is present. Outside a checkout, it now defaults to the published crates.io package.

## Quickstart

### Layer 1 — build the canonical corpus

Extract the last 4 hours into `~/.aicx/`. Extractors are quiet on stdout by default (`--emit none`).

```bash
aicx all -H 4 --incremental
```

See what landed:

```bash
aicx refs -H 4
aicx refs -H 4 --emit paths
```

### Layer 2 — materialize into memex

Materialization is operator-driven — nothing syncs automatically.
You decide when to embed the canonical corpus into the memex retrieval
kernel (vector + BM25):

```bash
aicx memex-sync              # first build or incremental update
aicx memex-sync --reindex    # full rebuild (after model/dimension change)
```

Or do both layers in one shot:

```bash
aicx all -H 4 --incremental --memex
```

Pipe one JSON payload (handy for automation):

```bash
aicx all -H 4 --emit json | jq '.store_paths'
```

## What Gets Written Where

### Layer 1 — canonical store (extractors, `store`)
- `~/.aicx/store/<organization>/<repository>/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
- `~/.aicx/non-repository-contexts/<YYYY_MMDD>/<kind>/<agent>/<YYYY_MMDD>_<agent>_<session-id>_<chunk>.md`
- `~/.aicx/index.json`

### Layer 2 — semantic index (`memex-sync`, `--memex`) — operator-driven
- `~/.aicx/memex/sync_state.json` (sync watermark — tracks what has been materialized)
- LanceDB tables + Tantivy BM25 index (managed by rmcp-memex)

Framework-owned repo-local context artifacts (not written by the `aicx` CLI itself):
- `.ai-context/share/artifacts/SUMMARY.md`
- `.ai-context/share/artifacts/TIMELINE.md`
- `.ai-context/share/artifacts/TRIAGE.md`

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

Steering retrieval (filter chunks by frontmatter metadata):

```bash
aicx steer --run-id mrbl-001
aicx steer --project ai-contexters --kind reports --date 2026-03-28
aicx steer --agent claude --date 2026-03-20..2026-03-28
```

Semantic materialization (memex — the retrieval kernel).
Materialization is always operator-driven; nothing happens until you run it:

```bash
# First build: embed all unsynced canonical chunks into the memex index
aicx memex-sync

# Incremental: only new chunks since last sync (same command, watermark-tracked)
aicx memex-sync

# Full rebuild: wipe the index and re-embed everything
# Use after an embedding model or dimension change
aicx memex-sync --reindex

# One-shot shortcut: extract + materialize in a single pass
aicx all -H 48 --memex

# Fine-grained: per-chunk upsert instead of batch JSONL import
aicx memex-sync --per-chunk
```

Batch sync (default) uses metadata-rich JSONL import, preserving `project`, `agent`, `date`, `session_id`, and `kind`. Use `--per-chunk` only when you need single-document granularity.

Single-session Gemini Antigravity extract (conversation artifacts first, explicit step-output fallback):

```bash
aicx extract --format gemini-antigravity \
  ~/.gemini/antigravity/conversations/<uuid>.pb \
  -o /tmp/antigravity-report.md
```

## Docs

- `docs/ARCHITECTURE.md` (module map + data flows)
- `docs/COMMANDS.md` (exact CLI reference + examples)
- `docs/STORE_LAYOUT.md` (store + framework-owned `.ai-context/` layouts)
- `docs/REDACTION.md` (secret redaction, regex engine notes)
- `docs/DISTILLATION.md` (chunking/distillation model + tuning ideas)
- `docs/RELEASES.md` (release/distribution workflow + maintainer checklist)

## Notes

- Secrets are redacted by default. Disable only if you know what you’re doing: `--no-redact-secrets`.
- Framework integration expects `aicx` or `aicx-mcp` in `PATH`.

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
