# aicx

**Operator front door for agent session logs.**

`aicx` turns raw transcripts from Claude Code, Codex, Gemini, Grok and other agents into a clean, deduplicated, steerable **canonical corpus** at `~/.aicx/`. This corpus is ground truth.

It is store-first and operator-driven: nothing mutates your corpus unless you run a command.

## The Model

### Layer 1 — Canonical corpus (the truth)
Read local agent session logs, normalise into a single timeline schema, deduplicate, chunk into steerable markdown with rich frontmatter (agent, run_id, prompt_id, timestamps, cwd, source refs, etc.).

This is the portable, human-auditable record of:
1. what the human wanted
2. what the agent claimed
3. what reality proves
4. what still requires a human decision

There is also **Layer 1b — Context corpus** for `loct` context packs (prism artifacts). These live under `~/.aicx/context-corpus/`, are excluded from normal `intents` and live-truth retrieval, and materialise into their own embeddings namespace.

### The Intents Engine ("silnik intencji")

This is the real payload.

**The 5-lane verification system** (the original operator flow, still visible in the command surface and in the corpus itself):

1. what the human wanted
2. what the agent claimed
3. what reality proves
4. what still requires a human decision

**Current structured form** (via `intents` + `migrate-intent-schema`):

Stored chunks are classified into a 9-type intent taxonomy. The `intents` command extracts `IntentRecord`s with `kind` (`decision`, `intent`, `outcome`, `task`, ...), confidence, state, and unresolved tracking (at session level or per-intent).

```bash
aicx intents --agent grok --kind decision --unresolved --unresolved-mode intent
aicx claims --session <id>
aicx results --session <id> --repo .
aicx clarify --session <id> --max 5
aicx migrate-intent-schema --dry-run
```

The corpus carries the signal. `intents` is the extractor. The lane commands (`claims`, `results`, `clarify`) are the operator verification surface. `migrate-intent-schema` upgrades the corpus to the full 9-type model.

See `docs/ORACLE_CORPUS.md` for the contract: raw/canonical corpus = truth; indexes are derived, rebuildable views that must disclose `oracle_status` (freshness, scope, fallback, Loctree scope safety).

### Layer 2 — Retrieval surfaces (optional, powerful)

- `search` — semantic by default (when embedder is available); automatic filesystem-fuzzy fallback. Always returns `oracle_status`.
- `steer` — retrieve by steering metadata (`run_id`, `prompt_id`, `agent`, `kind`, project, date, frame_kind) using sidecar data. Safe for precise, scope-narrowed re-entry.
- `serve` (MCP server) — `aicx_search`, `aicx_steer`, `aicx_intents`, `aicx_read`, `aicx_rank`, etc.
- `dashboard`, `reports`, `tail`, `read`.

`aicx` owns the canonical corpus and the reusable local embedding foundation. Advanced retrieval lives in the companion `rust-memex` project.

## Command Surface (current, from the live binary)

```
aicx — operator front door for agent session logs.

Operator-driven pipeline:
  Canonical corpus: extract, deduplicate, and chunk agent logs into
    steerable markdown at ~/.aicx/. This is ground truth.
  Layer 2 (optional semantic index): local embedding-backed retrieval for native builds,
    while the canonical corpus stays portable and useful without it.

Quick start:
  aicx all -H 4                      # build canonical corpus
```

### Corpus building (Layer 1)
- `all` — extract + store from all agents (Claude + Codex + Gemini + Junie + Codescribe) into the canonical corpus.
- `claude`, `codex`, `extract` (with `--format`), `store`, `ingest`.
- `conversations` — batch-export clean conversation JSONs without writing to the store.

Common powerful flags: `-H/--hours`, `--full-rescan`, `--user-only`, `--no-redact-secrets`, `--emit paths|json`, `-o` for local reports, `--loctree`.

### Session surface
- `sessions current` — the live session id (perfect for commit trailers and handoffs).
- `sessions list`, `sessions show`, `sessions report`.
- `list`, `sources` — raw agent logs on disk + protection status.

### Intents & verification (the lanes + taxonomy)
- `intents` — structured extraction from the corpus (`--kind decision,intent,outcome,task`, `--unresolved`, `--unresolved-mode session|intent`, `--agent`, `--frame-kind`, `--strict`, `--min-confidence`, `--collapse-session`, etc.).
- `claims`, `results`, `clarify` — the classic 5-lane tools for a single session.
- `migrate-intent-schema` — classify/upgrade stored chunks into the 9-type model.

### Retrieval & inspection
- `search` — semantic + fuzzy with quality scoring and `oracle_status`.
- `steer` — metadata-driven retrieval (requires the steer index).
- `read` (alias `open`), `tail`, `dashboard`, `reports`.

### Maintenance & truth
- `doctor` — diagnose + (optionally) repair the store and steer index.
- `health`, `refs`, `state`, `corpus`, `sources protect`.

### Integration
- `serve` — run as MCP server.
- `wizard` — interactive daily-driver that wires corpus, doctor, intents and store.
- `config`.

See `docs/COMMANDS.md` for the full map and every flag.

## Supported Agents

Claude Code, Codex (history + rollouts), Gemini (classic + Antigravity), **Grok** (full layout under `~/.grok/sessions/<encoded-cwd>/<session-uuid>/` — `chat_history.jsonl`, `events.jsonl`, `active_sessions.json`, etc.; first-class support via the same v1/responses parser + explicit current-session handling), Junie, Codescribe, and raw operator markdown via `ingest`.

## Quick Start (realistic operator flow)

```bash
# 1. Build / refresh the corpus (incremental by default)
aicx all -H 48

# 2. Check what’s actually live right now
aicx sessions current

# 3. Pull structured intents (the silnik intencji)
aicx intents --agent grok --kind decision --unresolved --limit 20

# 4. Deep dive on one session with the lane tools
aicx claims --session 019ecde7-9780-7ca0-b73b-dc931325b9d4 --agent grok
aicx results --session 019ecde7-9780-7ca0-b73b-dc931325b9d4 --agent grok --repo .
aicx clarify --session 019ecde7-9780-7ca0-b73b-dc931325b9d4 --agent grok --max 5

# 5. Health + repair
aicx doctor
```

## Philosophy (in the project’s own words)

- The canonical corpus **is** ground truth. Indexes, embeddings and dashboards are derived, rebuildable views that must always be able to tell you their `oracle_status` (freshness, scope, fallback, Loctree scope safety).
- Store-first and operator-driven. Nothing happens behind your back.
- Perception over memory. The raw + canonical record must remain usable even if the semantic layer is unavailable or stale.
- No silencers. Prefer loud, actionable diagnostics over silent degradation.
- High verification bar. Real runtime testing on the actual binary. Structural work starts with loctree before raw grep. `make check` is non-negotiable.

This is not “yet another RAG over chat logs”. It is an attempt to keep the *intention record* of agent work usable and provable over time — especially when the agents are running complex, multi-day workflows with their own internal state.

## Installation

```bash
cargo install --locked aicx
# with native GGUF embedder (Apple Silicon / Linux)
cargo install --locked aicx --features native-embedder
```

Prebuilt releases + the smart `install.sh` that ships with them are the recommended path for non-Rust users. See `docs/install-paths.md`.

Pin a specific release instead of `latest`:

```bash
AICX_INSTALL_MODE=release AICX_RELEASE_TAG=v0.10.0 bash install.sh
```

npm wrapper (`@loctree/aicx`) also exists.

## Development

See `AGENTS.md`. In short: `make check` always, loctree semantic first, verify on the real built binary, no new silencers without an extremely good recorded reason.

## Links

- Architecture & mental model: `docs/ARCHITECTURE.md`
- Exhaustive command reference: `docs/COMMANDS.md`
- Remote MCP agent adoption: `docs/MCP_AGENT_ADOPTION.md`
- Oracle / truth contract: `docs/ORACLE_CORPUS.md`
- Store layout, protection, redaction: `docs/STORE_LAYOUT.md`, `docs/SOURCE_PROTECTION.md`, `docs/REDACTION.md`
- Releases: `docs/RELEASES.md`
- Embeddings: `docs/EMBEDDINGS.md`

---

Built with excessive care. The goal is not to be the best AI context tool. The goal is that six months from now you can still understand what the agents were actually doing — and prove it.
