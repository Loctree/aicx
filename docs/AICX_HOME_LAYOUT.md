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
  reader-conversations-v1/          # disposable, validated source-reader cache
    catalog-source-v1-<source-identity-hash>.json
    catalog-cache-owner-v1.json
    catalog-cache-owner.lock
  overlay-index-v1/<repo-id-hash>/   # derived intent overlay and private feed caches
    side-index.json
    ov1:<revision>.json
    catalog-feed-v1.json
    catalog-source-v1-<source-identity-hash>.json
    materialized-output-v1.json
    catalog-cache-owner-v1.json
    producer-<repo-id-hash>.lock
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
Path prefixes are literal; glob and negation syntax is rejected for checkout
rules. A Windows drive path (`D:\work\private`) matches in any letter case. A rule change invalidates the published index and matching extract
cache automatically. If the file cannot be read, indexing and conversation
retrieval stop instead of admitting unfiltered content.

```
# ~/.aicx/.aicxignore
~/Repozytoria/moje_prywatne
/Volumes/secret/client-side
```

## Validated conversation reuse

Catalog-backed intent queries reuse whole cleaned conversations under
`reader-conversations-v1/`. This uses the overlay conversation-cache validation
rules, not another published index. The lexical `CURRENT` generation is unchanged
by these reads.

The existing `indexed/_all/source_parse_state.v1.json` ledger also carries
physical source identity, parser/extract coverage, and whole-session scope
receipts for newly parsed sources. Earlier rows retain explicit unknown
coverage until a deliberate full rescan or a source change requires parsing;
normal maintenance never invents those receipts from current file metadata.

Reuse requires a readable allowlisted source, matching source identity and
fingerprint, matching multi-file bundle metadata, unchanged ignore policy,
repository layout and producer schema, a verified payload checksum, and
cacheable parser coverage. Stable bounded projections retain their skipped
record count on cold and warm reads; they never become complete coverage.
Layout checks use the parser's original recorded workdirs;
reduced visible frames cannot reconstruct tool-only or absorbed paths.
Whole-session scope is retained before query-specific frame and date filtering.
Source and cache-directory identities are checked again during the operation;
missing, unreadable, replaced, or transiently partial sources do not authorize
stale cached claims. Corrupt disposable payloads are re-parsed from their source.

On Unix the cache directory is `0700` and atomically written payloads are `0600`.
The owner marker binds the directory to its canonical AICX home; owner claims
serialize through an exclusive advisory lock. Once established, ordinary
lookups validate the exact marker under a shared lock without rewriting
exclusive-holder diagnostics or syncing the lock file for each source.
Source slots are replaceable and atomically published, so concurrent readers
never consume half-written JSON.
An existing validated slot can also satisfy a legacy scope hole. A cache-only
lookup never fills a missing or invalid slot by implicitly parsing its source.

This directory is machine-local derived state. It can be discarded when no
reader is using it; source logs, catalog identity, and published generations
remain authoritative. An empty cache requires a first source parse; warm reuse
does not promise that live-root discovery or changed-source parsing is free.

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

After the `CURRENT` pointer flips, superseded complete
`generations/<id>/` directories are deleted, leaving the live generation and
one rollback. Interrupted directories without `manifest.json` remain
quarantined because another writer may still own them; remove those only in an
offline cleanup after all AICX writers are stopped.
Legacy `embeddings.ndjson` and `dense_brute_force.ndjson` are retired
intermediates. Their deletion is still an explicit operator action.

## Overlay cache

`aicx overlay` keeps its existing versioned document and stable semantic-group
side index under `overlay-index-v1/`. Private catalog-feed and conversation
caches let unchanged calls skip transcript parsing and let incremental calls
parse only changed sources. They remain derived data, never canonical session
history. Current source identity and ignore policy govern reuse; `--rebuild`
bypasses these caches. A producer advisory lock serializes writers for the same
repo/cache root, including calls from different worktrees. See
[OVERLAY.md](./OVERLAY.md) for freshness, failure, and instrumentation contracts.
The private owner marker prevents an explicit shared index root from being
silently adopted by another repository/home. Obsolete per-source conversation
slots are collected only after successful complete publication; revisioned
overlay documents keep their existing history/retention contract.

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
