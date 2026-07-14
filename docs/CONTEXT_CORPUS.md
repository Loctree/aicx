# Context Corpus Contract

The **Context Corpus** is `aicx`'s immutable retention path for `loct-context-pack` prism artifacts. It exists alongside the canonical session-log store but is governed by a different contract: where the canonical store is mutable, deduplicated, and watermark-tracked, the corpus is **append-only example evidence** consumed by `vc-polarize` gating, doctrine drafting, and any agent that needs durable structural-truth fragments without polluting live-session retrieval.

> **TL;DR.** `aicx ingest --source loct-context-pack <PACK_DIR>` writes the pack into `~/.aicx/context-corpus/<org>/<repo>/<YYYY_MMDD>/loct-context-pack/<batch>/{raw,sidecars,index.jsonl}`. Sidecars are stamped with `artifact_family=loct-context-pack`, `schema_version=context_corpus.v1`, `truth_status.role=Example`, and `content_sha256`. The corpus does **not** show up in `aicx intents` or live-truth semantic indexes — it materializes into a separate `context-corpus.embeddings.ndjson` namespace.

## Why a separate corpus

The canonical store at `~/.aicx/store/` is **operator session truth**: extracted from raw agent logs, deduplicated by content, indexed by `steer_db` for steering metadata retrieval, and consumed by `aicx intents` for the 11-type intent classifier.

`loct-context-pack` artifacts are different in kind:

- They are **structural evidence** (prism reports, polarize doctrine drafts, downstream pack snapshots) — not session logs.
- They are **example-status** by definition (`truth_status.role = Example`) — they describe what happened in the structural snapshot at a point in time, not what the operator decided.
- They must be **immutable once ingested** — re-running `aicx store` should never rewrite them, and the live-truth filter must keep them out of `aicx intents` so the classifier does not learn from example evidence.

Mixing them into the live-truth namespace would poison the steer index and the intent graph. Keeping them in a parallel corpus preserves both surfaces.

## Retention path layout

```
~/.aicx/
  context-corpus/
    <organization>/
      <repository>/
        <YYYY_MMDD>/
          loct-context-pack/
            <batch>/
              raw/
                <chunk>.md
              sidecars/
                <chunk>.json
              index.jsonl
```

Notes:

- `<organization>/<repository>` mirrors the canonical store's identity model — derived from the sidecar's `cwd` / repo metadata via the same `repo_name_from_cwd` heuristic in `src/sources.rs`.
- `<YYYY_MMDD>` is the ingest date in the canonical compact form (no separators).
- `loct-context-pack` is the only artifact family the corpus currently accepts. Future families (e.g. `polarize-doctrine`, `dou-snapshot`) would land as siblings.
- `<batch>` is the operator-supplied batch label or a generated identifier — it isolates ingest runs so the corpus stays auditable.
- `raw/` holds the markdown chunk bodies exactly as the pack provided them.
- `sidecars/` holds one JSON sidecar per chunk, name-aligned (`<chunk>.md` ↔ `<chunk>.json`).
- `index.jsonl` is a per-batch manifest (one row per chunk) summarizing the sidecar metadata for downstream materializers without forcing them to re-walk the directory.

The corpus root is created lazily on first ingest by `store::context_corpus_root_dir()` (`src/store.rs:236`). Operators can pre-create the empty tree if they prefer — the layout is stable.

## Sidecar schema fields

Sidecars in the corpus carry the standard `ChunkMetadataSidecar` shape **plus** the corpus-specific fields. After `aicx ingest --source loct-context-pack` runs, every sidecar in the corpus is guaranteed to have:

| Field | Type | Value | Why |
|---|---|---|---|
| `artifact_family` | string | `"loct-context-pack"` | Identifies the corpus family. `is_context_corpus_sidecar()` (src/store.rs:1260) keys on this. |
| `schema_version` | string | `"context_corpus.v1"` | Pinned schema. Consumers should fail closed on unknown values. |
| `truth_status.role` | enum | `Example` | Forces the immutability filter — keeps the chunk out of live-truth retrieval. |
| `truth_status.runtime_authoritative` | bool | `false` | Corpus chunks never override repo truth. |
| `truth_status.stale_against_current_head` | bool | as-ingested | Set if the pack itself flagged staleness. |
| `truth_status.current_head_when_ingested` | string\|null | as-ingested | Optional snapshot of the ingest-time HEAD. |
| `content_sha256` | string | hex SHA-256 | Computed during ingest; enables `aicx doctor --check-dedup` to detect duplicates across the live store and corpus. |
| `keywords` | string[] | as-ingested | Carried from the pack for retrieval/materialization. |

If the pack's sidecar does not specify `truth_status`, ingest synthesizes the `Example` shape above. Sidecars are otherwise preserved verbatim.

The `index.jsonl` file mirrors a strict subset:

```jsonc
{
  "id": "<chunk-stem>",
  "path": "raw/<chunk>.md",
  "artifact_family": "loct-context-pack",
  "schema_version": "context_corpus.v1",
  "truth_status_role": "Example",
  "keywords": ["..."],
  "band": "9..12",
  "content_sha256": "..."
}
```

The `band` field is populated when the upstream pack carries a prism band (see `loctree.prism.v1`). Absent for non-prism packs.

## Immutability filter behavior

Two consumers of the canonical store apply the corpus filter:

### `aicx intents`

The intent classifier and session post-processor walk the canonical store under `~/.aicx/store/` only — the corpus root is not on the scan list. Even if a corpus chunk somehow lands inside the canonical hierarchy (e.g. via a manual `cp`), `is_context_corpus_sidecar()` flags it on read and the classifier skips it. Result: the 11-type taxonomy (Intent / Task / Commitment / Why / Argue / Decision / Assumption / Outcome / Result / Question / Insight) is learned strictly from operator session truth, never from example evidence.

### Semantic index (`aicx_search`, `aicx_rank`, dashboard `/api/search/semantic`)

Live-truth materialization writes to `<index_root>/embeddings.ndjson`. Corpus materialization writes to a parallel `<index_root>/context-corpus.embeddings.ndjson` (`src/vector_index.rs:238`, `context_corpus_index_path()` at `src/vector_index.rs:263`). Searches over the live-truth namespace never include corpus chunks; searches that explicitly target the corpus namespace get only corpus chunks. The two namespaces are physically separate files — there is no implicit join.

## Cross-references

- Canonical layout (live store): see [`STORE_LAYOUT.md`](./STORE_LAYOUT.md) for the `~/.aicx/store/` hierarchy. The corpus is a sibling of `store/`, not a subdirectory.
- CLI surface: see [`COMMANDS.md`](./COMMANDS.md) for `aicx ingest --source loct-context-pack <PACK_DIR>` (also `aicx doctor --check-dedup` for cross-namespace duplicate reports).
- Oracle contract: see [`ORACLE_CORPUS.md`](./ORACLE_CORPUS.md) for the broader rule that raw source logs and the canonical corpus are truth, and indexes are derived/rebuildable views.
- Embeddings layer: see [`EMBEDDINGS.md`](./EMBEDDINGS.md) for the native embedding library powering both namespaces.

## Operating principles

1. **Append-only.** Re-running ingest on a previously seen content hash is a no-op (the dedup layer in `ingest_loct_context_pack` skips the chunk and counts it under `deduped_chunks`). Sidecars are never overwritten in place.
2. **Operator-driven.** The corpus does not auto-fill from session logs. Strong gating: only `aicx ingest --source loct-context-pack` writes to it.
3. **Auditable.** Every batch carries `index.jsonl`; every chunk carries `content_sha256`. `aicx doctor --check-dedup` reports duplicates across the live store and corpus to surface unintended bleed.
4. **Disposable indexes.** The corpus directory is the source of truth. The `context-corpus.embeddings.ndjson` namespace can be rebuilt from the corpus at any time.
