# Intent Overlay v1

The intent overlay is a derived, versioned view that joins AICX intent
evidence to Loctree structural anchors. The published document schema is
`loctree.overlay.intent.v1`; the persistent side-index schema is
`aicx.overlay.side_index.v1`.

## Extract-era status (read this first)

| Surface | Status |
|---------|--------|
| Contract `loctree.overlay.intent.v1` | **Live** — path/symbol targets, attributions, supersede chain |
| Path target grain | Path (+ optional additive `start_line`/`end_line`; anchors are path-grain today) |
| C6 `canonical-projection-v1` **write** mill | **Retired** — `write_canonical_projection_at` fails closed |
| Overlay **primary feed** | **Catalog typed intents** (`catalog-v1` → frozen `intent1:` evidence refs) |
| Overlay **fallback feed** | Residual C6 fixtures only (tests / legacy materialization) |
| Live operator path | `aicx catalog rebuild` → `aicx intents -p <owner/repo>` → `aicx overlay --repo` |
| `aicx ingest` | Operator-md / loct-context-pack only — **not** session→C6 |

There is **no** palette command named `canonical ingest`. Fail-closed errors
from `aicx overlay` must not invent one.

When neither catalog intents nor residual C6 fixtures match the exact
`owner/repo` identity, `aicx overlay --repo <path>` exits with a message that
names the real recovery path. That is correct fail-closed behavior.

## On-disk contract

By default, `aicx overlay --repo <path> --format json` writes beneath:

```text
~/.aicx/overlay-index-v1/<repo-id-hash>/
  side-index.json
  ov1:<revision>.json
  catalog-feed-v1.json
  catalog-source-v1-<source-identity-hash>.json
  materialized-output-v1.json
  catalog-cache-owner-v1.json
  producer-<repo-id-hash>.lock
```

`side-index.json` preserves intent and semantic-group identity across
incremental runs. Each `ov1:<revision>.json` is a complete
`loctree.overlay.intent.v1` document. The revision binds the exact repository
identity, legacy archive revision when present, Loctree snapshot and anchor-catalog
revision, attribution/dedup algorithms, and configured embedding model.

The v1 directory and JSON semantics are stable consumer contracts. Additive
fields must remain serde-backward-tolerant. An incompatible format change must
use a new schema and directory (for example `overlay-index-v2`) while keeping
v1 readable.

`catalog-feed-v1.json` and `catalog-source-v1-*.json` are private, disposable
acceleration caches, not new source truth or a replacement for the lexical
index. They use the versioned `aicx.overlay.catalog-feed.v1` envelope. A source
slot is replaced when that source changes; appends do not create a new slot
for every revision. The resolved `AICX_HOME` (or explicit API `index_root`)
owns these files; isolated calls never fall back to the operator's global home.
`materialized-output-v1.json` binds an emitted document to its revision and
checksum. Older output documents without that receipt are rematerialized once,
reusing surviving side-index identities and embeddings rather than trusting a
potentially stale pre-fix document.

On Unix, overlay JSON files are created owner-only (`0600`) at the temporary
file's creation, before payload bytes are written. Atomic replacement keeps
that mode; unrelated atomic writers and caller-selected directory permissions
are not changed. Windows uses the directory's inherited ACLs; this change does
not claim a new Windows ACL policy.

`catalog-cache-owner-v1.json` binds private conversation slots to the exact
repository identity and a SHA-256 digest of the canonical AICX home. A custom
index root owned by another home/repository fails closed. A legacy root without
private source slots may adopt the marker; an unowned root already containing
source-shaped slots is never adopted implicitly. A fixed root-wide advisory
lock serializes the first ownership claim before any repo-scoped producer lock,
so two repositories cannot concurrently adopt the same explicit index root.

## Identity and publication rules

- Repository identity is exact and case-insensitive: `owner/repo` matches only
  that canonical identity.
- Ownerless repositories use the explicit virtual identity `_/repo`.
- Bare `repo` values and cross-owner matches fail closed. Checkout paths are
  never used to guess an owner.
- Eligible claims are catalog typed intents (frozen `intent1:` evidence refs) or
  residual C6 cards with frozen `evidence_event_id`s. Catalog-admitted sessions
  are read through the existing allowlisted conversation parser to derive those
  intents; arbitrary raw sessions or rendered Markdown are never a fallback
  feed. The cache does not change this admission boundary.
- Attribution is precision-gated. Low-confidence candidates stay in
  `unresolved_attributions` and are not emitted as structural truth.

## Emission lifecycle

1. `loct anchors --format json` supplies the repository identity, snapshot, and
   versioned anchor catalog.
2. AICX acquires a cross-process producer lock under the resolved index root,
   then checks current source and policy fingerprints before extracting the
   **catalog intent feed** for the exact `repo_id`. Unchanged input reuses the
   complete feed before parsing or classification. Otherwise, changed sessions
   are parsed once and unchanged conversation frames are reused for both the
   user-message and assistant-reply lanes. Both lanes still use the existing
   global classification, cap, and deduplication rules. If the catalog feed is
   empty, the existing residual `canonical-projection-v1` fallback applies. If
   both feeds are empty, fail closed with the real recovery path.
3. New claims enter the persistent side index. Semantic similarity proposes
   dedup candidates; typed-target separation and the negation veto decide
   whether they may merge.
4. Evidence-backed reversals form `supersedes` / `superseded_by` relations.
   Current entries sort before superseded history for the same target.
5. The emitter writes the side index atomically, then writes the
   revision-addressed document. A warm run reuses the matching revision unless
   `--rebuild` is requested. `--rebuild` also bypasses the private feed and
   conversation caches.

For the catalog feed, the current admitted claims are authoritative: claims
absent after source changes, removal, or ignore-policy changes cannot remain
in the new overlay merely because the side index once held them. Surviving
claims retain their IDs and embeddings while their provenance fields follow
the current feed. Switching to a residual C6 fallback retires catalog-origin
claims rather than carrying them into the fallback document; C6-to-C6 append
behavior is unchanged.

Catalog revisions now bind the entire materialized feed, including provenance
time, authority, and turn identity, under the `aicx.catalog-feed-material.v2`
hash domain. This corrects the previous text-only material hash: catalog
`sr1:`/`ov1:` values roll once on upgrade, while their wire format, frozen
evidence IDs, and surviving intent IDs remain unchanged. A timestamp-only
source change can no longer return an old document under the same revision.

## Cache freshness and concurrent callers

The warm key covers selected catalog rows and their order, live source size and
timestamps, resolved paths and platform file identity, current approved roots,
checkout-ignore policy, and producer/parser/feed versions. Unix fingerprints
include inode and ctime, so a same-length edit with a restored mtime invalidates
reuse. On non-Unix platforms, a bounded-buffer streaming SHA-256 check supplies
the stronger identity check: those warm calls still read source bytes, but do
not parse or classify them. Unix/macOS warm checks remain metadata-only.
Each hit revalidates source containment and readability; checking a path
is not counted as parsing its transcript. Snapshots before and after the read
must agree before the feed is published.

Stable missing sources retain the existing skip-and-diagnostic behavior. Their
absence is checked on every call, so a reappearing source invalidates the feed.
Permission failures, uncertain source state, malformed/unfinished tails, and
time-dependent parser fallback do not produce a reusable feed. Stable oversized
Codex sources may reuse the existing bounded signal projection: its explicit
bounded-coverage disposition and skipped-record count are persisted and replayed
on warm calls, never upgraded to complete-visible coverage. The original full
source remains untouched. Corrupt private cache JSON or checksums are cache
misses, not trusted claims. Symlink cache entries are rejected. Source or policy
mutation during a build fails visibly instead of publishing a mixed snapshot.
The side index and cache-owner marker are protected registries rather than
disposable caches: malformed JSON is a hard error and is never overwritten as
an apparent cache miss.

An invalid, non-empty Grok `created_at` remains `unknown` in typed parser
provenance. The visible one-shot parse may retain its prior wall-clock fallback,
but that projection and the complete feed are not cacheable until the source
provides a deterministic timestamp.

After a complete cacheable feed is published, AICX removes only immediate,
regular, non-symlink `catalog-source-v1-<64 lowercase hex>.json` slots that no
longer correspond to a current readable catalog source. This covers a removed
catalog row, a stable missing source, and a replaced source path. Cleanup never
targets source files, revisioned `ov1:` documents, the side index, locks,
directories, symlinks, or unrelated names. Unavailable sources, parser errors,
and uncacheable partial reads suppress cleanup; a later successful run retries
an orphan left by a crash or removal error.

The producer uses AICX's existing OS advisory locks, not an age-only lock file.
The same repo and resolved index root share one producer across worktrees.
A waiting caller checks the newly completed feed after acquiring the lock.
The ordinary lock wait is bounded (currently 60 seconds); exceeding it reports
an error and does not start another expensive build. A live owner's lock never
expires merely because the build took longer than expected; the OS releases it
when the owner exits.

Loctree may still request a refresh after its own TTL. Such a refresh is cheap
when source fingerprints are unchanged; no Loctree TTL/config change is needed
for this producer-side fix.

CLI stdout remains only the existing overlay JSON. The stderr receipt adds:

- `source_sessions_parsed`: source parsing attempts during this call;
- `source_sessions_reused`: unchanged sessions served without parsing;
- `feed_cache_hit`: whether classification was skipped by a complete feed hit.

`raw_session_files_opened` retains its legacy fallback-feed meaning; it is not
a count of catalog-admitted source parses. Use the new counters to verify the
performance contract. Regression coverage is in `tests/overlay_cache_cli.rs`
and the private cache unit tests; CLI fixtures own their AICX home and do not
contact an embedding provider.

## Open cuts (post extract-era feed)

Highest-leverage unfinished work:

1. Raise path target grain from path-only to path+line-range when Loctree
   anchors gain span identity, and consume that grain in loctree `find`
   (literal-boost step 4 — consumer cut, not this emitter).
2. Attribution quality on catalog-era theses vs residual C6 cards — catalog
   summaries are noisier; may need tighter distill / confidence gates without
   re-opening raw sessions.

Overlay code is gated by the application feature. It does not add dependencies
or symbols to the `loctree-consumer` slim read-core, and consumers do not need a
coordinated change to continue reading AICX core APIs or existing v1 overlays.
