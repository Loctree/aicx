# AICX Oracle Corpus Contract

AICX is source-first. Raw agent logs own session content; the catalog owns
session identity and project attribution. Every search, steer, intent, BM25,
Lance, or embedding surface is a derived view that must be rebuildable from
those sources plus the compact catalog.

## Layers

- Layer 0 source logs: Claude, Codex, Gemini, Junie, Codescribe, and other
  local transcript sources. These are raw evidence.
- Layer 1 catalog and optional extracts: compact identity metadata under
  `$HOME/.aicx/catalog/` and, when requested, one readable whole-session file
  under `$HOME/.aicx/extracts/`.
- Derived views: filesystem fuzzy search, steering metadata indexes, BM25,
  Lance, native embeddings, and external rust-memex semantic indexes. These are
  accelerators, not sources of truth.

## Operator Surfaces

- `aicx search --json` and MCP `aicx_search` are lexical-first against the
  published `_all` CURRENT generation, with a recency prior. `--deep` opts into
  dense mmap RRF. Results carry an `index_snapshot`; run `aicx index status`
  when a fresh pending-corpus census is required.
- Only typed `IndexNotBuilt` may use the bounded recency-ranked filesystem
  fallback. Corrupt, stale, and busy indexes propagate their error. Treat
  fallback results as routing evidence only.
- `aicx intents --emit json` and MCP `aicx_intents` return
  `backend = canonical_corpus` and `index_kind = canonical_chunks`. This is
  canonical intent evidence, not semantic similarity.
- `aicx steer --json` and MCP `aicx_steer` return `backend = steer_metadata`
  and `index_kind = metadata_steer`. The index is derived and rebuildable; it is
  safe for Loctree metadata narrowing only when `source_paths_verified = true`
  and followed by canonical chunk reads.
- `aicx doctor --oracle` reports the whole oracle readiness state. Until a
  content semantic index is proven healthy, it reports
  `unsafe_for_loctree_scope`.

## Loctree Rule

Loctree may consume AICX for scoped context only when `loctree_scope_safe = true`
and the returned chunk paths are readable. Fuzzy fallback is deliberately marked
`loctree_scope_safe = false` even when it finds good-looking matches, because it
does not prove semantic coverage or freshness.

## Search Quality Budget

`tests/retrieval_eval/search_quality_seed.toml` is the dialogue-usefulness seed
matrix. It is separate from the 50-query backend retrieval harness and is meant
to keep human-useful dialogue ahead of runtime exhaust.

Every roadmap-critical query can declare:

- an `expected_identity` anchor and `expected_frame_kind` lane that a useful
  top-k hit must expose;
- `budget_top_k` plus `min_useful_top_hits` as the usefulness floor;
- `max_forbidden_noise_top_hits` and `[[questions.forbidden_noise]]` rules for
  tool-output exhaust, system-prompt echoes, duplicated compact recall, and
  opaque reasoning blocks;
- `max_duplicate_hits_per_anchor` to make compact-recall inflation visible
  instead of treating repeated copies as extra evidence.

`aicx eval search-quality --strict` validates the seed contract without reading
live sources. `aicx eval search-quality --run --strict` measures the active
CURRENT generation; a missing anchored corpus is a substrate failure, not
permission to weaken or delete hard queries.
