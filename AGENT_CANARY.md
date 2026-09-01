---
title: "aicx-canary: Truth-competition census of the memory engine."
author: "Maciej & Claude"
version: "0.1.0 (2026-08-26)"
description: "Repo-specific seed spec for the vc-canary v2.1 truth-competition radar over aicx: decision axes, measured evidence, and the census contract."
session_id: 76a219e6-7c55-4b91-b3c1-c8ec1acdf216
summary: "One night of live probing (2026-08-25/26) showed aicx competing with itself for several classes of truth: five adapters each privately deciding what human speech is, three storage layouts claiming to be THE dense index, session identity resolvable to a different session than asked, empty results shaped like success, and a phantom artifact org. This file freezes those axes as census seeds."
reference_protocol: vibecrafted-core/vibecrafted_core/skills/vc-canary (v2.1.0)
reference_mission: ~/.vibecrafted/artifacts/Loctree/aicx/2026_0826/plans/aicx-one-taxonomy-fusion-260826/SCAFFOLD.md
evidence_journal: ~/.vibecrafted/artifacts/Loctree/aicx/2026_0825/extract-human-shape/JOURNAL.md
---

# AICX CANARY — census charter

## Why this file exists

aicx is the memory engine of the whole stack — it feeds extract, intents,
continuity, recall and the loctree overlay. A memory engine that competes with
itself for truth does not merely have bugs: it **distills noise into every
consumer at once**. On 2026-08-25/26 we measured that competition live, without
looking for it. This file freezes what we saw as **census seeds** for a proper
`vc-canary` v2.1 run (mutation-free, one-instrument, machine absence proofs,
verdicts per pair, dispositions per row — the protocol lives in the skill, not
here).

This is a SEED SPEC, not a report. Starting hypotheses, not the boundary.
The census may dissolve axes (FALSE_PARALLEL) or discover new ones.

## Relationship to the active mission

Mission `aicx-one-taxonomy-fusion-260826` (compile-embargo doctrine) already
owns the REPAIR of axis 1 and parts of 3/5. This canary is its **W0-T3 organ**:
the anatomical census that must close before W1 shape work. Axes 2, 4, 6, 7
are diagnosis-only here — their cuts belong to later missions. Canary never
implements; findings feed thrones, thrones feed cuts, cuts get their own
vc-trust.

## Decision axes (census seeds, with measured evidence)

### Axis 1 — Speech truth: "what is a human utterance?" 🔥

Five adapters each privately decide what human speech is:
`crates/aicx-parser/src/adapters/{codex,claude,gemini,grok,junie}.rs`,
with additional reducers in `sanitize.rs`, `segmentation.rs`, `chunker.rs`,
`noise.rs`.

Measured 2026-08-25:
- codex `--conversation --human` lost 9/34 operator utterances (echo-seal
  channel in `<user_shell_command>`); 31.2% control-envelope leakage before
  cb17d5a;
- claude hides ~14/20 mid-turn steers in `queue-operation` frames (enqueue
  timestamp = real seal) — invisible to extraction;
- codex fork replays ~30 parent messages stamped with ONE fork-moment
  timestamp (replay ≠ utterance; lineage physics, session 01a03595);
- gemini adapter refuses whole sessions with `Fatal completeness` in bulk
  (diagnostics 2026-08-25: skips spanning 2025-11→2026-04).

Census question: every site that decides message admission/classification,
with references (not definitions), and one verdict per competing pair.

### Axis 2 — Dense/vector truth: "where is THE semantic index?" 🔥

Three layouts claim it simultaneously:
- canonical `<AICX_HOME>/indexed/<bucket>/embeddings.ndjson` (build source;
  `search --deep` requires it);
- generation layout `hybrid/generations/<g>/dense.exact_mmap_v1.bin` +
  `manifest.json` + `CURRENT` pointer;
- legacy residue: `~/index/_all/embeddings.ndjson` (OUTSIDE AICX_HOME —
  found live by the `--deep` probe) and the documented 15 GB + 15 GB
  `dense_brute_force.ndjson` twin (docs/EMBEDDINGS.md:230-234).

Measured consequence: "search --deep does not work despite having
embeddings" (Monika's report) — embeddings in the wrong layer/path.
The product has no working semantic mouth while the build machinery is
excellent (blake3-bound manifests, atomic pointer flip, GPU embedder).

### Axis 3 — Session identity truth: "which session am I?" 🔥

- `src/session_catalog.rs::resolve_from_sources` (`MatchKind::ExactAlias`,
  line ~682) can silently substitute a different session than requested;
- `src/catalog.rs::resolve_session` + `src/mcp_session.rs::resolve_session`
  — parallel resolution surfaces;
- 2026-08-26 multi-head discovery (codex session 01a03595 on dragon): one
  provider `session_id`, TWO live heads with divergent context state —
  provider session_id is NOT a valid identity or lock. Target identity is
  the (store-id, content-hash) pair; `forked_from_id` is a first-class
  lineage edge.

### Axis 4 — Artifact-store truth: "where do aicx artifacts live?" ⚠

Phantom org bucket: agents (and the vibecrafted launcher, in opposite
directions) guessed the org from the workspace path, producing
`artifacts/vetcoders/aicx/` beside canonical `artifacts/Loctree/aicx/`.
Consolidated 2026-08-26 (merge + symlink), but the RESOLVER that guesses
org from path still exists — census its call sites. Repo `vetcoders/aicx`
does not exist; aicx belongs to org Loctree.

### Axis 5 — Success/refusal truth: "did I actually answer?" 🔥

- silent-empty: extract paths returning Ok with an empty conversation
  (degenerate grok specimen 019fdeca: 2-line session → quiet emptiness,
  vs TB's loud typed refusal with evidence);
- fallback theatre: `search --deep` without an index prints an honest
  typed refusal (`kind: index_not_built`) and THEN returns
  "No matches (scanned 0 chunks)" — emptiness shaped like a result;
- contrast (healthy throne to protect): typed `RefusalReason`-style
  contracts and the detect-with-evidence gate planned in
  engine/{coverage,identity,validate}.rs.

### Axis 6 — Coverage/accounting truth: "what did I NOT consume?" ⚠

- 722 chunks pending/unindexed at census time (claude=389, codex=112,
  gemini=83, vc=98, junie=35, grok=5) — consistent between `index status`
  and search fallback, but invisible to consumers of results;
- catalog↔chunks lag (sessions newer than chunks) reported honestly in
  status yet not propagated to answer surfaces;
- TB contract to adopt: total accounting (`consumed_by_kind`,
  `known_skipped`, zero silent holes).

### Axis 7 — Redaction truth: "which layer kills a secret?" ⚠

Redact-by-default lives at the OUTPUT throne
(`src/output/conversation.rs::redact_conversation_messages`, tests:
`write_conversation_outputs_redact_by_default`, pipeline redacts once
before dedup). Raw substrate (provider rollouts/jsonl) is uncovered by
design — and now travels between machines via the transcript git sync.
Boundary is censused, not condemned: the v3 design (paste-interceptor at
input + `aicx redact --in-place` watcher at rest) lives in the evidence
journal, 2026-08-26 entries.

### Axis 8 — Epistemic competitors ◌

`doctor/checks.rs`, `validate.rs`, `noise_smoke.rs`, `oracle_envelope.rs`,
adversarial tests — judge similar truth without writing daily runtime.
Census must prove their non-runtime boundary rather than assume it.

## Census contract

Run per vc-canary v2.1 (the skill is the protocol authority):

- **Mutation-free.** No code edits, no cargo/make/lint — this repo is also
  under the mission's compile embargo (W0–W2). Double reason.
- **One instrument.** Loctree organs only; grep forbidden as inventory or
  absence evidence; gaps → `.loctree/loctree-fail.md` + `UNRESOLVED`.
- **References, not definitions.** A definition census once hid 141 call
  sites (codescribe W0 lesson).
- **Machine absence proofs.** `offset==0 · emitted==total ·
  truncated==false · scan_complete==true` + pinned snapshot fingerprint.
- **Verdict per pair** (SAME_SOURCE_OF_TRUTH / INTENTIONAL_VARIANT /
  DRIFTED_DUPLICATE / BYPASS_PATH / FALSE_PARALLEL), legend 🔥/⚠/◌,
  disposition per row (authority_edge / proven_non_runtime /
  obsolete_residue / UNRESOLVED).
- **Journal.** Append-only `./.loctree/canary/JOURNAL.md` in this repo.
- **Run verdict.** AXES_CLOSED_CANDIDATE / AXES_OPEN /
  INSTRUMENT_INCOMPLETE / LAUNCHER_CONTRACT_CONFLICT; report ends with
  `BUILD/LINT/TEST/RUNTIME=NOT_ASSESSED`.

## What the census must NOT do

- Propose thrones or refactors inside the run (evidence first; thrones are
  decided in the mission, cuts after QC).
- Treat every multi-authority as a defect — runtime/replay and
  runtime/test splits may be INTENTIONAL_VARIANT with a proven boundary.
- Quote counts without the pinned fingerprint, or reconstruct any missing
  historical checkpoint (record `MISSING`).

## Standing context

- Living Tree: the working tree may carry an uncommitted echo-seal cut from
  worker work-260825-223415-07065 (adjudication = mission W0-T1). Re-read
  before assuming tree state. Do not sweep, do not revert.
- Alarm doctrine (operator, 2026-08-26): report EVERY security-relevant
  observation in passing, no severity threshold — innocent-looking +
  execution-context change is the class that already ended in an attack.
