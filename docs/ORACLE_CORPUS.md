# AICX Oracle Corpus Contract

AICX is store-first. The raw agent logs and the canonical corpus under
`$HOME/.aicx/` are the truth. Every search, steer, intent, BM25, Lance, or
embedding surface is a derived view that must be rebuildable from that corpus.

## Layers

- Layer 0 source logs: Claude, Codex, Gemini, Junie, Codescribe, and other
  local transcript sources. These are raw evidence.
- Layer 1 canonical corpus: normalized, deduplicated, chunked markdown plus
  sidecar metadata under `$HOME/.aicx/store/` and
  `$HOME/.aicx/non-repository-contexts/`. This is AICX ground truth.
- Derived views: filesystem fuzzy search, steering metadata indexes, BM25,
  Lance, native embeddings, and external rust-memex semantic indexes. These are
  accelerators, not sources of truth.

## Operator Surfaces

- `aicx search --json` and MCP `aicx_search` are semantic-first. When the
  semantic index and embedder are ready they return `oracle_status.backend =
  hybrid_rrf`, `index_kind = onion_content`, and `loctree_scope_safe = true`.
  This is the preferred retrieval surface for humans and agents. Hybrid results
  also carry an `index_snapshot` payload (`freshness_verified = false`,
  `source_chunks = N`): the result reflects the committed index manifest, not a
  live freshness check. To confirm there are no pending (un-embedded) chunks,
  run `aicx index status` (or `aicx doctor`, check `index_freshness`) — the
  search hot path deliberately does not pay for that scan.
- If semantic preconditions are missing, `aicx search --json` and MCP
  `aicx_search` degrade to canonical-store filesystem fuzzy search and include
  an explicit `semantic_fallback` payload. Treat fallback results as routing
  evidence only; Loctree must read the canonical chunks before trusting scope.
  MCP clients that need fail-fast behavior can pass `strict_semantic = true`.
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
the live store. `aicx eval search-quality --run --strict` measures the active
store/index and is intentionally store-dependent: a missing anchored corpus is a
substrate failure, not permission to weaken or delete hard queries.
