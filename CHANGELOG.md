# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
## [Unreleased]

### Breaking

- state.json hash algorithm is now `blake3-128-v2` with length-prefixed
  field encoding (closes the raw-concat hash-splitting risk). Any older
  state — including legacy `siphash13-v1` (introduced in pass-2 G-1) and
  any interim `blake3-128-v1` builds — is treated as a legacy cache:
  current code migrates directly to `v2` on load and clears
  `seen_hashes` once. After upgrade, the first `aicx store` will
  re-process the recent `-H` window once. No data loss, but timeline
  may show duplicates if a parallel ingest is running.

### Added
- `aicx extract` batch conversation export command for emitting multiple
  session transcripts in a single pass without writing to the canonical store.
- `extract --conversation` output now carries `message_kind` and
  `collapse_stub_kind` metadata per message and surfaces extract
  statistics in the JSON projection.
- `aicx-monitor` crate for live CPU, RAM, GPU, and embedder process telemetry
  snapshots during long-running aicx pipelines.
- `aicx-progress-contracts` crate for shared indexing progress event contracts,
  telemetry snapshots, and sink traits across producers and UI consumers.
- Explicit `-p` filter syntax for `aicx index` and `aicx search`:
  `-p owner/repo` (strict slug), `-p owner/` (org wildcard),
  `-p /repo` (cross-org repo wildcard), `-p name` (cross-org match on
  organization or repository). Multiple `-p` flags or a comma list form a
  union. Filters resolve to canonical `<owner>/<repo>` slugs before
  downstream index lookup so a short repo name like
  `-p spotlight-convo-pipeline-v2` expands to its full
  `m-szymanska/spotlight-convo-pipeline-v2` index path.

### Changed
- **Project filter is now word-boundary path match, not substring.**
  `--project test` no longer matches `cwd: /tmp/fastest-project`; multi-word
  filters compose with AND semantics across path words, and multiple filters
  with ANY. Path is split on `/`, `\`, `-`, `_`, `.`; filter on `-`, `_`, `.`.
  Message-text matching is dropped entirely — a transcript that *mentions*
  a project name does not belong to that project.
- **Canonical store project filter (`aicx index/search -p`) no longer
  substring-matches.** `-p vista` previously matched `vista-portal`,
  `VistaBrain`, `vista-datasets`, `nextra-docs-vista` etc., ballooning a
  single-project request into seven projects (~32k chunks). Now `-p vista`
  matches the exact repo or organization name `vista` (case-insensitive).
  For multi-project intent, repeat the flag or use the explicit wildcards.
- `aicx extract --conversation` deduplicates exact-equal short user messages
  within the same session (≤ 1000 chars, ≤ 2 s delta). Assistant messages and
  long bodies are untouched.

### Fixed

- **Segmentation identity leak: text mentions could be promoted to
  assertable ownership.** `infer_tiered_identity_from_text` walked any
  absolute path it found in chunk text into the filesystem and called
  `git remote get-url origin` to resolve identity — so a chunk that
  merely *mentioned* `/Users/foo/Downloads/ai-collaborators/...` could
  hijack a session into whatever GitHub repo that local clone's remote
  pointed to (e.g. `Szowesgad/maciej-almanach`). Round-1 cut the FS
  walk; round-2 also drops text-mention identity from
  `infer_tiered_identity_from_entry` entirely. Entry-level identity now
  comes only from cwd / projectHash registry. Text mentions stay
  accessible through the standalone `resolve_bucket` API
  (`BucketingSource::ContentMention`) for future search-hint use cases,
  but never enter segment routing — so a session no longer splits on
  context_switch when a chunk casually links to another repo, and
  `segment.repo` no longer carries non-ownership signals.
- **`is_probably_repo_name` accepted date-shaped names.** Strings like
  `2026-01-22`, `2026_01_22`, `2026_0122` passed the alphanumeric+`.-_`
  filter and produced pseudo-repos such as `CodeScribe/2026-01-22` in
  the canonical store. New `looks_like_date_pattern` guard rejects
  these three shapes outright.
- **`aicx index/search -p <bare-name>` ambiguity is now reported.**
  When `-p codex` matches both an organization (`codex/*`) and a
  repository (`*/codex`), `resolve_project_filters_or_error` prints a
  stderr warning naming both matches and suggesting `-p codex/` or
  `-p /codex` to disambiguate. Filter behavior is unchanged (still
  returns the union); the warning just removes the silent WTF.
- `infer_repo_identity_from_known_layout` matches markers
  (`hosted`/`repos`/`repositories`/`github`/`git`) case-insensitively, so
  macOS conventions like `/Users/u/Git/Org/Repo` resolve through cwd
  instead of falling back to text inference.
- `aicx index -p` / `aicx search -p` reject filters with no matching
  project (instead of silently resolving to the `_all` bucket after a
  typo) and print accepted syntax in the error.
- Stale `embeddings.ndjson.tmp` checkpoint mismatch error now reports the
  checkpoint's recorded `schema/model/profile/dim` vs the active
  embedder's values, and suggests an exact `rm <path>` command.
- Junie extractor (`extract_junie_file`) now captures the full agent work
  trail — internal thoughts (`AgentThoughtBlockUpdatedEvent`), terminal
  commands (`TerminalBlockUpdatedEvent`), MCP calls (`McpBlockUpdatedEvent`),
  tool blocks (`ToolBlockUpdatedEvent`), file views
  (`ViewFilesBlockUpdatedEvent`), and file changes
  (`FileChangesBlockUpdatedEvent`) — in addition to the previously-only
  conversational user/assistant pairs. Sessions whose `ResultBlockUpdatedEvent`
  payloads are empty (most non-conversational steps) no longer index as
  bare prompts with zero context. Streaming snapshots are dedup'd per
  `(stepId, kind)` and pre-COMPLETED states are skipped for the streaming
  block kinds.
- Codex `extract --session <id>` now accepts a UUID prefix, suffix, or
  unique substring instead of requiring the full `session_meta.payload.id`.
  Ambiguous prefixes return a candidate list with an actionable error.
- Codex session parser surfaces aggregated diagnostics for missing
  `session_meta`, duplicate `session_meta.payload.id` values, filename ↔
  meta UUID mismatch, unparsable event_msg timestamps, and unrecognized
  event_msg `payload.type` values. Broad scans emit one summary line per
  run; direct file extracts emit per-file warning details.
- Codex `mcp_tool_call` and `mcp_tool_call_response` event types are now
  classified as `FrameKind::ToolCall` instead of being silently dropped.
- `infer_repo_identity_from_known_layout` (parser) now tries all five
  layout markers (`hosted`, `repos`, `repositories`, `github`, `git`).
  Previously a `?` inside the loop returned from the whole function on
  the first marker miss, so four of the five markers were dead code and
  paths like `~/repos/Org/Repo` fell back to the opaque bucket.
- Secret redaction now catches inline assignments such as
  `BRAVE_API_KEY="…"` or `api_key = "…"` embedded in prose and code
  spans, not only line-start environment declarations.

### Known Issues

- `cargo audit` still reports the RSA Marvin Attack advisory through the
  optional `rust-memex` transitive dependency surface. AICX does not use that
  RSA path as its own crypto hot path; the ignore rationale is tracked in
  `cargo-audit.toml` / `.cargo/audit.toml` until the upstream dependency stack
  clears it.

## [0.8.0] - 2026-05-15

### Added
- **Hybrid retrieval stack**: pure-Rust `BruteForceAdapter` for DenseIndex
  (zero C deps), Tantivy `LexicalIndex` adapter with Polish stemming and
  FilterCollector, retrieval evaluation harness with 50-query gold set and
  `make retrieval-eval` gate, fusion via Reciprocal Rank Fusion in the
  `aicx-retrieve` trait crate.
- **Live `aicx index` progress feedback**: per-chunk `IndexEvent` stream
  (RunStarted / ItemIndexed / ItemSkipped / ItemFailed / StatsTick /
  RunCompleted) with rolling rate and ETA. TTY-aware `IndicatifSink`
  shows a live progress bar with rate and ETA; piped runs fall through
  to structured `tracing` events. Previously the 75-minute embed loop
  emitted nothing on stdout until completion.
- **New workspace crates**: `aicx-progress-contracts` (typed event
  contracts, sink trait, rolling-rate helper) and `aicx-monitor` (live
  CPU/RAM/GPU and embedder process metrics via sysinfo, Apple Silicon
  GPU detection through ioreg).
- **Linux cross-compilation release matrix**: GitHub Actions workflow
  `release-linux.yml` plus `Cross.toml` config for x86_64/aarch64 musl
  and gnu targets.

### Changed
- **BREAKING**: NDJSON semantic index corruption now fails fast above the
  5% threshold instead of silent-swallowing corrupt lines. Operators
  running checkpoints from older builds may need `aicx index --sample 0`
  to rebuild cleanly.
- Zero-hour lookback (`--hours 0`) now aligns with the all-time contract
  across `aicx intents`, `aicx search`, and `aicx steer`.
- Active semantic index writer is reported as `busy` in `aicx doctor`
  output instead of falsely appearing idle.

### Fixed
- Partial semantic index builds resume from `.ndjson.tmp` checkpoint on
  subsequent runs instead of restarting from zero.
- Hybrid retrieve gate stabilized: fusion RRF orchestrator returns
  consistent ranks under mixed-adapter contention.

## [0.7.4] - 2026-05-15

## [0.7.3] - 2026-05-13

### Added
- Unified multi-project scope handling across search, intents, semantic index,
  MCP, dashboard, and doctor surfaces so operators can narrow to one or more
  projects with the same contract everywhere.

### Changed
- `aicx store` progress output is now bounded and human-readable: structured
  progress ticks remain machine-parseable while interactive terminals keep a
  stable three-line status view instead of flooding logs.

### Fixed
- Gemini JSONL extraction now treats `.jsonl` files as session transcripts,
  preserving `sessionId` metadata and allowing `aicx all` to ingest Gemini
  sources alongside Claude, Codex, Junie, and CodeScribe.
- Junk corpus bucket slugs are covered so malformed or placeholder project
  names no longer leak into canonical project grouping.

## [0.7.1] - 2026-05-12

### Changed
- improve failure UX and make lance optionality clear

### Fixed
- install python before release version check

## [0.7.0] - 2026-05-08

### Added
- **Context Corpus Contract** for immutable `loct-context-pack` prism packs: sidecars now carry `artifact_family`, `schema_version`, `truth_status`, `learning_use`, `keywords`, and `content_sha256`; `aicx ingest --source loct-context-pack <PACK_DIR>` retains packs under `$HOME/.aicx/context-corpus/...` with `index.jsonl`.
- `aicx store` writes content hashes into sidecars and skips duplicate chunk bodies in the target bucket; `aicx doctor --check-dedup` reports duplicate content hashes across the live store and context corpus.
- `aicx doctor` surfaces the context-corpus state as a first-class check (`context_corpus` field on `DoctorReport`): reports `empty (will be created on first ingest)` when the directory is absent, `empty (no batches yet)` when the tree exists but holds no chunks, or a `N chunks across M batch(es) / R repo(s)` summary when populated. Operators no longer need to `ls ~/.aicx/context-corpus/` to confirm corpus existence.
- New operator documentation `docs/CONTEXT_CORPUS.md` covering the immutable-corpus contract: ingest source semantics, `~/.aicx/context-corpus/<org>/<repo>/<date>/loct-context-pack/<batch>/{raw,sidecars,index.jsonl}` retention layout, sidecar schema fields (`artifact_family`, `schema_version`, `truth_status`, `content_sha256`, `keywords`), immutability filter behavior (`aicx intents` and live-truth semantic indexes exclude `Example`-role chunks), and the parallel `context-corpus.embeddings.ndjson` materialization namespace. Cross-linked from `STORE_LAYOUT.md`, `COMMANDS.md`, and `README.md`.
- **9-type intent taxonomy** (`EntryType` enum): Intent, Why, Argue, Decision, Assumption, Outcome, Result, Question, Insight — replaces the flat 4-kind `IntentKind`.
- **Intent entry state machine** (`EntryState` enum): Proposed → Active → Done/Superseded/Contradicted with explicit lifecycle transitions.
- **Typed link graph** (`LinkType` + `Link`): DerivedFrom, Supersedes, Verifies, Contradicts, Supports, ResultsIn, Answers, LinksTo — first-class relations between intent entries.
- **`IntentEntry` struct** in `types.rs` with stable deterministic IDs, confidence scoring, topic tags, and cross-project linking.
- **`classify_chunk_entries()`** — per-chunk classifier covering all 9 types with marker-based and NL-pattern heuristics; abstain-first (confidence < 0.5 = skip).
- **Session-level post-processing**: unresolved intent detection (7-day threshold), supersedes chain detection (same topic, newer date), contradicted assumption detection (Result + failure words), insight-to-source `DerivedFrom` linking (top-3 in session).
- **`intent_entries` field** on `ChunkMetadataSidecar` for sidecar-level intent storage (backward compatible: empty Vec default).
- **`aicx migrate-intent-schema`** CLI subcommand with `--dry-run` (default) for classification count reports per-type and per-project.
- 25 new unit tests: 20 classifier tests (per-type + abstain + all-9 chunk + deterministic IDs + tag inference), 5 session-level tests (supersedes, contradicted, insight linking, unresolved threshold, recent not tagged).

### Changed
- `aicx intents` and semantic index writes exclude immutable `loct-context-pack` examples from the live-truth namespace; context-corpus embeddings materialize to a separate `context-corpus.embeddings.ndjson` namespace.
- Operator surface wording: "push" → "materialize" in CLI help text, progress messages, and doc comments to reinforce the two-layer mental model (canonical corpus first, semantic materialization second).
- Semantic compatibility validation now detects stale metadata even when no documents exist yet in the memex index; reports diverged fields explicitly.
- Compatibility validation runs before file scanning in `memex-sync`, failing fast on config mismatches.
- `claude`, `codex`, `all`, and `store` now use watermark-tracked incremental refresh by default. `--full-rescan` is the explicit escape hatch for backfills, while legacy `--incremental` is accepted as a hidden no-op with a deprecation notice.
- `aicx dashboard` now owns both static HTML generation and live serving. `dashboard-serve` is kept as a hidden compatibility shim while public help/doc surfaces point to `aicx dashboard --serve`, including explicit `--allow-cors-origins` policies for non-loopback binds and `--bg` background launch.
- `aicx reports-extractor` is renamed to `aicx reports`, with default HTML output moved under `~/.aicx/` to avoid polluting the current working directory.

### Fixed
- Test isolation: source extraction tests use unique temp directories per test to prevent cross-test interference on parallel runs.

## [0.6.5] - 2026-05-06

### Added
- Public GitHub Release binaries for `aicx` and `aicx-mcp` on macOS arm64,
  Linux x64 GNU, and Linux arm64 GNU.
- Slim unsigned release archives with adjacent `.sha256` sidecars for each
  published target.
- Release-bundle install path that copies prebuilt `aicx` and `aicx-mcp`
  without requiring a Rust toolchain on the target machine.

### Changed
- GitHub Releases are the supported public binary install lane for this
  release. The npm wrapper lane remains present in-tree, but is not the active
  v0.6.5 install path until its platform packages match the release asset
  matrix.

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
- MCP background refresh and `aicx_store` now use the real incremental rescan path (`aicx all --emit none`, with `--full-rescan` reserved for backfills) instead of relying on a misleading stdout contract.
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

- LICENSE (BSL 1.1), CONTRIBUTING.md, CHANGELOG.md, SECURITY.md.
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
