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
| Overlay **read** of residual C6 fixtures | Still required by `aicx overlay` |
| Extract-era catalog → overlay feed | **Not wired** (open cut) |
| Live intents for operators | `aicx catalog rebuild` → `aicx intents -p <owner/repo>` |
| `aicx ingest` | Operator-md / loct-context-pack only — **not** session→C6 |

There is **no** palette command named `canonical ingest`. Fail-closed errors
from `aicx overlay` must not invent one.

When residual C6 fixtures are absent (typical after extracts-store polarize),
`aicx overlay --repo <path>` exits with a message that names the real
commands above. That is correct fail-closed behavior, not a broken install.

## On-disk contract

By default, `aicx overlay --repo <path> --format json` writes beneath:

```text
~/.aicx/overlay-index-v1/<repo-id-hash>/
  side-index.json
  ov1:<revision>.json
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

## Identity and publication rules

- Repository identity is exact and case-insensitive: `owner/repo` matches only
  that canonical identity.
- Ownerless repositories use the explicit virtual identity `_/repo`.
- Bare `repo` values and cross-owner matches fail closed. Checkout paths are
  never used to guess an owner.
- Only typed canonical cards with frozen `evidence_event_id` references are
  eligible. The emitter never falls back to raw sessions or rendered Markdown.
- Attribution is precision-gated. Low-confidence candidates stay in
  `unresolved_attributions` and are not emitted as structural truth.

## Emission lifecycle

1. `loct anchors --format json` supplies the repository identity, snapshot, and
   versioned anchor catalog.
2. AICX reads residual `canonical-projection-v1` fixtures (when present) and
   selects cards for the exact catalog identity. If none exist, fail closed
   with the real recovery path — do **not** invent a "canonical ingest"
   command. The write mill is retired; only fixture materialization (tests)
   or a future extract-era feed can refill this step.
3. New claims enter the persistent side index. Semantic similarity proposes
   dedup candidates; typed-target separation and the negation veto decide
   whether they may merge.
4. Evidence-backed reversals form `supersedes` / `superseded_by` relations.
   Current entries sort before superseded history for the same target.
5. The emitter writes the side index atomically, then writes the
   revision-addressed document. A warm run reuses the matching revision unless
   `--rebuild` is requested.

## Open cut: extract-era feed

Highest-leverage unfinished work:

1. Source overlay claims from durable catalog sessions / typed intent records
   (same frozen evidence refs, no raw-session fallback).
2. Raise path target grain from path-only to path+line-range when Loctree
   anchors gain span identity, and consume that grain in `find`.
3. Keep fail-closed identity: exact `owner/repo`, no bare-repo fan-out.

Overlay code is gated by the application feature. It does not add dependencies
or symbols to the `loctree-consumer` slim read-core, and consumers do not need a
coordinated change to continue reading AICX core APIs or existing v1 overlays.
