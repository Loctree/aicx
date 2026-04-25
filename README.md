# AI Contexters

Operator front door for agent session logs.

`aicx` is store-first:

1. **Canonical corpus** (`~/.aicx/`) — extract, deduplicate, chunk, and store
   agent session logs as steerable markdown with frontmatter metadata. This is
   ground truth. Built by extractors (`claude`, `codex`, `all`) and `store`.

2. **Retrieval surfaces** — filesystem search, steering metadata, MCP tools, and
   a reusable native embedding library. AICX stays portable; heavy retrieval
   belongs to Roost/rust-memex.

`aicx` owns the canonical corpus and the portable local embedding foundation.
Roost/rust-memex owns the advanced retrieval/operator plane.

Supported sources:
- Claude Code: `~/.claude/projects/*/*.jsonl`
- Codex: `~/.codex/history.jsonl`
- Gemini CLI: `~/.gemini/tmp/<hash>/chats/session-*.json`
- Gemini Antigravity direct extract: `~/.gemini/antigravity/conversations/<uuid>.pb` or `~/.gemini/antigravity/brain/<uuid>/`

## Install

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

Native embedder profiles:
- `base` — F2LLM-v2 0.6B `Q4_K_M` GGUF, 1024 dims, about 397 MB
- `dev` — F2LLM-v2 1.7B `Q4_K_M` GGUF, 2048 dims, about 1.1 GB
- `premium` — F2LLM-v2 1.7B `Q6_K` GGUF, 2048 dims, about 1.4 GB
- Picker during install: `bash install.sh --pick-embedder`

Config truth:
- AICX native embedder preferences live in `~/.aicx/embedder.toml` or `AICX_EMBEDDER_CONFIG`.
- The picker writes `backend = "gguf"`, `profile`, `repo`, and exact `filename`; model hydration is explicit.
- Roost/rust-memex retrieval config remains separate, usually `~/.rmcp-servers/rust-memex/config.toml` or `RUST_MEMEX_CONFIG`.
- Current public release bundles stay slim; they do not auto-bundle model weights.

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

### Native local embeddings

AICX ships the reusable `aicx-embeddings` library behind the
`native-embedder` feature. The installed bundle does not carry model weights;
the picker can write config and optionally hydrate exactly one GGUF file:

```bash
bash install.sh --pick-embedder
hf download mradermacher/F2LLM-v2-0.6B-GGUF F2LLM-v2-0.6B.Q4_K_M.gguf
```

The config file is plain TOML:

```toml
[native_embedder]
backend = "gguf"
profile = "base"
repo = "mradermacher/F2LLM-v2-0.6B-GGUF"
filename = "F2LLM-v2-0.6B.Q4_K_M.gguf"
prefer_embedded = false
max_length = 512
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

### Native embedder config
- `~/.aicx/embedder.toml` — local GGUF backend/profile/repo/filename/path preference
- HuggingFace cache snapshots under `~/.cache/huggingface/hub/`

Framework-owned repo-local context artifacts (not written by the `aicx` CLI itself):
- `.ai-context/share/artifacts/SUMMARY.md`
- `.ai-context/share/artifacts/TIMELINE.md`
- `.ai-context/share/artifacts/TRIAGE.md`

Store ignore contract:
- Optional `~/.aicx/.aicxignore` excludes matching canonical chunk paths from steer indexing and downstream retrieval materialization.
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

Native embedder hydration — picking the local model without bloating the bundle:

```bash
bash install.sh --pick-embedder
cat ~/.aicx/embedder.toml
```

Heavy retrieval lives outside this CLI surface:
- Use Roost/rust-memex for advanced retrieval pipelines, provider routing, and operator-scale indexing.
- Keep Roost/rust-memex settings in its own config plane (`RUST_MEMEX_CONFIG`, usually `~/.rmcp-servers/rust-memex/config.toml`).
- Do not put the heavy retrieval provider config into `~/.aicx/embedder.toml`; that file governs only AICX local embeddings.

Example Roost/rust-memex provider config:

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
- Native embedding models are never downloaded silently by package install or MCP startup.

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
