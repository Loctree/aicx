# Changelog

All notable changes to `aicx-parser` will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-04-28 (unreleased - pending integration verification)

Initial public release of `aicx-parser`, extracted from the `aicx` 0.6.2 workspace.

### Features

- Frame-aware transcript chunking with onion-layer slicing (outer/middle/inner/core)
- Source-format-agnostic API operating on `TimelineEntry` values parsed by consumer adapters
- Section-aware segmentation respecting markdown headers, code fences, and turn boundaries
- Sanitize preprocessing for boilerplate and UI noise filtering
- Frontmatter parsing for `kb:*` document conventions
- Optional `json-schema` feature for `schemars::JsonSchema` derives

### Notes

- This crate is companion to the `aicx` CLI/MCP tool. Source format adapters
  (Claude/Codex/Gemini/Junie filesystem walkers and JSON parsers) live in
  `aicx` itself.
- Lean dependency budget: `anyhow`, `chrono`, `serde`, `serde_json`, `regex`,
  optional `schemars`. No network, async runtime, CLI, TUI, embedding, or
  retrieval dependencies.
- Pure-compute library boundary: consumers handle storage, embedding, and
  retrieval, then call into this crate with normalized timeline entries.

### Migration from in-tree

Prior to v0.1.0, this code lived inside `aicx` (`src/chunker.rs`,
`src/sanitize.rs`, `src/segmentation.rs`, `src/frontmatter.rs`,
`src/timeline.rs`, `src/types.rs`). The extraction preserved file history
via `git mv` and was verified byte-identical for `aicx --help`/`--version`
and full-corpus store smoke tests.
