---
contract_version: 1
status: active
owner: Vetcoders
last_reviewed: 2026-07-02
revalidate_by: 2026-08-02
---

# AICX Card Contract

## Purpose

This contract defines the aicx card schema v2 at the type level. A card is the
pair of a chunk `.md` file and its `.meta.json` sidecar. In v2, the sidecar is
the structured L1 truth and the `.md` file is a deterministic human projection.

This cut is additive. Existing v1 sidecars remain readable forever.

## Layer Model

| Layer | Surface | Contract |
|---|---|---|
| L0 | Raw source session file | Immutable source material. The card sidecar may point back to it with `source.path`, `source.sha256`, and `source.span`; the raw payload is not copied into the pointer. |
| L1 | `.meta.json` sidecar | Machine truth for card identity, provenance, schema version, claim honesty, typed signals, and content hash. Readers should prefer this surface when a structured field exists. |
| L2 | Chunk `.md` | Human projection for reading and retrieval context. It may render headers and signal prose, but it is not the canonical schema. |

## Field Table

| Field | Type | Required | Canonical value | Meaning |
|---|---:|---:|---|---|
| `schema_version` | `u32` | No | `2` for v2, default `1` when absent | Card sidecar schema version. Missing means legacy v1. |
| `source.path` | `string` | No | Raw session path | L0 source pointer for forensic recovery. |
| `source.sha256` | `string` | No | SHA-256 of raw source | Optional integrity pointer for the L0 source. |
| `source.span` | `[u64, u64]` | No | Start and end offsets or line bounds | Optional source span in the L0 file. The exact span unit must be documented by the writer that populates it. |
| `claim_scope` | `string` | No | `session_close` | Claims describe what was true at session close. |
| `freshness_contract` | `string` | No | `historical` | Card claims are historical records, not live status probes. |
| `verification_state` | `string` | No | `not_verified_by_aicx` | aicx records the claim but has not independently verified it against current runtime truth. |
| `signals[].kind` | `string` | Yes when signal exists | Extractor-defined | Signal category such as decision, outcome, intent, or result. |
| `signals[].text` | `string` | Yes when signal exists | Extracted text | Typed signal body. |
| `signals[].line_span` | `[u64, u64]` | No | Source line bounds | Optional line span for the extracted signal. |
| `signals[].extractor_version` | `string` | No | Extractor identifier | Version or name of the signal extractor that produced the record. |

Existing sidecar fields remain valid and keep their current meaning:
`id`, `project`, `agent`, `date`, `session_id`, `cwd`, `kind`, `frame_kind`,
`content_sha256`, `run_id`, `prompt_id`, source import fields, tags, and
context-corpus metadata.

## Honesty Semantics

Card claims are historical. They are valid at session close and never imply live
runtime truth.

The canonical honesty tuple for card v2 is:

| Field | Canonical value |
|---|---|
| `claim_scope` | `session_close` |
| `freshness_contract` | `historical` |
| `verification_state` | `not_verified_by_aicx` |

Readers and display surfaces must not present card claims as current repo state,
current runtime behavior, or current deployment truth unless a later verifier
has explicitly attached fresh evidence.

## Versioning Policy

Card schema evolution is additive.

- Missing `schema_version` deserializes as `1`.
- v1 sidecars remain readable forever.
- v2 adds typed fields; it does not remove or rename v1 fields.
- Unknown extra fields must not fail deserialization.
- Writers may start emitting `schema_version: 2` only in the writer cut that
  intentionally upgrades card output.
- Readers must keep bracket-header and legacy sidecar fallbacks unless a later
  migration contract explicitly proves they are unnecessary.

## Cut Boundary

A1 defines types and this contract only. It does not populate `source`,
honesty fields, or typed `signals`; it does not change `.md` rendering or reader
selection behavior. Population, writer changes, reader changes, validator work,
and migration are later cuts.
