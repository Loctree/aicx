# Durable semantic memory and search quality

AICX 0.16 covers durable passage indexing and evidence retrieval. Personal,
team and shared memory boundaries remain a separate design phase; this release
does not introduce tenants or sharing.

Acceptance contract:

- Retain successful embedding work across interruption, corpus changes and
  publication. Reuse only an exact admitted input and model/input contract.
- Embed bounded passages covering the complete admitted extract, including its
  end. Keep parser/extract coverage limits explicit. Reject native token
  overflow instead of silently dropping input.
- Publish lexical sessions and their complete dense passages together through
  the existing atomic generation gate. Recheck source/privacy state before
  admission. Deleted and withheld material cannot enter the new generation.
- Fuse at session identity without rewarding sessions for having more passages;
  return the matching passage as evidence. Expose deep retrieval explicitly in
  MCP while keeping its current default.
- Verify interruption plus corpus drift, edits, removals, model changes,
  malformed vectors, tail retrieval, snippets, CLI/MCP behavior and fixed-query
  retrieval quality. Distinguish mock pipeline contracts from real-model quality.

Production index, preserved checkpoint and paused refresh schedule are protected.
Tests use isolated storage/provider roots; implementation does not imply a
production rebuild or an installed service restart.

## Incremental operation

Run `aicx index --semantic` for corpus changes. The report distinguishes parsed
and reused sources from reused and newly embedded passages. Successful admission
keeps the vector cache; interrupting the next run does not remove completed
batches. A model, endpoint, native weight digest or input-contract change selects
a different cache. Cloud deployments should use versioned model identities:
a server replacing weights behind an unchanged alias cannot be detected locally.

The first migration from the old session-prefix vectors requires new passage
vectors. That cost is explicit; an embedding of one text cannot represent a
different text. The existing generation stays usable while its replacement is
built. Legacy v1 checkpoints, including explicitly preserved copies, are not
rewritten, deleted or silently relabelled as passage vectors.

Passages cover the complete **admitted extract**. Parser/extract bounds and
`conversation_coverage` remain explicit; this is not a claim that every raw
provider byte is conversation. Native token overflow is an error, never silent
truncation. Batch fsync precedes progress, and preparation, reuse, throughput,
ETA and failures remain observable.

Every new source generation binds its privacy policy. A policy edit blocks
serving its stored text until a replacement is admitted. The candidate is
checked against the current source/policy snapshot before CURRENT changes.
Removed inputs disappear from that generation and its compacted current cache.
Historical generations, model-specific caches and explicitly preserved legacy
checkpoints are retention artifacts, not an automatic secure-erasure service.

Passage generations use manifest schema 3.0. Upgrade every index writer before
building the first passage generation, and keep scheduled writers paused until
that is verified. Older readers reject this schema; older running writers cannot
be repaired by changing a file on disk. The new writer refuses to replace an
unsupported future contract. The existing source generation remains intact
until the first new generation is atomically admitted.

New dense payloads carry a BLAKE3 digest and readers reject modified bytes.
Payload and native-weight digests are cached against file identity, length,
write time and metadata change time. Unchanged warm queries reuse only the
small digest receipt; modified or replaced files are hashed again. Filesystems
without a reliable identity/change stamp are hashed on every verification.
The cache retains no open handles, mappings or model buffers.
Legacy payloads remain readable with unknown freshness/integrity evidence;
upgrading a reader does not manufacture a new receipt for an old artifact.

## Query surfaces

CLI: `aicx search --deep 'question' --json`.
MCP: `aicx_search` with `{"query":"question","deep":true}`.
Both opt in to lexical plus semantic fusion. The ordinary default stays lexical.
A session votes once through its best semantic passage, so a long session does
not win by contributing more vectors. Evidence uses that complete bounded
passage in CLI text/JSON and MCP rather than truncating it a second time to 200
characters. Lexical snippets include bounded neighbouring context around the actual match.

`index_snapshot.freshness` is `current`, `stale` or `unknown`, separately from
successful query execution. Freshness covers the durable catalog and its known
live sources. New provider files enter that observation after catalog refresh;
`aicx index --semantic` performs the census before indexing. A current result is
a fingerprint observation at query time, with historical conversation coverage
still defined by the parser/extract receipts.

## Acceptance evidence

The canonical `make test-retrieval-eval` now executes source indexing and lexical
queries on twelve public fixtures; each known answer must appear in the top
five and in its evidence snippet. `make test-retrieval-eval-live` runs the same
questions against an explicitly configured real model and isolated index. A
cross-language control must miss the target lexically and rank it first through
hybrid retrieval, with the expected answer evidence.
The former zero-recall baseline is preserved as historical evidence and no
longer provides a passing floor. Full instructions live in
`tests/retrieval_eval/README.md`.

A fixed small corpus does not certify daily usefulness on all private session
history. Transcript Builder/Memex comparisons require matching source material;
no comparative ranking win is inferred from these gates.
