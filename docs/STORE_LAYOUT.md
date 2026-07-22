# Store Layout

`aicx` writes artifacts to the canonical store under `~/.aicx/` (cross-repo, global, machine-local).

Optional store control file:
- `~/.aicx/.aicxignore` — glob patterns relative to `~/.aicx/`; matching chunk paths are excluded from steer indexing and downstream retrieval materialization.

## Canonical Store: `~/.aicx/`

Created and managed by `src/store.rs`.

### Layout

Contexts are chunked and stored by project and date:

```
~/.aicx/
  index.json
  store/
    <organization>/
      <repository>/
        <YYYY_MMDD>/
          <kind>/
            <agent>/
              <YYYY_MMDD>_<agent>_<session-id>_<chunk>.md
              <YYYY_MMDD>_<agent>_<session-id>_<chunk>.meta.json
  non-repository-contexts/
    <YYYY_MMDD>/
      <kind>/
        <agent>/
          <YYYY_MMDD>_<agent>_<session-id>_<chunk>.md
  steer_db/
    (LanceDB metadata index)
```

Notes:
- Repository identity is derived from entry `cwd` via `repo_name_from_cwd` (`src/sources.rs`).
- Chunks are stored in a canonical hierarchy: `store/<org>/<repo>/<date>/<kind>/<agent>/`.
- If no repository can be inferred, chunks fall back to `non-repository-contexts/`.
- Each `.md` chunk has a sibling `.meta.json` sidecar containing steering and telemetry metadata.
- `steer_db` is a fast LanceDB-backed index of all sidecar metadata, enabling millisecond filtering by `run_id`, `prompt_id`, `agent`, etc.

### Card Anatomy (schema v2)

A card is the pair `.md` + `.meta.json`. Since card schema v2
(see [`CARD_CONTRACT.md`](./CARD_CONTRACT.md)) the sidecar is the structured
L1 truth and the `.md` is a human projection.

The `.md` starts with YAML frontmatter (v2 writer and v2-migrated cards):

```markdown
---
project: loctree/aicx
agent: claude
date: 2026-06-27
frame_kind: tool_call
schema: card.v2
---

[03:11:42] tool: id: toolu_013dHDzbZaSHP3VjscubvrBv
...
```

The sidecar carries schema version, honesty fields, and (when present) the L0
`source` pointer and typed `signals`. Real v2 sidecar produced by
`aicx migrate --cards-v2` (from the D1 verification walk-around; home path
neutralized):

```json
{
    "agent": "claude",
    "claim_scope": "session_close",
    "content_sha256": "94239a770d4bc024b06106fbb0aec7ed2bc549ed7daf25bebb095e1ceb7729cd",
    "cwd": "/Users/user/vc-workspace/vetcoders/aicx",
    "date": "2026-06-27",
    "frame_kind": "tool_call",
    "freshness_contract": "historical",
    "id": "loctree/aicx_claude_2026-06-27_052",
    "kind": "other",
    "project": "loctree/aicx",
    "schema_version": 2,
    "session_id": "d0dcea8a-f733-4583-9f02-b135af7319c4",
    "verification_state": "not_verified_by_aicx"
}
```

Freshly written cards (post-A2 writer) additionally carry `source`
(`path`/`sha256`/`span` L0 pointer) and `signals` typed records when the chunk
has high-signal content.

**v1 compatibility:** legacy cards use a bracket header
(`[project: <project> | agent: <agent> | date: <YYYY-MM-DD>]`) instead of
frontmatter, and their sidecars have no `schema_version` field (reads as `1`).
Readers are header-agnostic; v1 cards remain readable forever.
`aicx migrate --cards-v2` upgrades them in place without changing body bytes.

### `index.json`

`index.json` is a manifest used to quickly list stored projects, dates and totals.
It is updated on every store write.

### Retrieval Integration

AICX exposes canonical chunks through filesystem search, steering metadata,
dashboard search, MCP tools, and the reusable `aicx-embeddings` library. Heavy
semantic retrieval/indexing is owned by Roost/rust-memex outside this CLI
surface.

`~/.aicx/.aicxignore` is honored before queueing chunks for the steer index and
should also be honored by downstream retrieval materializers that consume the
canonical store.

### Sibling: Context Corpus (`~/.aicx/context-corpus/`)

Immutable retention path for `loct-context-pack` prism artifacts. Lives next to
`store/` under the same `~/.aicx/` root, but is governed by a different
contract — append-only, operator-driven (via `aicx ingest --source
loct-context-pack`), excluded from `aicx intents` and live-truth semantic
indexes, and materialized into a separate `context-corpus.embeddings.ndjson`
namespace. See [`CONTEXT_CORPUS.md`](./CONTEXT_CORPUS.md) for the layout,
sidecar schema, and immutability filter.

## Identity Model & Compatibility Rules (v0.5.0+)

Historically, `aicx` grouped contexts under a file-centric identity (e.g., `file: session.jsonl`). Starting in v0.5.0, AICX shifted to a strictly repo-centric identity model.

**Compatibility Rules:**
- Older stored artifacts are NOT automatically orphaned or silently broken on read. However, they will no longer be updated.
- To maintain a single coherent history, run `aicx migrate`. This command will cleanly move your older `~/.ai-contexters` contexts into the correct repository-named directories in `~/.aicx/` and update your `index.json`.

## Store reconciliation contract

Identity migration and abandoned canonical-projection stages are exercised as
one recovery workflow against a copied `AICX_HOME`; neither operation requires
or permits mutation of the live store during verification.

1. `aicx doctor --migrate-identities` inventories identity drift and writes the
   resumable `migration/identity-manifest.json`. This is a dry run: a recursive
   hash of `store/` must remain unchanged.
2. `aicx doctor --fix-buckets --dry-run` inventories abandoned projection
   stages from their `stage.json` leases without reading payloads.
3. Apply only after reviewing both previews. Identity apply is resumable after
   every persisted step; eligible projection stages move, never delete, into a
   named `quarantine/projection-stages-<timestamp>/` artifact with a restore
   manifest.
4. Verify before/after inventories by file count, bytes, unique logical
   sessions, identities, resolvable physical duplicates, and unresolved
   conflicts. A non-identical pair for one logical session is an explicit
   conflict: no freshest-wins choice is allowed. Rollback must restore the
   original payload inventory byte-for-byte.

The synthetic contract pack at
`tests/fixtures/store_reconciliation/synthetic_store.json` mirrors casing/style
drift, ownerless and underscore-prefixed buckets, deprecated checkouts, and a
divergent logical-session pair without copying private session content. The E2E
tests materialize `reconciliation-before.json` and
`reconciliation-after.json` inside their temporary copied home.

## Repo-local Context Artifacts: `.ai-context/`

While `aicx init` has been retired in favor of framework-level orchestration (`/vc-init`), the framework still produces repo-local artifacts for agent awareness:

```
.ai-context/
  share/
    artifacts/
      SUMMARY.md
      TIMELINE.md
      TRIAGE.md
```

Recommended sharing rules:
- Commit `share/artifacts/SUMMARY.md` and `share/artifacts/TIMELINE.md` to help onboarding new agents.
- Keep other artifacts local or share case-by-case.
