# Parser replacement engine contract

Status: harness contract. Normative field ownership is imported from
[`PARSER_NORMATIVE_CONTRACT.md`](PARSER_NORMATIVE_CONTRACT.md) and
`tests/parser_oracle/normative_fields.toml`; this document does not redefine
that matrix. C0 may be committed before C0A, but the oracle cut is not verified
until the exact C0A commit is recorded and its matrix passes the comparator.

## Ownership and source of truth

- C0A owns field classification, raw-unit taxonomy, parse-status semantics,
  usage semantics, and `evidence_event_id`.
- C0 owns fixtures, differential comparison, performance measurement, changed
  file enforcement, and the legacy-deletion guard.
- Transcript Builder is a development oracle. It is neither a runtime
  dependency nor AICX's production schema.
- Heuristic donor fields such as intent summaries, decision candidates,
  outcomes, titles, and deliverables never enter the canonical parser
  fingerprint merely because Transcript Builder emits them.

## Replacement flow

```text
SessionRef -> SessionCatalog/direct locator -> SourceHandle
  -> bounded RawUnitReader -> AgentAdapter classify -> AgentAdapter assemble
  -> SessionModel + CoverageReport + UsageEvent[]
  -> invariant validator -> deterministic projections
  -> AICX canonical store -> IntentRecord/search/steer -> Loctree
```

Resolution happens before parsing. A direct file handle forbids corpus
discovery. A session-id lookup may inspect bounded catalog metadata, but no
stage may parse all sessions and then filter one.

## Raw-unit accounting

Every declared physical or adapter-specific logical raw unit terminates in
exactly one state:

```text
consumed(kind) XOR skipped(reason)
```

The consumed and skipped sets are disjoint and their union equals every raw
unit in the selected source. Recognized non-conversational metadata is
consumed, not silently dropped. Unknown, malformed, oversized, encrypted, or
unsupported units remain explicit evidence with stable ordinals and byte
counts. A violated partition is fatal and produces no ingestible model.

## Parse status and boundaries

Visible completeness is orthogonal to inaccessible or unsupported boundaries:

- `complete_visible`: all supported visible events are accounted for.
- `partial_visible`: supported visible content is known to be lost or malformed.
- `fatal`: no projection or ingest may proceed.
- `opaque_reasoning_present`: private/encrypted reasoning exists; by itself it
  does not make visible conversation partial.
- `unsupported_visible_event`: a visible raw unit is preserved as unsupported
  and requires a warning/boundary.

The exact truth table belongs to C0A. The differential harness compares it as
normative data rather than translating it into Transcript Builder's historical
`clean|complete_with_warnings|partial|failed` axis.

## Usage and evidence identity

Typed usage events preserve provider and model provenance, input/output/
reasoning/cache token components, reported cost and currency, timestamp or
span, and `snapshot|delta|cumulative` semantics. Missing values remain unknown,
never fabricated as zero.

The parser emits `evidence_event_id` for source events. C6 later emits
`store_revision`; the downstream A1 line owns persistent `intent_id`, mutable
`content_hash`, and `overlay_revision`. Attribution changes cannot mutate
`store_revision`. Event identity cannot embed absolute paths or use a mutable
whole-file hash as its only input.

## Determinism

Identical source bytes, adapter version, and explicit configuration produce
byte-identical canonical model bytes, ordering, warnings, samples, usage, and
fingerprints. Canonical truth excludes wall clock, process environment,
network, repository HEAD, unrelated filesystem state, and output directory.
Two consecutive runs must compare exactly on every C0A normative field.

## Differential oracle policy

`tests/parser_oracle/manifest.toml` names each agent fixture, expected envelope,
the exact donor command when supported, normative exact paths, and heuristic
semantic assertions. Codex, Claude, Gemini, and Grok use Transcript Builder as
the donor oracle. Junie uses Rust-native goldens because the donor has no Junie
adapter.

Exact fields fail with a JSON-style field path. Heuristic assertions are
evaluated by declared predicates (`nonempty`, `contains_any`, or `equals`) and
are never snapshot-equal by accident. Golden updates require a reviewed field
classification change or verified oracle evidence; visual approval is not a
gate.

## Public Loctree boundary

The compatibility boundary is AICX's in-process read surface used by Loctree:
`aicx::api::Aicx`, `IntentsConfig`, `IntentRecord`, canonical store readability,
and the `loctree-consumer` feature. Provider modules, recursive walkers,
TimelineEntry-first normalization, and current CLI plumbing are not public
compatibility surfaces.

## Benchmark contract

`tools/bench_single_session.sh` emits one JSON document with two consecutive
runs and separate locate, parse, and projection timings. It also reports the
number and bytes of selected source inputs opened by the measured contract.
Missing, negative, non-finite, or structurally absent metrics are a hard
failure. The private 52.8 MB rollout is read in place and is never copied into
the repository.

## Legacy deletion contract

`tools/legacy_parser_manifest_v1.toml` freezes paths and implementation symbols
removed by `b1f2712`, plus temporary facade/boundary paths and their current
consumers. `tools/verify_no_legacy_parser.py` proves deleted paths are absent at
the S0 baseline and live tree, and rejects resurrection of deleted symbols or
fallback bodies. Temporary facade symbols are explicitly allowed only inside
the sealed fail-closed boundary until their declared removal cut.
