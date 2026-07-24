# Context corpus contract

The context corpus is an explicit append-only retention surface for
`loct-context-pack` example evidence. It is separate from cataloged agent
sessions and from the global live-session search generation.

```bash
aicx ingest --source loct-context-pack <PACK_DIR>
```

## Layout

```text
$AICX_HOME/context-corpus/
  <organization>/<repository>/<YYYY_MMDD>/loct-context-pack/<batch>/
    raw/<chunk>.md
    sidecars/<chunk>.json
    index.jsonl
```

`src/legacy_archive.rs` currently hosts the compatibility ingest/read code for
this older explicit artifact surface. Runtime-root resolution still belongs to
`src/aicx_home.rs`.

## Contract

- Only explicit `ingest --source loct-context-pack` writes this tree.
- Re-ingesting an existing content hash is a no-op.
- Raw markdown is preserved.
- Each sidecar identifies `artifact_family=loct-context-pack`,
  `schema_version=context_corpus.v1`, example truth role, and `content_sha256`.
- Context packs never override repo/runtime truth.
- Live session catalog/index builds do not scan this tree.
- Intent and live retrieval paths exclude context-corpus sidecars.

## Why it remains separate

Context packs are structural snapshots and examples, not operator
conversations. Mixing them into session retrieval would create self-echo and
false attribution. The physical split keeps example evidence durable without
claiming it is live memory.

## Recovery

The corpus is source data for its own explicit consumers; derived embeddings
or indexes are rebuildable. Doctor may report duplicate hashes or malformed
sidecars but must not silently delete source artifacts.

For the rest of the runtime tree, see
[AICX_HOME_LAYOUT.md](./AICX_HOME_LAYOUT.md).
