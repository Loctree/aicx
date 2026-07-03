---
contract_version: 2
status: active
owner: Vetcoders
last_reviewed: 2026-07-02
revalidate_by: 2026-08-02
---

# AICX Card Contract

> **runtime > kontrakt > pamięć.** This file is an anchor, not scripture — it
> carries an expiry date (`revalidate_by`); an old, unverified decision is
> suspect, not sacred. Runtime does NOT decide what SHOULD exist; runtime only
> shows what DOES exist. When runtime and this contract diverge, that is a
> **fracture**, not an automatic runtime win: either the code drifted (build it
> back to the contract) or the contract died (update the contract).

## Purpose

This contract defines the aicx card schema v2 at the type level. A card is the
pair of a chunk `.md` file and its `.meta.json` sidecar. In v2, the sidecar is
the structured L1 truth and the `.md` file is a deterministic human projection.

Schema v2 is additive. Existing v1 sidecars remain readable forever.

Unlike the A1 draft of this contract, everything below describes **landed
runtime behavior** (cuts A1–D1, see the decision log) — not aspiration. The
only forward-looking statements live under "Open / not landed".

## Layer Model

| Layer | Surface | Contract |
|---|---|---|
| L0 | Raw source session file | Immutable source material. The card sidecar may point back to it with `source.path`, `source.sha256`, and `source.span`; the raw payload is not copied into the pointer. |
| L1 | `.meta.json` sidecar | Machine truth for card identity, provenance, schema version, claim honesty, typed signals, and content hash. Readers should prefer this surface when a structured field exists. |
| L2 | Chunk `.md` | Human projection for reading and retrieval context. The v2 writer renders a YAML frontmatter header (`schema: card.v2`) and an optional `[signals]` block projected from the sidecar's typed records. It is not the canonical schema. |

## Field Table

Canonical Rust definition: `ChunkMetadataSidecar` in
`crates/aicx-parser/src/chunker.rs:91`. The v2 additions below are quoted
against that struct; the constants live at `chunker.rs:172-194`.

| Field | Type | Required | Canonical value | Meaning | Struct ref |
|---|---:|---:|---|---|---|
| `schema_version` | `u32` | No | `2` for v2, default `1` when absent | Card sidecar schema version. Missing means legacy v1. Serialized only when ≠ 1. | `chunker.rs:98` |
| `migrated_from_schema` | `u32` | No | `1` on cards upgraded by `migrate --cards-v2` | Explicit marker that a v2 sidecar was born from legacy data. Validators use this marker to keep born-v2 strict while tolerating legacy-only gaps as warnings. | `chunker.rs:99` |
| `source` | `CardSource` | No | L0 pointer object | Provenance pointer to the raw session file. | `chunker.rs:197` |
| `source.path` | `string` | Yes within `source` | Raw session path | L0 source pointer for forensic recovery. | `chunker.rs:198` |
| `source.sha256` | `string` | No | SHA-256 of raw source | Optional integrity pointer for the L0 source. | `chunker.rs:200` |
| `source.span` | `[u64, u64]` | No | Start and end offsets or line bounds | Optional source span in the L0 file. The exact span unit must be documented by the writer that populates it. | `chunker.rs:202` |
| `claim_scope` | `string` | No | `session_close` | Claims describe what was true at session close. | `chunker.rs:173` (const) |
| `freshness_contract` | `string` | No | `historical` | Card claims are historical records, not live status probes. | `chunker.rs:174` (const) |
| `verification_state` | `string` | No | `not_verified_by_aicx` | aicx records the claim but has not independently verified it against current runtime truth. | `chunker.rs:175` (const) |
| `signals[].kind` | `string` | Yes when signal exists | One of the canonical kinds below | Signal category. | `chunker.rs:206` (`CardSignal`) |
| `signals[].text` | `string` | Yes when signal exists | Extracted text | Typed signal body. | `chunker.rs:206` |
| `signals[].line_span` | `[u64, u64]` | No | Source line bounds | Optional line span for the extracted signal. | `chunker.rs:206` |
| `signals[].extractor_version` | `string` | No | Writer stamps `env!("CARGO_PKG_VERSION")` | Version of the signal extractor that produced the record (`SIGNAL_EXTRACTOR_VERSION`, `chunker.rs:194`). | `chunker.rs:206` |

Existing sidecar fields remain valid and keep their current meaning:
`id`, `project`, `agent`, `date`, `session_id`, `cwd`, `kind`, `frame_kind`,
`content_sha256`, `run_id`, `prompt_id`, source import fields, tags, and
context-corpus metadata.

`content_sha256` is the SHA-256 of the full markdown card file as stored on
disk, including the header/frontmatter. It is not a body-only hash. A header
rewrite that preserves body bytes must still refresh `content_sha256`.

`schema_version` deserialization is tolerant: it accepts a JSON number or a
string, and strings may carry the prefixes `card.v`, `context_corpus.v`, or
`v` (`parse_schema_version_string`, `chunker.rs:267`). Serialization always
emits a plain number.

### Canonical signal kinds

One label per `ChunkSignals` family plus `highlight`. Consumers (validator,
migration, display) must match against these constants
(`chunker.rs:180-190`), never re-derive the strings:

`skill` · `todo_open` · `todo_done` · `ultrathink` · `insight` · `plan_mode` ·
`intent` · `decision` · `result` · `outcome` · `highlight`

## Honesty Semantics

Card claims are historical. They are valid at session close and never imply
live runtime truth.

The canonical honesty tuple for card v2 is:

| Field | Canonical value | Constant (`chunker.rs`) |
|---|---|---|
| `claim_scope` | `session_close` | `CARD_CLAIM_SCOPE_SESSION_CLOSE` (l. 173) |
| `freshness_contract` | `historical` | `CARD_FRESHNESS_CONTRACT_HISTORICAL` (l. 174) |
| `verification_state` | `not_verified_by_aicx` | `CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX` (l. 175) |

Readers and display surfaces must not present card claims as current repo
state, current runtime behavior, or current deployment truth unless a later
verifier has explicitly attached fresh evidence. The `aicx intents` and MCP
display surfaces render this as a claim-honesty frame
(`_claims: historical @ session close · not verified by aicx_`).

## Versioning Policy

Card schema evolution is additive.

- Missing `schema_version` deserializes as `1`.
- v1 sidecars remain readable forever.
- v2 adds typed fields; it does not remove or rename v1 fields.
- Unknown extra fields must not fail deserialization.
- The writer emits `schema_version: 2` since the A2 writer cut (`a37460f`);
  every new sidecar carries the honesty tuple and, when present, typed
  `signals`.
- The migration emits `migrated_from_schema: 1`; new writer-born v2 cards leave
  this field absent.
- Readers stay header-agnostic: both the legacy bracket header
  (`[project: … | agent: … | date: …]`) and the v2 YAML frontmatter are
  parsed structurally (shared helper in
  `crates/aicx-parser/src/card_header.rs`). Bracket-header fallback must be
  kept as long as un-migrated v1 cards can exist in a store.

## Runtime Surfaces (landed)

| Surface | Location | Behavior |
|---|---|---|
| Writer | `crates/aicx-parser/src/chunker.rs` (`format_chunk_text_inner`, l. 833; `From<&Chunk> for ChunkMetadataSidecar`, l. 281) | Emits `.md` with YAML frontmatter (`schema: card.v2`) + optional `[signals]` block, and a sidecar with `schema_version: 2`, honesty tuple, `source` L0 pointer, typed `signals`. |
| Readers | `crates/aicx-parser/src/card_header.rs`, `src/intents.rs::parse_chunk_document` | Header-agnostic (bracket or frontmatter); sidecar-first metadata selection. |
| Validator | `src/corpus/validate.rs` (`aicx corpus validate-cards`) | Accepts `schema_version` 1 or 2; on born-v2 checks honesty values, `source.path`, header shape, full-file `content_sha256`, and md↔sidecar signal parity as hard errors. On migrated-v2 (`migrated_from_schema` present), missing legacy `source.path` becomes `migrated_missing_source` warning and unbackfilled markdown signals become `migrated_signals_unbackfilled` warning. Hash mismatch stays an error for all cards. |
| Migration | `src/store/migration/cards_v2.rs` (`aicx migrate --cards-v2`) | In-place v1→v2 upgrade: sidecar gains schema/honesty fields plus `migrated_from_schema: 1`, bracket header → YAML frontmatter, and refreshed full-file `content_sha256` with old/new hashes recorded in the manifest. Body bytes never change; dry-run by default, `--apply` to write; idempotent. |

## Decision Log

| Date | Decision | Provenance |
|---|---|---|
| 2026-07-02 | Card schema v2 type level: versioned sidecar, `CardSource` L0 pointer, honesty fields, `CardSignal`; contract v1 authored (cut A1). | plan `~/.vibecrafted/artifacts/Loctree/aicx/2026_0702/plans/cards-v2/SCAFFOLD.md` · commit `840af2a` |
| 2026-07-02 | Readers made header-agnostic; sidecar-first metadata (cut C1). | same plan · commit `5d89472` |
| 2026-07-02 | Claim-honesty frame rendered on `intents` display + MCP surfaces (cut B2). | same plan · commit `d8ea714` |
| 2026-07-02 | Writer flipped to v2: YAML frontmatter + L0 provenance sidecar (cut A2). | same plan · commit `a37460f` |
| 2026-07-02 | Typed `CardSignal` records in sidecar; `[signals]` block rendered from records; canonical kind vocabulary (cut B1). | same plan · commit `3140fe6` |
| 2026-07-02 | `corpus validate-cards` contract gate for v1/v2 (cut C2). | same plan · commit `fe59416` (content originally in a peer envelope; re-committed after a Living Tree reset) |
| 2026-07-02 | Store migration v1→v2 with body-hash invariant + CLI arm `migrate --cards-v2` (dry-run default, `--apply` gate) (cut D1). | same plan · commits `2d2b12d`, `8ca7a6b` |
| 2026-07-02 | Contract finalized against landed runtime; A1 "Cut Boundary" section retired (cut D2). | same plan · this commit |
| 2026-07-02 | Migration and validation reconciled with explicit `migrated_from_schema` marker, full-file hash refresh, and born-vs-migrated validation policy (cut D1b). | settlement review · this commit |

## Open / not landed

Explicitly future or known-broken; nothing here is implied by the sections
above.

- **Custom-root migration is blocked by a pre-existing sanitize allowlist
  bug**: `aicx migrate --cards-v2 <ROOT>` with a non-default root (and any
  custom `AICX_HOME`) fails with "Cannot read from path outside allowed
  directories". Works against the default `~/.aicx` store. Control experiment
  proved this pre-dates the cards-v2 line (found during A2/D1 verification).
- **Legacy migration warning debt is real and large**: settlement review counted
  about `10,033` source-less v1 cards and about `2,089` cards whose markdown
  `[signals]` blocks cannot be represented in legacy sidecars without a
  re-extraction/backfill cut. After D1b, migrated cards surface these as
  `migrated_missing_source` and `migrated_signals_unbackfilled` warnings, not
  hard validation failures. The earlier `~35` note only covered the A2→B1
  born-v2 gap and was not the full migration debt.
- **Signal re-extraction policy** is not defined: `extractor_version` is
  stamped, but no cut re-extracts signals from older cards yet.
- Retiring the bracket-header read fallback would require a migration
  contract proving no v1 cards remain; no such cut is planned.
