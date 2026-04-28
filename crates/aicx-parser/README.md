# aicx-parser

Transcript parser, chunker, and slicer for AI session transcripts (Claude,
Codex, Gemini, Junie). Companion crate of
[aicx](https://github.com/Loctree/aicx).

`aicx-parser` is the reusable transcript processing core extracted from the
`aicx` 0.6.2 workspace. It contains the shared timeline types, chunking logic,
semantic segmentation, frontmatter parsing, and sanitization helpers used by
the `aicx` CLI and MCP server.

The crate is intentionally lean. It does not own filesystem walking, source
discovery, embedding, retrieval, networking, async runtimes, CLIs, or TUIs.
Consumers load transcript data through their own adapters, convert it into
`TimelineEntry`, and hand those entries to the parser APIs.

The main chunking path is frame-aware: user messages, agent replies, internal
thoughts, and tool calls can be preserved as separate frame kinds while still
supporting turn-pair windows and onion-layer slicing for retrieval workflows.

## Status

`aicx-parser` v0.1.0 is prepared path-first. The crate artifacts are ready for
`cargo publish --dry-run`, while the real publish is intentionally held until
downstream integration verification is complete.

## Quickstart

```rust
use aicx_parser::{chunk_entries, ChunkerConfig, TimelineEntry};

fn main() {
    let entries: Vec<TimelineEntry> = vec![
        // Loaded by your adapter from Claude, Codex, Gemini, Junie, or another source.
    ];

    let config = ChunkerConfig::default();
    let chunks = chunk_entries(&entries, "my-project", "codex", &config);

    for chunk in chunks {
        println!("{:?}: {} bytes", chunk.kind, chunk.text.len());
    }
}
```

Adapters are expected to handle IO and source-specific parse failures before
calling into this crate. The parser core receives already-normalized timeline
entries and returns deterministic in-memory chunks and segments.

## Public API Surface

The crate root re-exports the primary types and helpers used by `aicx`:

- `chunker`: `Chunk`, `ChunkerConfig`, `ChunkMetadataSidecar`, `classify_kind`
- `frontmatter`: `ReportFrontmatter`
- `sanitize`: `filter_self_echo`, `is_self_echo`, `normalize_query`
- `segmentation`: `ProjectHashRegistry`, `TieredIdentity`,
  `classify_cwd_tier`, `infer_repo_identity_from_entry`,
  `infer_tiered_identity_from_entry`, `semantic_segments`,
  `semantic_segments_with_registry`
- `timeline`: `ConversationMessage`, `ExtractionConfig`, `FrameKind`, `Kind`,
  `RepoIdentity`, `SemanticSegment`, `SourceInfo`, `SourceTier`,
  `TimelineEntry`
- `types`: `EntryState`, `EntryType`, `IntentEntry`, `Link`, `LinkType`

Module-level APIs are also public for consumers that need lower-level control:

- `chunker::chunk_entries`
- `chunker::format_chunk_text`
- `chunker::estimate_tokens`
- `chunker::chunk_summary`
- `frontmatter::parse`
- `sanitize::validate_read_path`
- `sanitize::validate_write_path`
- `sanitize::validate_dir_path`

The public type vocabulary includes the stable axes used by downstream stores:

- `Kind` for conversation, plan, report, or other chunk classification
- `FrameKind` for user message, agent reply, internal thought, and tool call
- `RepoIdentity` for owner/repository identity
- `SourceTier` for primary, secondary, fallback, or opaque identity evidence
- `SemanticSegment` for repo-aware groups of timeline entries

## Feature Flags

`aicx-parser` keeps its default dependency surface small:

- `default = []` - lean default build
- `json-schema` - opt in to `schemars::JsonSchema` derives on supported public
  types

The `json-schema` feature is useful for tools that expose parser types over MCP,
HTTP, or generated contract documentation. Consumers that only chunk or segment
in process can leave it disabled.

## Source Format Support

The parser is source-format agnostic. It currently supports normalized timeline
entries produced from these source families in the companion `aicx` crate:

- Claude JSONL sessions
- Codex session transcripts
- Gemini JSONL sessions
- Junie session transcripts

The filesystem walkers, JSON readers, session discovery logic, and
source-specific adapters live in `aicx`. This crate starts after that boundary:
given `TimelineEntry` values, it performs sanitization, segmentation, chunking,
classification, and frontmatter interpretation.

## Companion To aicx

`aicx-parser` is published as a companion library for
[aicx](https://github.com/Loctree/aicx), the operator CLI and MCP server for
AI session context ingestion and retrieval.

Use `aicx` when you want the full product surface: local corpus discovery,
commands, MCP tooling, optional embedding materialization, and operator-facing
workflows. Use `aicx-parser` when you want the reusable pure-compute transcript
core inside another Rust application.

## Dependency Budget

The v0.1.0 crate depends on:

- `anyhow`
- `chrono`
- `serde`
- `serde_json`
- `regex`
- optional `schemars`

It does not bring in async runtimes, HTTP stacks, terminal UI crates, model
runtimes, vector databases, or embedding clients.

## Release Strategy

The first public crate version is prepared as `0.1.0`, but publication is
path-first: downstream integration gets verified before the actual
`cargo publish` step. This avoids publishing a public API that immediately
needs a breaking correction after real consumer use.

## License

`aicx-parser` is licensed under the Business Source License 1.1 (BUSL-1.1).
See [LICENSE](LICENSE).

## Authors

- Maciej Gad <void@div0.space>
- Monika Szymanska <hello@vetcoders.io>
