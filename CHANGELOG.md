# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [0.5.5] - 2026-03-31

### Performance
- **Steer Indexing:** Integrated `rmcp-memex` (LanceDB backend) to dramatically speed up `aicx steer` and `aicx_steer` MCP queries. Metadata searches now take milliseconds instead of seconds by bypassing filesystem sidecar parsing in favor of a columnar metadata index.
- **Fast Text Search:** Upgraded `aicx_search` MCP tool to use the embedded `BM25Index` and `StorageManager` from `rmcp-memex`. Full-text searches across all stored contexts are now instantaneous, replacing the slow sequential file scans.

### Added
- **Frontmatter steering metadata** (`workflow_phase`, `mode`, `skill_code`, `framework_version`) on `Chunk` and `ChunkMetadataSidecar`.
- **`aicx steer` CLI command** — retrieves chunks by steering/sidecar metadata (run_id, prompt_id, agent, kind, project, date range).
- **`aicx_steer` MCP tool** — same steering-aware retrieval for MCP clients.
- **`/api/search/steer` dashboard endpoint** — HTTP GET with the same filtering surface.
- **Live search** with CLI `aicx search` subcommand and real-time result deduplication.
- **Resizable dashboard** with drag-to-resize panels.
- **Store progress reporting** on stderr (TTY-gated `Chunking... N/M segments`).
- Session metadata (agent, model, cwd) included in search output.
- `cwd` field on `Chunk` for working-directory awareness.

### Changed
- Frontmatter parser now separates `telemetry` from `steering` and strips detected frontmatter from chunk text even when YAML is malformed.
- Extracted shared types (`types.rs`) to break the `segmentation ↔ store` cycle; `segmentation` no longer depends on `store`.
- Removed `init` submodule and deprecated `Init` command (returns naturally instead of `process::exit`).
- Search results now strip aicx boilerplate for cleaner output.
- Docs: "memory extraction" → "timeline extraction", "vector memory" → "semantic index" across README, ARCHITECTURE, COMMANDS, and help text.

### Removed
- `src/init.rs` deleted (`git rm`); init flow fully retired.

## [0.5.4] - 2026-03-31 (Pre-release)

### Fixed
- Sync result reporting precise enough for framework orchestration.
- Hardened `aicx` to `rmcp-memex` transport seam.

## [0.5.3] - 2026-03-30

### Added
- **Frontmatter steering metadata** (`workflow_phase`, `mode`, `skill_code`, `framework_version`) on `Chunk` and `ChunkMetadataSidecar`.
- **`aicx steer` CLI command** — retrieves chunks by steering/sidecar metadata (run_id, prompt_id, agent, kind, project, date range).
- **`aicx_steer` MCP tool** — same steering-aware retrieval for MCP clients.
- **`/api/search/steer` dashboard endpoint** — HTTP GET with the same filtering surface.
- **Live search** with CLI `aicx search` subcommand and real-time result deduplication.
- **Resizable dashboard** with drag-to-resize panels.
- **Store progress reporting** on stderr (TTY-gated `Chunking... N/M segments`).
- Session metadata (agent, model, cwd) included in search output.
- `cwd` field on `Chunk` for working-directory awareness.

### Changed
- Frontmatter parser now separates `telemetry` from `steering` and strips detected frontmatter from chunk text even when YAML is malformed.
- Extracted shared types (`types.rs`) to break the `segmentation ↔ store` cycle; `segmentation` no longer depends on `store`.
- Removed `init` submodule and deprecated `Init` command (returns naturally instead of `process::exit`).
- Search results now strip aicx boilerplate for cleaner output.
- Docs: "memory extraction" → "timeline extraction", "vector memory" → "semantic index" across README, ARCHITECTURE, COMMANDS, and help text.

### Removed
- `src/init.rs` deleted (`git rm`); init flow fully retired.

## [0.5.2] - 2026-03-28

### Added
- **YAML frontmatter parsing** for chunk metadata extraction.
- **Sidecar files** (`.meta.yaml`) written alongside memex chunks for external tooling.

## [0.5.1] - 2026-03-24

### Added
- **Repo-signal segmentation** in the store pipeline — chunks now carry repository identity signals.
- **Memex chunk sidecars** and `--preprocess` flag for pre-processing before memex push.
- **Makefile** with comprehensive build, test, lint, and release targets.
- Gemini truncation support and improved fuzzy search scoring.
- Test: repo-centric store runtime contract (`runtime_cli_store_contract.rs`).
- Test: legacy Codex format rejection (`legacy_codex_format_test.rs`).

### Changed
- Store contracts and migration scaffolding landed for repo-centric retrieval.
- Read/query surfaces hardened for repo-centric store paths.
- Checkpoint extraction seam hardened.

### Fixed
- Gemini JSON message structures preserved instead of being flattened (`sources.rs`).

## [0.5.0] - 2026-03-21

### Added
- **Repo-centric Migration Assistant:** Added the `aicx migrate` subcommand. This tool safely migrates older file-centric contexts (`file: <name>`) in your `~/.ai-contexters` store to the new canonical repo-centric directories. Use `aicx migrate --dry-run` to preview the changes.

### Changed
- **Behavioral Shift (Identity Model):** AICX now uses a canonical repo-centric identity model. Extracted contexts and stored artifacts are now grouped primarily by repository name rather than the raw filename of the agent log. This significantly improves retrieval quality and consistency, especially when syncing contexts to vector stores (memex) or running direct extractions.
- Direct `extract` now infers repository identity when possible, demoting file provenance to secondary metadata.

## [0.4.3] - 2026-03-17

### Fixed

- Corrected the `SECURITY.md` disclosure path so private vulnerability reports go to the public `VetCoders/ai-contexters` repository instead of a stale owner link.
- Updated GitHub Actions workflow dependencies to current major versions for `checkout`, `cache`, `setup-python`, `upload-artifact`, and `download-artifact`, removing the Node 20 deprecation surface from future CI and release runs.

## [0.4.2] - 2026-03-17

### Added

- Tracked `Cargo.lock`, so `--locked` now works in CI and release automation instead of failing on GitHub runners.
- Shared validated filesystem helpers in `sanitize.rs` for safe file creation, file reads, and directory reads.

### Changed

- Public install docs and `install.sh` now reflect the live crates.io path, while still supporting local checkout and git install modes.
- Security-sensitive file and directory reads now go through validated helper paths across `init`, `intents`, `main`, `rank`, and `sources`.

## [0.4.1] - 2026-03-17

### Added

- Release/distribution docs now spell out the current source-first install path and the tag-driven GitHub Release lane.

### Changed

- Installer now prefers local checkout installs, supports a git fallback, and finishes setup with a quiet incremental refresh plus compact summary output.
- MCP background refresh and `aicx_store` now use the real incremental rescan path (`aicx all --incremental --emit none`) instead of relying on a misleading stdout contract.
- `docs/COMMANDS.md` has been expanded to cover the active CLI surface and current stdout defaults.

## [0.4.0] - 2026-03-16

### Added

- **MCP server** (`aicx serve` / standalone `aicx-mcp` binary): 4 tools (search, rank, refs, store) over stdio and streamable HTTP transports.
- **Per-chunk quality scoring** (`rank.rs`): content-level signal/noise classification (0-10 scale) replacing the old all-SIGNAL output.
- `aicx rank` subcommand with `--strict` (hide noise) and `--top N` flags.
- **Dashboard search API**: `/api/search/fuzzy`, `/api/search/semantic`, `/api/search/cross` endpoints with rmcp-memex integration.
- `/api/health` and `/health` endpoints.
- Polish diacritics normalization for fuzzy search (wdrozenie matches wdrozenie).
- `project=` filter on fuzzy search (scopes to single project).
- Auto-rescan before search queries (incremental, milliseconds).
- Unified JSON error contract for all 400 responses.
- `aicx intents` subcommand for structured intent/decision extraction.

### Changed

- Rank made default command (`aicx -p proj` runs rank).
- Skills removed from repo — canonical source: VetCoders/vetcoders-skills.
- Package excludes: `*.html`, `*.patch`, `*.orig`, `.ai-agents/`, `skills/`.

### Added (Governance)

- LICENSE (MIT), CONTRIBUTING.md, CHANGELOG.md, SECURITY.md.
- GitHub Actions CI workflow (ubuntu + macos-14).
- Issue templates (bug report, feature request).
- Cargo.toml: keywords, categories, homepage, excludes.

### Fixed

- Bundle grouping bug in rank output.
- `.ai-agents/` paths now repo-relative, not absolute.
- Trailing whitespace in `is_noise_artifact`.
- Redundant closure in default command path.

## [0.3.1] - 2026-03-13

### Changed

- Refactored `run_extraction` to use `ExtractionParams` struct.

### Fixed

- Clippy `nonminimal-bool` warning.

## [0.3.0] - 2026-03-12

### Changed

- Renamed CLI binary from `agent-memory` to `aicx`.
- Updated showcase copy to Claude Code focus.

### Added

- VetCoders skills suite and ai-contexters skill.
- `vetcoders-decorate` and showcase polish.
- Memex-first dashboard generator.

## [0.2.x] - 2026-02 to 2026-03

### Added

- Codex and Gemini support in extract.
- `extract` subcommand for direct Claude file processing.
- Intent and TODO signal surfacing in chunk output.
- Agent prompt defaults and init improvements.
- Claude stream-json mode with `--verbose` flag.
- Ultrathink/Insight and Plan Mode signal extraction.
- Chunk highlights and redaction optimizations.
- `action`/`emit` flags and artifacts layout.
- Semantic chunker and memex integration.

### Changed

- Init mode and store command improvements.

### Fixed

- Assistant message extraction from content array.

## [0.1.0] - 2026-01

### Added

- Initial commit as `agent-memory` CLI tool.
- Claude Code JSONL extraction.
- Codex history support.
- Markdown and JSON output generation.

---

Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders
