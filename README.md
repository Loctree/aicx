# AI Contexters

Operator front door for agent session history.

`aicx` orchestrates a two-layer pipeline:

1. **Canonical corpus** (`~/.aicx/`) — extract, deduplicate, chunk, and store
   agent session logs as steerable markdown with frontmatter metadata.
   This is ground truth. Built by extractors (`claude`, `codex`, `all`) and `store`.

2. **Semantic materialization** (memex) — embed the canonical corpus into a
   vector + BM25 index for retrieval by agents and MCP tools.
   Built by `memex-sync`, the `--memex` shortcut on any extractor, or the
   background `aicx-memex daemon`.

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

Toolchain-free release archive on macOS/Linux:

1. Download the right archive from [GitHub Releases](https://github.com/VetCoders/ai-contexters/releases).
2. Extract it.
3. Run the bundled installer from inside the extracted folder:

```bash
./install.sh
```

The bundled installer copies `aicx`, `aicx-mcp`, and `aicx-memex` into `~/.local/bin` by default (override with `AICX_BIN_DIR`), configures MCP clients with the absolute `aicx-mcp` path, bootstraps the canonical store, and starts or nudges the background memex daemon.
Each extracted bundle also includes `presence/index.html`, a local one-pager you can open in a browser before touching the terminal.

From a local checkout:

```bash
./install.sh
```

`install.sh` installs `aicx`, `aicx-mcp`, and `aicx-memex` from the current checkout, configures Claude Code, Codex, and Gemini when their MCP settings directories already exist, then bootstraps the canonical store and starts or nudges the background `aicx-memex daemon` so semantic indexing can catch up without another manual step.

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
cargo install --path . --locked --bin aicx --bin aicx-mcp --bin aicx-memex
./install.sh --skip-install
```

`install.sh` prefers the local checkout when one is present. Outside a checkout, it now defaults to the published crates.io package. After install, use `aicx-memex status` to inspect the daemon's current phase.
If you want the calmest front door instead of memorizing commands, run:

```bash
aicx
```

If you want the full section-by-section readiness report, run:

```bash
aicx doctor
```

If you want the tool to repair the obvious gaps it can handle by itself, run:

```bash
aicx doctor --fix
```

If you prefer a browser-first local view instead of starting in terminal output, run:

```bash
aicx dashboard --open
```

If you want the richer live local UI with on-demand regeneration, run:

```bash
aicx dashboard-serve --open
```

If you want the fastest re-entry path after that, run:

```bash
aicx latest -p ai-contexters
```

## Workspace Boundaries

The shared workspace is now split into three explicit Cargo packages:

- `aicx-parser` owns canonical extraction, chunking, frontmatter, store layout, ranking, and the parser-side heuristics reused by higher layers.
- `aicx-memex` owns steer indexing, memex materialization/search, and the daemonized background sync surface.
- `ai-contexters` stays thin and orchestration-focused: CLI entrypoints, MCP surface, dashboard server, local output/reporting, intents extraction, and compatibility binaries.

Boundary rule:

- `aicx-parser` must not depend on `rmcp-memex`.
- `aicx-memex` may depend on parser-side canonical contracts.
- `ai-contexters` is the glue layer that composes both surfaces for shipped binaries and operator-only UX.

Contributor loops can now target the relevant cone directly:

```bash
cargo check -p aicx-parser
cargo check -p aicx-memex
cargo check -p ai-contexters --bin aicx --bin aicx-mcp --bin aicx-memex

cargo test -p aicx-parser
cargo test -p aicx-memex
cargo test --bin aicx --bin aicx-mcp --bin aicx-memex
```

## Quickstart

Start with the guided front door:

```bash
aicx
```

It gives you the current verdict, suggested next moves, and the shortest command path forward.
If you want the full readiness breakdown, run `aicx doctor`.
If you want the same front door to also repair what it can automatically, use `aicx doctor --fix`.
If you would rather browse than memorize commands, use `aicx dashboard --open` for a local HTML snapshot or `aicx dashboard-serve --open` for the live local UI.
If you want the newest readable chunks after the readiness check, use `aicx latest -p <project>` and open one result directly with `aicx read <ref-or-path>`.

If you prefer to start with the sectioned health check immediately, run:

```bash
aicx doctor
```

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

Run one-shot syncs when you want explicit control, or hand the loop to the
background daemon when you do not want to think about indexing day to day:

```bash
aicx memex-sync              # first build or incremental update
aicx memex-sync --reindex    # full rebuild (after model/dimension change)
aicx-memex daemon            # background refresh + steer repair + memex sync
aicx daemon-status           # inspect daemon health / last cycle
```

The daemon bootstraps once on startup and auto-reindexes the semantic layer
when runtime embedding truth drifts, while keeping canonical `.md` outputs as
the source of truth.

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

### Layer 2 — semantic index (`memex-sync`, `--memex`, `aicx-memex daemon`)
- `~/.aicx/memex/sync_state.json` (sync watermark — tracks what has been materialized)
- LanceDB tables + Tantivy BM25 index (managed by rmcp-memex)
- `~/.aicx/daemon/aicx-memex.sock` (Unix socket control plane)
- `~/.aicx/daemon/aicx-memex.status.json` (last known daemon status snapshot)

Framework-owned repo-local context artifacts (not written by the `aicx` CLI itself):
- `.ai-context/share/artifacts/SUMMARY.md`
- `.ai-context/share/artifacts/TIMELINE.md`
- `.ai-context/share/artifacts/TRIAGE.md`

Store ignore contract:
- Optional `~/.aicx/.aicxignore` excludes matching canonical chunk paths from memex materialization and steer indexing.
- Patterns are matched relative to `~/.aicx/` using glob syntax, for example:

```gitignore
store/VetCoders/ai-contexters/**/reports/**
!store/VetCoders/ai-contexters/**/reports/2026_0406_codex_important_001.md
```

## Common Workflows

Daily “what changed?” with incremental refresh plus compact summary:

```bash
aicx all -H 24 --incremental --emit none
aicx latest -H 24 -l 5
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

Open one chunk directly after discovery:

```bash
aicx read store/VetCoders/ai-contexters/2026_0331/reports/codex/2026_0331_codex_sess-read01_001.md

# or paste an absolute path copied from `aicx search` / `aicx refs --emit paths`
aicx read /Users/you/.aicx/store/VetCoders/ai-contexters/2026_0331/reports/codex/2026_0331_codex_sess-read01_001.md
```

Semantic materialization (memex — the retrieval kernel).
Choose explicit one-shot syncs or a background daemon:

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

# Background loop: one startup bootstrap, then periodic refresh/sync on a Unix socket
aicx-memex daemon

# Inspect / control the daemon
aicx daemon-status
aicx daemon-sync
aicx daemon-stop
```

Batch sync (default) uses metadata-rich JSONL import, preserving `project`, `agent`, `date`, `session_id`, and `kind`. Use `--per-chunk` only when you need single-document granularity.
The daemon uses the same `.aicxignore` contract and will auto-reindex memex if an install/update changes runtime embedding truth.

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
- `presence/index.html` (repo-local product face / one-pager for demos and packaging)

## Notes

- Secrets are redacted by default. Disable only if you know what you’re doing: `--no-redact-secrets`.
- Framework integration expects `aicx` or `aicx-mcp` in `PATH`; background upkeep uses `aicx-memex` when installed.
- `aicx memex-sync` now emits live scan/embed/index progress on TTY stderr instead of going silent after preflight.

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
