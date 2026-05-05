# AICX Oracle Corpus Contract

AICX is store-first. The raw agent logs and the canonical corpus under
`$HOME/.aicx/` are the truth. Every search, steer, intent, BM25, Lance, or
embedding surface is a derived view that must be rebuildable from that corpus.

## Layers

- Layer 0 source logs: Claude, Codex, Gemini, Junie, CodeScribe, and other
  local transcript sources. These are raw evidence.
- Layer 1 canonical corpus: normalized, deduplicated, chunked markdown plus
  sidecar metadata under `$HOME/.aicx/store/` and
  `$HOME/.aicx/non-repository-contexts/`. This is AICX ground truth.
- Derived views: filesystem fuzzy search, steering metadata indexes, BM25,
  Lance, native embeddings, and external rust-memex semantic indexes. These are
  accelerators, not sources of truth.

## Operator Surfaces

- `aicx search --json` and MCP `aicx_search` return `oracle_status.backend =
  filesystem_fuzzy`, `index_kind = none`, and a non-null `fallback_reason`.
  Treat these results as routing evidence only. Loctree must read the canonical
  chunks before trusting scope.
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
