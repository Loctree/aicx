# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

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
