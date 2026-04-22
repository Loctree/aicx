# AI Contexters

Operator front door for agent session logs.

`aicx` orchestrates a two-layer pipeline:

1. **Canonical corpus** (`~/.aicx/`) — extract, deduplicate, chunk, and store
   agent session logs as steerable markdown with frontmatter metadata.
   This is ground truth. Built by extractors (`claude`, `codex`, `all`) and `store`.

2. **Optional semantic index** (memex) — embed the canonical corpus into a
   vector + BM25 index for semantic retrieval by agents and MCP tools.
   Built by `memex-sync`, or the `--memex` shortcut on any extractor.

`aicx` owns the canonical corpus; memex is an optional semantic index layered on top.

Supported sources:
- Claude Code: `~/.claude/projects/*/*.jsonl`
- Codex: `~/.codex/history.jsonl`
- Gemini CLI: `~/.gemini/tmp/<hash>/chats/session-*.json`
- Gemini Antigravity direct extract: `~/.gemini/antigravity/conversations/<uuid>.pb` or `~/.gemini/antigravity/brain/<uuid>/`

## Install

Public install from crates.io:

```bash
cargo install aicx --locked
```

Public install from npm:

```bash
npm install -g @loctree/aicx
```

This installs both shipped commands:

```bash
aicx --help
aicx-mcp --version
```

From a local checkout:

```bash
./install.sh
```

`install.sh` installs `aicx` + `aicx-mcp` from the current checkout and configures Claude Code, Codex, and Gemini when their MCP settings directories already exist.

From a release bundle:

```bash
bash install.sh
```

Bundle install copies prebuilt `aicx` + `aicx-mcp` into `~/.local/bin`, removes stale user-local / cargo-installed copies, then refreshes MCP configuration.
No Rust toolchain and no local memex compilation are required on the target machine.

Directly from GitHub Releases with SHA-256 verification before unpacking:

```bash
AICX_INSTALL_MODE=release bash install.sh
AICX_INSTALL_MODE=release AICX_RELEASE_TAG=v0.6.2 bash install.sh
```

On macOS this consumes the signed/notarized release zip published by CI on the
`dragon-macos` self-hosted runner. On Linux it consumes the release tarball.

From an accessible GitHub repo when you want unreleased source:

```bash
cargo install --git https://github.com/Loctree/aicx --locked aicx
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

`install.sh` prefers a colocated release bundle first, then a local checkout, and otherwise falls back to the published install path.

Maintainer release bundle path on macOS:

```bash
make release-bundle KEYS=~/.keys
make release-bundle KEYS=~/.keys NOTARY_PROFILE=vc-notary
```

That release path cleans `target/<triple>` after the bundle is safely written so
the self-hosted box does not keep hauling old release artifacts. Use `CLEAN=0`
when you explicitly want to keep the local build outputs.

Profile defaults:
- Runtime default: `base` — portable 1024-dim Qwen 0.6B memex preset
- Heavier runtime opt-ins: `AICX_RUNTIME_PROFILE=dev` (2560-dim Qwen 4B), `AICX_RUNTIME_PROFILE=premium` (4096-dim Qwen 8B)
- Native embedder build default: `AICX_BUILD_PROFILE=base`; opt into larger bundles with `AICX_BUILD_PROFILE=dev` or `AICX_BUILD_PROFILE=premium`
- Optional native embedder picker during install: `bash install.sh --pick-embedder`

Config truth:
- Active memex retrieval config lives in `rust-memex` discovery paths such as `~/.rmcp-servers/rust-memex/config.toml`
- Native embedder preferences live in `~/.aicx/embedder.toml`
- Current public release bundles stay slim; they do not auto-bundle model weights

## Quickstart

### Layer 1 — build the canonical corpus

Extract the last 4 hours into `~/.aicx/`. Extractors are quiet on stdout by default (`--emit none`).

```bash
aicx all -H 4                      # daily driver: watermark-tracked, skips already-processed entries
aicx store -p MyProject -H 720     # store-first: watermark-tracked refresh into the canonical corpus
aicx store -p MyProject -H 720 --full-rescan  # explicit backfill / recovery pass
```

`-p/--project` on extractors and `store` narrows source session discovery before
repo segmentation. One run can still resolve into multiple canonical repo buckets
or `non-repository-contexts`; `--emit json` makes that explicit through
`requested_source_filters` and `resolved_store_buckets`.

See what landed:

```bash
aicx refs -H 4
aicx refs -H 4 --emit paths
```

Surface contract:
- `aicx refs` is the active CLI inventory command for canonical chunks.
- There is currently no `aicx rank` CLI subcommand; ranking stays on the MCP surface as `aicx_rank`.
- `aicx init` is retired; framework bootstrap now lives in `/vc-init`.

### Layer 2 — materialize into memex

Materialization is operator-driven — nothing syncs automatically.
You decide when to build the optional memex semantic index
(vector + BM25):

```bash
aicx memex-sync              # first build or incremental update
aicx memex-sync --reindex    # full rebuild (after model/dimension change)
```

Or do both layers in one shot:

```bash
aicx all -H 4 --memex
```

Pipe one JSON payload (handy for automation):

```bash
aicx all -H 4 --emit json | jq '.store_paths'
aicx all -H 4 --emit json | jq '.resolved_store_buckets'
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
aicx all -H 24 --emit none
aicx refs -H 24
```

Full-window backfill (ignore the stored watermark explicitly):

```bash
aicx all -H 168 --full-rescan
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

Semantic materialization — turning canonical chunks into the optional memex semantic index.
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

# Explicitly keep the older stronger-workstation preset
aicx memex-sync --profile dev

# Opt into the heaviest preset for 4096-dim / Qwen 8B setups
aicx memex-sync --profile premium
```

Batch sync (default) uses metadata-rich JSONL import, preserving `project`, `agent`, `date`, `session_id`, and `kind`. Use `--per-chunk` only when you need single-document granularity.

Runtime profile resolution:
- Default with no explicit provider config: `base`
- One-off helper override: `aicx memex-sync --profile <base|dev|premium>`
- Env helper override: set `AICX_RUNTIME_PROFILE=<base|dev|premium>`
- Config override: set `RUST_MEMEX_CONFIG=/path/to/config.toml` or edit the discovered `rust-memex` config file, usually `~/.rmcp-servers/rust-memex/config.toml`
- Explicit `[embeddings]` and legacy `[mlx]` config remain authoritative; helper presets should be treated as convenience, not as hidden overrides of hand-pinned providers or dimensions

Example persistent config:

```toml
[embeddings]
required_dimension = 2560

[[embeddings.providers]]
name = "mlx-local"
base_url = "http://127.0.0.1:1234"
model = "qwen3-embedding-4b"
priority = 1
```

Single-session Gemini Antigravity extract (conversation artifacts first, explicit step-output fallback):

```bash
aicx extract --format gemini-antigravity \
  ~/.gemini/antigravity/conversations/<uuid>.pb \
  -o /tmp/antigravity-report.md
```

Review Vibecrafted workflow and marbles artifacts as a standalone dossier:

```bash
aicx reports \
  --repo ai-contexters \
  --workflow marbles \
  --date-from 2026-04-10 \
  --date-to 2026-04-12 \
  -o ~/.aicx/aicx-reports.html \
  --bundle-output ~/.aicx/aicx-reports.bundle.json
```

The generated HTML embeds the selected slice directly and can also import/export
compatible JSON bundles client-side, so you can merge multiple workflow slices
without standing up a server.

Local browsing now shares one surface:

```bash
# Static HTML artifact (default output: ~/.aicx/aicx-dashboard.html)
aicx dashboard --generate-html -p ai-contexters -H 24

# Live local server
aicx dashboard --serve -p ai-contexters -H 24

# Remote / Tailscale server with explicit CORS policy
aicx dashboard --serve --host 0.0.0.0 --allow-cors-origins tailscale --bg
```

The `tailscale` CORS preset accepts both tailnet IP origins and MagicDNS browser origins (`*.ts.net`).

## Intent Taxonomy

The intent engine classifies stored chunks into 9 semantic types with typed link relations:

| Type | What it captures | Initial state |
|------|-----------------|---------------|
| `intent` | User-expressed goal or proposal | proposed |
| `why` | Motivation behind a decision | active |
| `argue` | Multi-voice disagreement or trade-off | active |
| `decision` | Crystallized choice from discussion | active |
| `assumption` | Hypothesis treated as true until verified | proposed |
| `outcome` | Broad result of an action | done |
| `result` | Concrete measurable data point | done |
| `question` | Open knowledge gap | proposed |
| `insight` | Reframe backed by research evidence | active |

Link types: `derived_from`, `supersedes`, `verifies`, `contradicts`, `supports`, `results_in`, `answers`, `links_to`.

State transitions: Proposed → Active → Done/Superseded/Contradicted. Session-level post-processing detects unresolved intents (no outcome after 7 days), supersedes chains (newer entry on same topic), contradicted assumptions (result + failure signal), and insight sourcing (DerivedFrom links to research chunks).

```bash
aicx migrate
aicx migrate-intent-schema
aicx migrate-intent-schema --project MyProject --dry-run
```

## Docs

- `docs/ARCHITECTURE.md` (module map + data flows)
- `docs/COMMANDS.md` (exact CLI reference + examples)
- `docs/STORE_LAYOUT.md` (store + framework-owned `.ai-context/` layouts)
- `docs/REDACTION.md` (secret redaction, regex engine notes)
- `docs/DISTILLATION.md` (chunking/distillation model + tuning ideas)
- `docs/RELEASES.md` (release/distribution workflow + maintainer checklist)

## Notes

- Secrets are redacted by default on corpus-building commands (`claude`, `codex`, `all`, `extract`, `store`). Disable only if you know what you’re doing: `--no-redact-secrets`.
- Framework integration expects `aicx` or `aicx-mcp` in `PATH`.
- `aicx memex-sync` now emits live scan/embed/index progress on TTY stderr instead of going silent after preflight.

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
