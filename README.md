# aicx

**Operator front door for agent session logs.**

`aicx` catalogs live Claude, Codex, Grok, Gemini, Junie, Vibecrafted, and
operator-owned sources; renders readable whole-session extracts; and publishes
a lexical-first search index. Sources remain content truth. The compact catalog
owns session identity and topical project attribution.

There is no per-frame card mill and no `aicx store` command.

## Runtime model

1. **Catalog** — `~/.aicx/catalog/sessions.jsonl` maps a session to project,
   agent, date, cwd, canonical source path, title, machine, and live source
   fingerprint.
2. **Extract** — `aicx extract <agent> --session <id> --conversation` renders a
   readable user/assistant transcript. Caching under `~/.aicx/extracts/` is
   optional.
3. **Index** — `aicx index` incrementally parses changed sources, removes
   tool/internal/system noise, and atomically publishes the global `_all`
   Tantivy generation.
4. **Search** — `aicx search` queries CURRENT with lexical ranking plus a
   recency prior. `--deep` explicitly adds dense mmap re-ranking.

```bash
aicx catalog rebuild
aicx index
aicx search 'arrows vc-frame'
aicx search -p vetcoders/vibecrafted 'routing strzałek taby'
```

Project selection is metadata filtering over `_all`, not a requirement to build
parallel project indexes.

## Why this shape

- One catalog row replaces thousands of duplicated frame cards.
- Source fingerprints make indexing incremental for both new and changed
  sessions.
- Human-readable extracts stay whole-session and optional.
- Default search does not load an embedder or stream multi-gigabyte NDJSON.
- Missing indexes have a typed, bounded fallback; corrupt or stale indexes fail
  honestly.

## Source coverage

First-class roots include:

- `~/.claude/projects`
- `~/.codex/sessions`
- `~/.grok/sessions/*/chat_history.jsonl`
- `~/.gemini/tmp/*/chats/session-*.json`
- Junie sources
- Vibecrafted `control_plane/runtime_runs/*/transcript.log`
- explicit operator markdown and Codescribe imports

Source opens go through a canonical allowlist resolver. Traversal, non-files,
and symlink escapes are rejected.

## Intents and verification

The intent surface remains available over cataloged/indexed evidence:

```bash
aicx intents --kind decision --unresolved
aicx claims --session <id>
aicx results --session <id> --repo .
aicx clarify --session <id> --max 5
```

Indexes and agent claims are derived evidence, not automatic runtime truth.
See [ORACLE_CORPUS.md](docs/ORACLE_CORPUS.md).

## Other surfaces

- `sessions`, `list`, `sources` — discover and protect raw sources.
- `read`, `tail`, `dashboard`, `reports` — inspect derived artifacts.
- `serve` — MCP over stdio or HTTP.
- `wizard` — daily-driver UI; its Rebuild screen runs catalog rebuild then
  index.
- `doctor`, `health`, `index status` — bounded diagnostics.
- `legacy_archive` — explicit read/recovery ownership for residual old card
  trees. It is not an ingestion path.

Use `aicx --help` and `aicx <command> --help` for the authoritative grammar.
The maintained overview is [COMMANDS.md](docs/COMMANDS.md).

## Installation

```bash
cargo install --locked aicx

# Native GGUF embedder support:
cargo install --locked aicx --features native-embedder
```

Prebuilt releases and `install.sh` are the recommended path for non-Rust users.
See [install-paths.md](docs/install-paths.md).

## Development

```bash
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Repository work starts with Loctree structural mapping. Security fixes close
the path, ownership, or taint boundary directly; production silencers are not
accepted.

## Documentation

- [Architecture](docs/ARCHITECTURE.md)
- [Commands](docs/COMMANDS.md)
- [AICX home layout](docs/AICX_HOME_LAYOUT.md)
- [Context corpus](docs/CONTEXT_CORPUS.md)
- [Source protection](docs/SOURCE_PROTECTION.md)
- [Releases](docs/RELEASES.md)

Built so that months later the operator can still find the session, open the
source, read the conversation, and prove which evidence was current.
