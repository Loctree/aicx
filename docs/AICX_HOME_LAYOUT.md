# AICX home layout

`AICX_HOME` defaults to `~/.aicx`. The runtime root is resolved by
`src/aicx_home.rs` and contains compact identity metadata, optional readable
extracts, derived indexes, and operator state.

```text
$AICX_HOME/
  catalog/
    sessions.jsonl
  extracts/                         # optional whole-session cache
  indexed/
    _all/
      hybrid/
        CURRENT
        generations/<generation>/
          tantivy_lex/
          dense.exact_mmap_v1.bin   # optional
          manifest.json
      source_parse_state.v1.json
  context-corpus/                   # explicit Loctree/example ingestion
  state/
  locks/
  config.toml
  .aicxignore                       # checkout paths excluded from the index
```

## `.aicxignore`

`$AICX_HOME/.aicxignore` lists checkout paths that must not enter search
memory. `~/Repozytoria/moje_prywatne` covers that directory and every
nested repo under it. Absolute paths work the same way.

The catalog still records the session. Multi-root sessions already split
on `cwd` into project buckets; only frames whose `cwd` sits under a listed
prefix are dropped before index/extract-for-search. A stray `cd` into a
private tree does not throw away the rest of the session.

```
# ~/.aicx/.aicxignore
~/Repozytoria/moje_prywatne
/Volumes/secret/client-side
```

## Catalog

Each catalog row maps one session to:

- session id
- project
- agent
- date
- cwd
- canonical source path
- title/first user line
- machine
- source length and mtime-ns

The catalog adds topical project attribution without duplicating conversation
content.

Inspect drift without rewriting: `aicx catalog status` (see
[COMMANDS.md](./COMMANDS.md) and [MULTI_MACHINE.md](./MULTI_MACHINE.md)).

## Extract cache

`aicx extract` renders one readable session on request.
`aicx index --cache-extracts` may cache those renderings under `extracts/`.
Deleting the cache does not delete source truth.

## Search generations

`indexed/_all/hybrid/CURRENT` names the published generation. Tantivy is the
default query path. Dense mmap is optional and read only for `--deep`.

After the `CURRENT` pointer flips, unreferenced `generations/<id>/`
directories older than the last two (live + one rollback) are deleted.
Legacy `embeddings.ndjson` and `dense_brute_force.ndjson` are retired
intermediates. Their deletion is still an explicit operator action.

## Residual old artifacts

An existing `~/.aicx/store/` tree is a **legacy archive**, not a live write
target. Doctor and migration code can inspect or quarantine those files through
`src/legacy_archive/`. No catalog, extract, index, wizard, API, or MCP
production path creates per-frame cards or projection stage directories.

## Configuration precedence

1. non-empty `AICX_HOME`;
2. `[storage].home` in `~/.aicx/config.toml`;
3. `~/.aicx`.

Configured homes must be absolute (or `~/...`) and cannot contain parent
traversal or control characters.

## Context corpus

`context-corpus/` is an explicit append-only example-evidence surface for
Loctree context packs. It is not agent-session memory and is excluded from the
live session index. See [CONTEXT_CORPUS.md](./CONTEXT_CORPUS.md).
