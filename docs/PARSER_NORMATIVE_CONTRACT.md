# AICX Parser Normative Contract (v1)

Status: **frozen** (cut C0A of `aicx-parser-transplant-v1`).
Consumers: C1 (engine kernel), C1L (session catalog), C2/C2G/C3/C4/C5 (agent
adapters), C6 (store projection), C7 (CLI/projections), C7H (compact-recall
consumer), and — cross-plan — `substrate-makieta v4` (C0-01, A1, A2).

This document is the smallest complete contract the Rust kernel and every
adapter must implement. It is the checkpoint C1 must intake **before** freezing
`SessionModel` or `AgentAdapter`. Machine-readable truth lives next to it:

- `tests/parser_oracle/normative_fields.toml` — donor field ownership matrix
- `tests/parser_oracle/raw_unit_taxonomy.toml` — per-agent raw-unit taxonomy
- `tests/parser_oracle/contract_check.py` — the executable contract gate
- `tests/fixtures/parser_engine/contract/` — identity / status / usage fixtures

Gate (run from repo root):

```bash
python3 tests/parser_oracle/contract_check.py --self-test
python3 tests/parser_oracle/contract_check.py \
  --fields tests/parser_oracle/normative_fields.toml \
  --taxonomy tests/parser_oracle/raw_unit_taxonomy.toml
```

---

## 1. Donor is oracle, never schema

Transcript Builder (the donor checkout, `session_record.v1` schema `1.2.0`) is
the **behavioral oracle**: it proves classify → assemble → validate with
explicit raw-unit coverage. It is **not** the production schema. AICX does not
copy `session_record.v1`; it classifies every donor field into exactly one of
four classes and implements only what is parser truth.

### 1.1 Field classes

| Class | Meaning | Kernel fingerprint |
|---|---|---|
| `normative` | Deterministic raw-source truth the kernel MUST reproduce byte-stably. | allowed |
| `heuristic_projection` | Donor computes it with heuristics (regex slicing, signal scoring, verdict guessing). Not parser truth; owned by later layers (projections, A1 intent overlay). | **forbidden** |
| `diagnostic` | Parser observability/telemetry (counters, samples, generator provenance, wall-clock stamps). May be emitted, never fingerprinted. | **forbidden** |
| `out_of_scope` | Donor-only product surface AICX does not adopt (preview caps, `map_id` layout, deprecated aliases, TB verification axis). | **forbidden** |

Every donor field is classified in `normative_fields.toml` with a `reason` and
a target `owner`. The contract checker rejects unclassified donor fields and
rejects any non-`normative` field marked for the kernel fingerprint.

### 1.2 Canonical fingerprint rule

The kernel's canonical serialization/fingerprint (C1 `canonical.rs`) may
contain **only** fields classified `normative`. No wall clock, no generator
version, no heuristic output, no absolute paths. For identical source bytes,
adapter version, and explicit configuration, canonical model bytes are
identical.

---

## 2. Raw-unit taxonomy (physical vs logical)

Every agent format decomposes into **raw units**. A *physical unit* is the
smallest independently-addressable region of the source (a JSONL line, a
whole-file document). A *logical unit* is a nested, adapter-declared structure
inside a physical unit (a Claude content block, a Codex `response_item`
payload, a Gemini `messages[i]` entry, a Junie `(stepId, kind)` block stream).

The full taxonomy — containers, physical units, logical units, match rules,
and provenance grade (`donor_spec` / `donor_adapter` / `aicx_history` /
`recon`) — is frozen in `tests/parser_oracle/raw_unit_taxonomy.toml`:

| Agent | Container | Physical unit | Logical units |
|---|---|---|---|
| Codex | single JSONL file | line (`session_meta`, `turn_context`, `event_msg`, `response_item`) | `response_item.payload.type`: `message`, `function_call`, `function_call_output`, `reasoning`, `encrypted_reasoning`; `event_msg` usage snapshot (`token_count`) |
| Claude | single JSONL file | line (`user`, `assistant`, `system`, `summary`, `attachment`, `file-history-snapshot`, `pr-link`, `queue-operation`, `ai-title`, `mode`, `permission-mode`, `last-prompt`) | `message.content[]` blocks: `text`, `tool_use`, `tool_result`, `thinking`; `message.usage` (delta usage) |
| Gemini CLI | dual: whole-file JSON **or** JSONL stream | whole-file: the document (line numbers substituted); JSONL: line (`header`, `message`, `$set` heartbeat) | `messages[i]` (`user`, `gemini`, `error`, `info`); nested `thoughts[]`; nested `toolCalls[]` (inline result) |
| Gemini Antigravity | brain-state directory | opaque `.pb` (never parsed as plaintext) | recovered `conversation_artifact`; `step_output_fallback` decision stream |
| Grok | session directory | `chat_history.jsonl` line (`user`, `assistant`, `reasoning`, `tool_result`, `system`, `backend_tool_call`); `summary.json` whole-file; `hunk_records.jsonl` line | assistant fan-out: message text, inline `reasoning`, `tool_calls[]` (JSON-string args); reasoning record: `summary[]` text or `encrypted_content` (opaque) |
| Junie | session directory (`session-*/events.jsonl`) | line (`UserPromptEvent`, `UserResponseEvent`, `SystemMessageEvent`, `CurrentDirectoryUpdatedEvent`, `*BlockUpdatedEvent` family) | `(stepId, kind)` streaming snapshots — logical unit is the **last** snapshot per `(stepId, kind)`; earlier `IN_PROGRESS` snapshots are consumed as stream updates |

Accounting invariant (inherited from the donor, kept normative): every raw
unit terminates in exactly one state — `consumed(kind)` XOR `skipped(reason)`.
Unknown unit shapes are **data** (skipped with a typed reason and a warning),
never silence. Every fixture unit must terminate in exactly one declared
taxonomy kind; the checker enforces unique match.

---

## 3. Parse status — an orthogonal contract

Donor `parse_status` (`clean | complete_with_warnings | partial | failed`)
blends two axes. The AICX contract splits them:

### 3.1 Visible completeness (exactly one)

| Value | Meaning |
|---|---|
| `complete_visible` | Every supported **visible** event is accounted for. |
| `partial_visible` | ≥1 visible event was malformed, truncated, or lost. |
| `fatal` | No valid model may be projected or ingested. |

### 3.2 Boundary flags (orthogonal, any combination)

| Flag | Meaning |
|---|---|
| `opaque_reasoning_present` | Encrypted/private reasoning exists in the source. **Alone it never degrades visible completeness** — a modern Codex session with encrypted reasoning is `complete_visible + opaque_reasoning_present`. |
| `unsupported_visible_event` | A visible unit was preserved as unsupported; requires an explicit warning/boundary record. Preservation is not loss: it does not by itself force `partial_visible`. |

### 3.3 Truth-table rules (checker-enforced)

1. `malformed_tail_present` ⇒ visible completeness is `partial_visible` or `fatal`, never `complete_visible`.
2. `partial_visible` requires concrete visible loss (`malformed_tail_present` or `visible_event_lost`). Opaque reasoning alone can never justify it.
3. `unsupported_visible_event` ⇒ `warnings_count ≥ 1` (the boundary must be recorded).
4. `fatal` ⇒ no model is projected or ingested (`model_projected = false`); partial/malformed **tails** do not erase earlier valid units in non-fatal states.
5. Donor mapping: `clean`/`complete_with_warnings` → `complete_visible` (flags carry the warnings axis); `partial` → `partial_visible`; `failed` → `fatal`. The donor enum is not re-emitted.

Fixture truth table: `tests/fixtures/parser_engine/contract/parse_status_truth_table.toml`.
Contradictory states are rejected by the checker.

---

## 4. UsageEvent — first-class, typed

The donor does **not** model usage telemetry; `UsageEvent` is an AICX
normative extension. Every adapter that can see usage data emits typed
`UsageEvent` records in the `SessionModel`:

| Field | Type | Notes |
|---|---|---|
| `provider` | string | e.g. `openai`, `anthropic`, `google`, `xai` |
| `model` | string \| unknown | per-event provenance; model drift within one session is legal and preserved per event |
| `tokens.input` | u64 \| unknown | |
| `tokens.output` | u64 \| unknown | |
| `tokens.reasoning` | u64 \| unknown | |
| `tokens.cache_read` | u64 \| unknown | |
| `tokens.cache_creation` | u64 \| unknown | |
| `cost` | { amount ≥ 0, currency } \| unknown | reported cost only; never computed by the parser |
| `timestamp` / `span` | RFC3339 / [start, end] \| unknown | |
| `counter_semantics` | `snapshot` \| `delta` \| `cumulative` | **required** — e.g. Codex `token_count` events are `cumulative` session counters; Claude `message.usage` is `delta` per API call; Gemini per-message token fields are `snapshot` |

**Unknown stays unknown, never zero.** A source that does not report a
component yields `unknown` for that component; fabricating `0` is a contract
violation. The fixture matrix (`usage_matrix.toml`) covers cumulative
counters, cache tokens, missing cost, and mid-session model drift, plus
invalid events the checker must reject.

---

## 5. `evidence_event_id` — stable source-event identity (derivation v1, frozen)

Every classified raw unit carries an `evidence_event_id`. This is the
producer-side identity later consumed by `substrate-makieta`'s intent overlay
(`refs[].evidence_event_id`).

### 5.1 Derivation

```text
unit_content_hash = sha256(raw_unit_bytes)
  raw_unit_bytes  = exact bytes of the physical unit without trailing EOL,
                    or the canonical JSON serialization of a logical unit

evidence_event_id = "ev1:" + agent + ":" + session_id + ":" + locator
                    + ":" + unit_kind + ":" + hex16(unit_content_hash)

locator = zero-padded decimal physical ordinal ("%06d", 1-based), when the
          format is append-only line-oriented;
        = adapter-declared logical locator token, when no stable physical
          ordinal exists (e.g. Junie "step:<stepId>:<kind>",
          Gemini whole-file "msg:<index>", Antigravity "artifact:<name>")
```

### 5.2 Invariants (fixture-verified)

1. **Append-stable**: appending raw units preserves every prior id byte-for-byte.
2. **Relocation-stable**: the derivation takes no filesystem path; moving the source file changes no id.
3. **Content-scoped mutation**: mutating one raw unit changes only that unit's id.
4. **No absolute paths**: no id may embed an absolute path, and the mutable whole-file hash is never the sole identity.
5. **Uniqueness**: duplicate ids within a session are a fatal accounting violation.

Not guaranteed (explicitly): stability under mid-file insertion or reordering —
agent session logs are append-only; a rewritten history is a new source truth
(`original_source_hash` changes) and re-ingest is correct behavior.

Fixtures: `identity_base.jsonl` → `identity_append.jsonl` (append) and
`identity_mutated.jsonl` (single-unit mutation) under
`tests/fixtures/parser_engine/contract/`.

---

## 6. Cross-plan handoff (`substrate-makieta v4`)

Exact ownership — no party may emit another party's identity:

| Identity | Producer | Consumers | Rule |
|---|---|---|---|
| `evidence_event_id` | **parser (this contract, C1–C5)** | C6 store cards, A1 `refs[]` | derivation v1 above; frozen here |
| `store_revision` | **C6 store projection** | A1, Loctree overlay | describes the canonical source corpus only; **attribution algorithm versions can never alter `store_revision`** |
| `intent_id` | **A1** | overlay consumers | persistent cluster identity, assigned at first cluster creation, never recomputed from content |
| `content_hash` | **A1/A2** | overlay consumers | hash of the current intent distillate; changes on merge/append |
| `overlay_revision` | **A1** | Loctree | changes with store_revision, anchor catalog, or attribution/dedup/model/threshold version |

The parser must not invent `intent_id`, `content_hash`, or `overlay_revision`.
A1-01/A1-02 intake this contract's frozen `evidence_event_id` derivation; they
begin only after the transplant audits are green.

---

## 7. Direct-file extraction — bounded consumer contract

Direct-file extraction (`aicx extract <agent> --file <path> --conversation`)
is a **first-class bounded consumer contract**, the surface used by the shared
compact-recall hook (C7H migrates `aicx-compact-recall@personal` onto it).

Invariants:

1. Once a path is supplied, the engine **may not discover or scan the corpus**:
   no catalog walk, no sibling globbing, no store read. Work is proportional to
   the supplied file's bytes only.
2. The path is validated (open/path-safety, max-unit-size) and parsed as
   exactly one `SourceHandle`.
3. Extraction is a pure read/projection: no store, index, or cache mutation.
4. Output is deterministic for identical bytes + adapter version + config.

---

## 8. C1 intake instruction

C1 (engine kernel) must:

1. Intake **this exact contract commit** before freezing `SessionModel` or the
   sealed `AgentAdapter` trait.
2. Build the canonical fingerprint from `normative` fields only
   (`normative_fields.toml` is the machine truth; the checker is the gate).
3. Implement visible completeness + boundary flags (§3) instead of the donor
   `parse_status` enum, with the §3.3 truth table as validator rules.
4. Carry typed `UsageEvent` (§4) and `evidence_event_id` derivation v1 (§5)
   in the model; add a Rust `normative_contract` test in `aicx-parser` that
   consumes these TOML/fixture files so `cargo test -p aicx-parser
   normative_contract` stops being vacuous.
5. Keep heuristic donor surfaces (intent/title/outcome/dispatch signals) out
   of the kernel; they belong to projections and the A1 overlay.
