---
title: "aicx-canary: Truth-competition census of the memory engine."
author: "Maciej & Claude; refresh Maciej & Cursor"
version: "0.2.0 (2026-09-17)"
description: "Repo-specific seed spec for the vc-canary v2.1 truth-competition radar over aicx. v0.1 froze the 2026-08-25/26 axes; v0.2 records what 0.13.0/0.13.1 closed, what still competes, and the new extract/search-read axes measured 2026-09-17."
session_id: 8f8472c3-a928-4276-a23f-66216c5d11d2
prior_session_id: 76a219e6-7c55-4b91-b3c1-c8ec1acdf216
summary: "August 2026: five adapters privately deciding human speech, three dense-index layouts, silent session substitution, empty-shaped success, phantom artifact org. September 2026: one throne classify exists (six adapters + engine::frames::classify, 14 where-symbol sites on 426/426 files) but --conversation still drops classified ShellAction; Claude Bash is hashed to $ $; search paths cannot be read; dense_count is 0. This file remains a seed spec, not a census report."
reference_protocol: vibecrafted-core/vibecrafted_core/skills/vc-canary (v2.1.0)
reference_mission_closed: ~/.vibecrafted/artifacts/Loctree/aicx/2026_0826/plans/aicx-one-taxonomy-fusion-260826/SCAFFOLD.md
reference_mission_open: ~/.vibecrafted/artifacts/Loctree/aicx/2026_0917/plans/aicx-0131-fail-gaps/SCAFFOLD.md
evidence_journal: ~/.vibecrafted/artifacts/Loctree/aicx/2026_0825/extract-human-shape/JOURNAL.md
fail_log: ~/.vibecrafted/aicx/aicx-fail.md
snapshot: feat/index-single-command@05bb14e
---

# AICX CANARY — census charter

## Why this file exists

aicx is the memory engine of the whole stack — it feeds extract, intents,
continuity, recall and the loctree overlay. A memory engine that competes with
itself for truth does not merely have bugs: it **distills noise into every
consumer at once**.

On 2026-08-25/26 we measured that competition live, without looking for it.
On 2026-09-17 we re-probed the same classes of truth on installed
`aicx 0.13.1+g97ac6cc6` against checkout `feat/index-single-command@05bb14e`
(426 files, 194330 LOC, loctree 0.14.4+g3e9eb0a7). The August axes did not
all vanish. Some gained a throne and a bypass. Two new axes showed up in
the fail-log: tool-io rendering, and discover→read.

This is a SEED SPEC, not a report. Starting hypotheses, not the boundary.
The census may dissolve axes (FALSE_PARALLEL) or discover new ones.

## Relationship to active missions

- Mission `aicx-one-taxonomy-fusion-260827` **shipped in 0.13.0** (CHANGELOG
  2026-09-01): one `engine::frames::classify`, seven frame classes, typed
  refusals at projection seams, `PackageIdentity` = (store id, source
  hash). That mission owned the REPAIR of August axis 1 and parts of 3/5.
  It is no longer the compile-embargo reason to skip a census — the
  embargo is closed. Canary stays mutation-free **by protocol**, not by
  embargo.
- Mission `aicx-0131-fail-gaps` (2026-09-17) owns the REPAIR of the live
  extract/search-read collisions (axes 9, 10, and the leftover of 5/6)
  before 0.13.1 ships. Canary does not implement those cuts.
- Axes 2, 4, 7 remain diagnosis-only here unless a later mission claims
  them. Canary never implements; findings feed thrones, thrones feed cuts,
  cuts get their own vc-trust.

## Refresh receipt (2026-09-17)

| Field | Value |
| ----- | ----- |
| Checkout | `/Volumes/vc-workspace/Loctree/aicx` · `feat/index-single-command@05bb14e` · ahead 4 of origin · working tree clean |
| Installed binary | `/opt/homebrew/bin/aicx` `0.13.1+g97ac6cc6` (≠ HEAD) |
| Loctree | `0.14.4+g3e9eb0a7` · `repo-view` 426 files · health 90 |
| `classify` where-symbol | 14/14 emitted, offset 0, truncated false, scanned 426/426 |
| `project_conversation` | 1 definition, `src/extraction/conversation.rs:1065` |
| `read_context_chunk` | 1 definition, `src/legacy_archive.rs:425` |
| Cursor session | `8f8472c3-a928-4276-a23f-66216c5d11d2` |

Adapters that now exist: Claude, Codex, Gemini, Grok, Junie, **Kimi**.
W0 of `aicx-0131-fail-gaps` landed at `05bb14e` (kimi allowlist +
watermark coverage). That is intake, not speech/tool-io truth.

## Decision axes (census seeds, with measured evidence)

Status tags: `OPEN` still competes · `THRONE+BYPASS` a writer exists and
a daily path skips it · `NARROWED` August defect shrunk, competition
remains · `WATCH` diagnosis-only.

### Axis 1 — Speech truth: "what is a human utterance?" 🔥 `THRONE+BYPASS`

August seed: five adapters each privately decide what human speech is,
plus reducers in `sanitize.rs`, `segmentation.rs`, `chunker.rs`, `noise.rs`.

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

Measured 2026-09-17:
- Writer/arbiter candidate: `crates/aicx-parser/src/engine/frames.rs:282`
  `classify`. Six adapter `classify` impls remain (`claude/codex/gemini/grok/junie/kimi`
  + `adapters/mod.rs` + `ContractAdapter`). That is the census pair:
  transport-local `AgentAdapter::classify` vs throne `frames::classify`.
- Gemini oversized-document `Fatal completeness` is addressed in 0.13.1
  notes (`max_document_bytes`, nested `skipped(oversized)`). Confirm live
  before dissolving the August gemini row.
- Mid-turn Claude queue and Codex echo-seal are claimed fixed in 0.13.0
  CHANGELOG. Re-probe; do not dissolve from changelog prose.
- **Bypass (new, daily):** `project_conversation` drops
  `kind == ShellAction && entry.frame_class.is_some()`. Classified tool
  frames never become "what was said/done" on `--conversation` (the
  SessionStart:compact mouth). See axis 9.

Census question: every site that decides message admission/classification,
with references (not definitions), and one verdict per competing pair.

### Axis 2 — Dense/vector truth: "where is THE semantic index?" 🔥 `OPEN`

Three layouts still claim it:
- canonical `<AICX_HOME>/indexed/<bucket>/embeddings.ndjson` (build source;
  `search --deep` requires it);
- generation layout `hybrid/generations/<g>/dense.exact_mmap_v1.bin` +
  `manifest.json` + `CURRENT` pointer;
- legacy residue: `~/index/_all/embeddings.ndjson` (OUTSIDE AICX_HOME)
  and the documented `dense_brute_force.ndjson` twin.

Measured 2026-09-17: `aicx search loctree-fail -j` →
`oracle_status.dense_count = 0`, `executed_path = lexical_only`. The
product still has no working semantic mouth. Generation-prune hak
(218 GB, 2026-08-19) is parked outside 0.13.1.

### Axis 3 — Session identity truth: "which session am I?" 🔥 `NARROWED`

August: `ExactAlias` silent substitute; parallel
`catalog::resolve_session` + `mcp_session::resolve_session`; multi-head
codex `01a03595`. Target identity = (store-id, content-hash);
`forked_from_id` is a lineage edge.

0.13.0 shipped `PackageIdentity` and `--lineage`. 2026-09-17 extract
probes resolved `36acb38d` and `01a084e3` via `ExactSourceId` honestly.
Census the remaining resolvers; do not assume the parallel surfaces
dissolved.

### Axis 4 — Artifact-store truth: "where do aicx artifacts live?" ⚠ `WATCH`

Phantom org `artifacts/vetcoders/aicx/` vs canonical
`artifacts/Loctree/aicx/`. Consolidated 2026-08-26. Repo is `Loctree/aicx`.
Census the resolver that still guesses org from path. 2026-09-17 scaffold
`aicx-0131-fail-gaps` wrote under `Loctree/aicx` on purpose.

### Axis 5 — Success/refusal truth: "did I actually answer?" 🔥 `THRONE+BYPASS`

August: silent-empty extract; `search --deep` typed `index_not_built` then
"No matches (scanned 0 chunks)".

0.13.0: empty extract is supposed to be a typed `refused:` + exit 1 at
projection seams. Protect that throne.

2026-09-17 bypass: `aicx read --help` claims it "closes the discover ->
read loop"; every live `aicx read` of a search `path` returned
`chunk not found`. Help is a success-shaped lie. See axis 10.
`dense_count=0` plus lexical hits is also emptiness dressed as a complete
answer unless the coverage stanza is on the MCP/human mouth (not only
`-j`).

### Axis 6 — Coverage/accounting truth: "what did I NOT consume?" ⚠ `NARROWED`

August: 722 pending (claude=389, …). Invisible to result consumers.

2026-09-17: CLI search JSON already carries `coverage.skipped`
(`claude_unindexed=253`, `codex_unindexed=186`, `kimi_unindexed=33`,
`vibecrafted_unindexed=290`; scanned 14935 of 15770). Human/MCP mouths
still present hits as the whole truth. Fresh extracts are not searchable
until index. `aicx-0131-fail-gaps` W3 owns the honesty stanza; this axis
is not closed by the JSON field existing.

Parser-side TB contract (`consumed_by_kind`, `known_skipped`) shipped in
0.13.0. Census whether extract/search consume that ledger or ignore it.

### Axis 7 — Redaction truth: "which layer kills a secret?" ⚠ `WATCH`

Unchanged seed: redact-by-default at
`src/output/conversation.rs::redact_conversation_messages`. Raw jsonl
uncovered by design; travels between hosts via transcript sync. Alarm
doctrine still applies. v3 design (paste-interceptor + `aicx redact
--in-place`) remains journaled, not shipped.

### Axis 8 — Epistemic competitors ◌ `WATCH`

`doctor/checks.rs`, `validate.rs`, `noise_smoke.rs`, `oracle_envelope.rs`,
adversarial tests, plus 2026-09-17 `scaffold-doctor` (plan-package gate,
not runtime). Census must prove their non-runtime boundary rather than
assume it.

### Axis 9 — Tool-io truth: "what did the agent actually run?" 🔥 `OPEN`

New seed from fail-log / hak #2 / live 2026-09-17. `--conversation` is
the recall hook mouth (`SessionStart:compact`). Two adapters, two lies,
one projection:

- **Claude (hash-without-command).** Session
  `36acb38d-3e95-4838-a804-05151a3f58d9`: raw jsonl has 1992 Bash
  `tool_use` with real `input.command` (first: `date && hostname && git
  status -sb…`). Extract: 3914 entries / 21434 lines / **2216**
  `$ $  [N lines, sha256:…] [0 lines, sha256:e3b0…]` / **0**
  `$ <alpha-cmd>`. Adapter arm `("assistant", "tool_use")` in
  `crates/aicx-parser/src/adapters/claude.rs` pushes `""` and hashes
  `input`. Fold then renders `$ {empty-or-$}`.
- **Codex (drop-without-trace).** Session
  `01a084e3-c1a3-7992-a6d6-2e83c532181c`: raw rollout 548
  `function_call` + 548 `function_call_output`. Extract: 11841 lines,
  **0** `sha256`, **0** `function_call`. Codex already fills `cmd` via
  `shell_action_marker`. `project_conversation` then **deletes**
  classified `FrameClass::ShellAction`.
- Docs compete: `docs/COMMANDS.md` says `--conversation` includes
  `$ cmd [N lines, sha256]`; `docs/OUTPUT_PROJECTION_CONTRACT.md` says
  `--conversation` has no shell lane. Founder hak #2 is the product
  decision: command stays.

Census question: every site that decides whether a tool call is speech,
stub, hash, or absence — adapter, `fold_shell_results`,
`project_conversation`, `--result`, recall hook — one verdict per pair.

### Axis 10 — Discover→read truth: "how do I open a hit?" 🔥 `OPEN`

New seed. `aicx_search` / `aicx search -j` emit `path` (extract md or,
historically, raw jsonl). `aicx read` / `aicx_read` resolve only legacy
archive chunks (`read_context_chunk`). Live 2026-09-17: extract path from
search, filename, `chunk:work-…`, and raw
`~/.codex/sessions/...jsonl` all → `chunk not found`.
`oracle_status.loctree_scope_note` =
`safe_for_lexical_scope_when_followed_by_canonical_chunk_read` is a
false safety label.

Census question: every writer of a "reference" (search `path`, `label`,
`chunk_id`, `source_path`, MCP `reference`) and every reader that claims
to accept it.

## Census contract

Run per vc-canary v2.1 (the skill is the protocol authority):

- **Mutation-free.** No code edits, no cargo/make/lint. The compile
  embargo of the taxonomy mission is closed; the protocol still forbids
  mutation inside a canary run.
- **One instrument.** Loctree organs only; grep forbidden as inventory or
  absence evidence; gaps → `.loctree/loctree-fail.md` + `UNRESOLVED`.
  (2026-09-17 Cursor session had no loctree-mcp namespace; CLI `loct`
  worked. That is an instrument gap, not permission to grep.)
- **References, not definitions.** A definition census once hid 141 call
  sites (codescribe W0 lesson). `classify` has 14 where-symbol sites —
  count call sites next, not this row.
- **Machine absence proofs.** `offset==0 · emitted==total ·
  truncated==false · scan_complete==true` + pinned snapshot fingerprint.
- **Verdict per pair** (SAME_SOURCE_OF_TRUTH / INTENTIONAL_VARIANT /
  DRIFTED_DUPLICATE / BYPASS_PATH / FALSE_PARALLEL), legend 🔥/⚠/◌,
  disposition per row (authority_edge / proven_non_runtime /
  obsolete_residue / UNRESOLVED).
- **Journal.** Append-only `./.loctree/canary/JOURNAL.md` in this repo
  (directory did not exist at v0.2 refresh — create on first run).
- **Run verdict.** AXES_CLOSED_CANDIDATE / AXES_OPEN /
  INSTRUMENT_INCOMPLETE / LAUNCHER_CONTRACT_CONFLICT; report ends with
  `BUILD/LINT/TEST/RUNTIME=NOT_ASSESSED`.

## What the census must NOT do

- Propose thrones or refactors inside the run (evidence first; thrones are
  decided in the mission, cuts after QC). `aicx-0131-fail-gaps` already
  has cuts for axes 9/10 — canary may confirm or dissolve, not redesign.
- Treat every multi-authority as a defect — runtime/replay and
  runtime/test splits may be INTENTIONAL_VARIANT with a proven boundary.
- Quote counts without the pinned fingerprint, or reconstruct any missing
  historical checkpoint (record `MISSING`).
- Dissolve an August row from CHANGELOG prose. Re-probe or mark
  `UNRESOLVED`.

## Standing context

- Living Tree: `feat/index-single-command@05bb14e` was clean at refresh.
  Re-read before assuming tree state. Do not sweep, do not revert. The
  August note about uncommitted echo-seal
  `work-260825-223415-07065` is historical; do not hunt that dirty tree.
- Installed homebrew `aicx` and workspace HEAD are different SHAs.
  Runtime probes must name which binary ran.
- Alarm doctrine (operator, 2026-08-26): report EVERY security-relevant
  observation in passing, no severity threshold — innocent-looking +
  execution-context change is the class that already ended in an attack.
- Host/auth: dragon MCP `403 Host header is not allowed` was Founder-
  resolved in config (2026-09-09). Leftover product work is wording
  (rejected Host ≠ token invalid), not a new identity axis unless the
  census finds a second auth truth.
