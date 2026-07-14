# AICX Intents Classification Rules

This document describes the current implementation behavior of `aicx intents`.
It is intentionally descriptive, not aspirational: it records how the classifier
currently fills the visible buckets, including places where that behavior is
semantically weak.

For the target semantic contract, see `docs/INTENTS_CORE_ONTOLOGY.md`.

Current branch/context: `fix/aicx-intents`, June 2026.

## Two Layers Of Semantics

There are two related but different classification layers:

- `IntentKind`, the visible `aicx intents` CLI bucket: `decision`, `intent`,
  `outcome`, `task`.
- `EntryType`, the richer internal classifier taxonomy: `intent`, `task`,
  `commitment`, `why`, `argue`, `decision`, `assumption`, `outcome`, `result`,
  `question`, `insight`.

The current 4-bucket CLI collapses the richer taxonomy:

| `EntryType` | Visible `IntentKind` |
|---|---|
| `Decision` | `decision` |
| `Task` | `task` |
| `Intent` | `intent` |
| `Question` | `intent` |
| `Why` | `intent` |
| `Outcome` | `outcome` |
| `Result` | `outcome` |
| `Commitment` | dropped |
| `Argue` | dropped |
| `Assumption` | dropped |
| `Insight` | dropped |

This means the current CLI is not a complete semantic ledger. It is a compact
compatibility surface over a richer, partially used classifier.

Source anchors:

- `IntentKind`: `src/intents/types.rs`
- `EntryType`: `crates/aicx-parser/src/types.rs`
- 11-type to 4-bucket mapping: `src/intents.rs::entry_type_to_timeline_kind`

## Pre-Classification File Selection

Before any line is classified, `aicx intents` selects stored canonical chunks:

1. Scans canonical chunk markdown files.
2. Keeps only `.md` files.
3. Filters by project using a case-insensitive substring match on the stored
   project slug/path.
4. Loads the sidecar metadata when available.
5. Skips Loctree context pack chunks.
6. Skips sidecars marked as example truth.
7. Applies `frame_kind`; default is `user_msg`.
8. Applies the time cutoff using the canonical chunk date from the store layout.
   Filesystem `mtime` is only fallback when canonical date parsing fails.

Default consequence: ordinary `aicx intents -p X` reads `user_msg` chunks, not
agent reply chunks. Agent reply chunks require explicit `--frame-kind`.

Source anchors:

- Chunk collection: `src/intents.rs::collect_chunk_files`
- Default frame kind: `src/intents/types.rs::IntentsConfig::default_frame_kind`

## Chunk Parsing

Each selected chunk is split into:

- `[signals] ... [/signals]`
- raw transcript entries

Transcript entries are recognized only from headers shaped like:

```text
[HH:MM:SS] role: message
```

The parser skips:

- card header lines — both the legacy v1 bracket shape (`[project: ...]`)
  and the v2 YAML frontmatter block are stripped structurally
- fenced code blocks in transcript
- fenced code blocks inside `[signals]`
- skill banner boilerplate inside `[signals]`

Source anchor: `src/intents.rs::parse_chunk_document`.

## Common Noise Filters

Several lines are ignored before they can become records:

- source metadata lines such as `source:`, `kind:`, `source_file:`, `project:`,
  `author:`, `input:`, `output:`
- local command artifact lines
- reingested charter / AGENTS / CLAUDE / Vibecrafted manifesto lines
- pasted user references beginning with `>` or `[Pasted text #`
- harness-injected synthetic user turns, such as slash-command bodies, skill
  bodies, inline local command IO, and system reminders

These filters try to keep generated framework material from becoming fake
operator intent.

Source anchors:

- `src/intents.rs::is_source_metadata_line`
- `src/intents.rs::is_reingested_charter_line`
- `src/sources/shared/conversation.rs::is_harness_injected_noise`
- `src/sources/shared/conversation.rs::intent_line_modality`

## Signal Section Rules

Inside `[signals]`, section headers strongly steer classification:

| Section header | Bucket |
|---|---|
| `Intent:` | `intent` |
| `Decision:` | `decision` |
| `Results:` | `outcome` |
| `Outcome:` | `outcome` |
| `Ultrathink:` | ignored |
| `Insight:` | ignored by the 4-bucket extractor |
| `Plan mode:` | ignored |
| `Notes:` | ignored |

After a recognized section header, non-empty bullet payloads under that section
inherit the section bucket. If there is no active section, the line falls back to
the raw line inference rules.

Checklist syntax is parsed before section bucket inference, so checklist items
become task events even if they appear under a signal section.

Source anchor: `src/intents.rs::extract_signal_candidates`.

## Transcript Rules

For raw transcript lines:

1. User messages get the richer `classify_line_entry_type` path first.
2. Non-user lines do not get user-intent keyword inference.
3. Explicit decision tags can still produce `decision` from non-user lines.
4. Outcome/result-looking non-user lines can produce `outcome`, except when the
   role suppresses outcome promotion.
5. `agent_reply`, `tool_*`, and `tool` roles suppress outcome/result promotion.

Source anchors:

- `src/intents.rs::extract_transcript_candidates`
- `src/intents.rs::infer_kind_from_line`
- `src/intents.rs::role_suppresses_outcome_promotion`

## Decision Bucket

Current `decision` records can come from:

1. `[signals]` section `Decision:`.
2. Explicit tags:
   - `[decision]`
   - `decision:`
3. User-authored lines containing a current policy/constraint marker:
   - `nie fixujemy`
   - `nie robimy`
   - `nie ruszamy`
   - `out of scope`
   - `poza scope`
   - `od teraz`
   - `from now on`
   - `canonical`
   - `kanonicz`
   - `default`
   - `domysln`
   - `domyśln`
   - `tylko przez`
   - `bez zgadywania`
   - `bez fallback`
   - `no fallback`
   - `ma byc dodatkiem`
   - `ma być dodatkiem`
4. The richer classifier returning `EntryType::Decision`.

Confidence examples:

- explicit `decision:` or `[decision]`: `0.95` in the richer classifier
- operator policy/constraint marker on user line: `0.75`
- fallback explicit decision tag: decision bucket directly

Current scope: this bucket no longer treats generic operator pressure such as
`musimy`, `trzeba`, or `nie moze byc` as a decision by itself. Those lines now
fall into intent unless they also contain a stronger policy/constraint signal.

Source anchors:

- `src/intents.rs::looks_like_operator_decision_line`
- `src/intents.rs::classify_line_entry_type`
- `crates/aicx-parser/src/chunker.rs::is_decision_tag`

## Intent Bucket

Current `intent` records can come from:

1. `[signals]` section `Intent:`.
2. User-authored `EntryType::Intent`.
3. User-authored `EntryType::Question`.
4. User-authored `EntryType::Why`.
5. Typed intent directive heads on user lines:
   - `intent:`
   - `[intent]`
6. User-authored intent keywords with word-boundary matching:
   - `mam pomysl`, `mam pomysł`
   - `mam taki pomysl`, `mam taki pomysł`
   - `pomysl`, `pomysł`
   - `proponuje`, `proponuję`
   - `zrobmy`, `zróbmy`
   - `ustalmy`
   - `chce`, `chcę`
   - `chcialbym`, `chciałbym`
   - `potrzebuje`, `potrzebuję`
   - `prosze`, `proszę`
   - `odpal`
   - `uruchom`
   - `usun`, `usuń`
   - `następny krok`, `nastepny krok`
   - `kolejny krok`
   - `i want`
   - `i'd like`
   - `let's`
   - `next step`
7. Severity markers `P0`, `P1`, `P2` in fallback intent inference.
8. User-authored operator requirement/pressure markers when no stronger
   decision marker applies:
   - `nie moze byc`
   - `nie może być`
   - `ma byc`
   - `ma być`
   - `musi `
   - `musimy `
   - `trzeba `
   - `zrob to testowalne`
   - `zrób to testowalne`
   - `pelny ownership`
   - `pełny ownership`
   - `teraz wypuszuj`

Keyword matching tries to avoid false positives by:

- requiring word boundaries
- ignoring matches inside inline code spans
- ignoring immediately negated keywords such as `nie chcę` or `let's not`

Important current behavior: `task:` and `zadanie:` are now preempted by the
task classifier before the generic typed-directive fallback. Plain imperative
requests such as "stworz prosze plik..." can become `task` when they start with
a known action verb and carry enough content to be a concrete work item.

Source anchors:

- `src/intents.rs::looks_like_intent_line`
- `src/intents.rs::matches_keyword_word_boundary`
- `src/sources/shared/conversation.rs::intent_line_modality`
- `crates/aicx-parser/src/chunker.rs::INTENT_KEYWORDS`

## Outcome Bucket

Current `outcome` records can come from:

1. `[signals]` sections:
   - `Results:`
   - `Outcome:`
2. Explicit outcome tags:
   - `[skill_outcome]`
   - `outcome:`
   - `validation:`
3. Explicit result tags:
   - `result:`
   - `wynik:`
4. Result keywords from the chunker:
   - `smoke test`
   - `passed`
   - `all checks passed`
   - `0 failed`
   - `completed`
   - `done`
   - `zrobione`
   - `dowiezione`
   - `gotowe`
   - `dziala`
   - `działa`
5. Strict result markers:
   - `passed`
   - `failed`
   - `score=`
   - `score:`
   - `latency`
   - `p0=`, `p1=`, `p2=`
   - `/10`
   - `clippy`
   - `cargo test`
   - `0 warnings`
   - `0 errors`
6. Soft result markers only when the line has result shape:
   - `tests `
   - `error:`
7. Completed-state report markers when the line also looks file/artifact-shaped:
   - `zostal dodany`, `został dodany`
   - `zostala dodana`, `została dodana`
   - `zostaly dodane`, `zostały dodane`
   - `zostal utworzony`, `został utworzony`
   - `has been added`, `was added`
   - `has been created`, `was created`
8. Observed count/result reports when the line has digits, count nouns such as
   `records`, `rekord`, or `wynik`, and a result verb such as `dal`, `dał`,
   `yielded`, `produced`, or `gave`.

A line has result shape when it contains a digit, a pass/fail-like token, or a
known status word such as `done`, `skipped`, `ignored`, `timeout`, `panicked`,
or `panic:`.

Bare acknowledgements are not outcomes by themselves:

- `zrobione`
- `dowiezione`
- `gotowe`
- `dziala`
- `działa`
- `done`
- `completed`

They become eligible again when followed by detail, for example:

```text
done: cargo test passed
```

Source anchors:

- `src/intents.rs::is_outcome_line`
- `src/intents.rs::line_has_result_shape`
- `src/intents.rs::looks_like_completion_outcome_line`
- `src/intents.rs::looks_like_observed_count_outcome_line`
- `src/intents.rs::classify_line_entry_type`
- `crates/aicx-parser/src/chunker.rs::is_result_line`
- `crates/aicx-parser/src/chunker.rs::is_outcome_tag`

## Task Bucket

Current `task` records can come from two paths:

1. Checklist lifecycle events:

```text
- [ ] open task
- [x] done task
* [ ] open task
+ [ ] open task
```

2. Raw semantic task candidates:

```text
task: create import adapter
zadanie: dodaj test regresji
[ ] stworz prosze plik z zasadami klasyfikacji
stworz prosze plik z zasadami klasyfikacji
```

Checklist rules:

1. The first character must be `-`, `*`, or `+`.
2. It must be followed by `[ ]`, `[x]`, or `[X]`.
3. Empty task text is ignored.
4. Checklist items create task events, not normal candidates.
5. Task events are merged by normalized task text.
6. Later state wins when the same task appears again.
7. Only open tasks survive finalization.
8. If `--kind` is not `task`, task events are not returned.

Raw task classifier rules:

1. `task:`, `todo:`, and `zadanie:` are typed task directives.
2. Bare user-authored `[ ]`, `[x]`, and `[X]` lines are treated as task
   candidates even without a bullet prefix.
3. User-authored imperative lines can be tasks when they start with a known
   action verb such as `stworz`, `dodaj`, `napraw`, `zaimplementuj`, `create`,
   `fix`, or `write`.
4. Very short imperatives are ignored; the line needs at least three words.

Consequences:

- A human task request can now become `task` without checklist syntax when it
  has concrete imperative shape.
- `task:` / `zadanie:` now becomes `task`, not `intent`.
- A completed checklist item can remove the task from visible output.
- Raw semantic tasks do not participate in checklist open/closed finalization;
  they are ordinary classified candidates.

This removes the worst naming mismatch, but the task lane is still mixed:
checklist tasks have lifecycle semantics, while raw semantic tasks are
point-in-time work-item detections.

Source anchors:

- `crates/aicx-parser/src/chunker.rs::parse_checklist_task`
- `src/intents.rs::build_task_event`
- `src/intents.rs::finalize_tasks`
- `src/intents.rs::looks_like_task_directive_line`
- `src/intents.rs::looks_like_actionable_task_line`

## Confidence And Dedup Rules

Candidate confidence:

| Candidate source | Starting confidence |
|---|---:|
| `[signals]` candidate | 4 |
| raw `intent` candidate | 2 |
| raw `decision`, `outcome`, or `task` candidate | 3 |

Confidence bonuses:

- `+1` when surrounding context exists
- `+1` when evidence tokens exist
- `+1` for `intent` with `P0`, `P1`, or `P2`
- capped at `5`

Default thresholds:

- normal mode: `1`
- `--strict`: `4`
- `--min-confidence N`: explicit threshold override

Dedup:

- normal candidates dedup by `(kind, project, normalized summary)`
- task events dedup by normalized task text
- summary normalization removes invisible characters, collapses whitespace, and
  lowercases text
- duplicate candidates merge evidence/context/provenance, preferring stronger or
  later records

Source anchors:

- `src/intents.rs::calculate_confidence`
- `src/intents.rs::dedup_candidates`
- `src/intents.rs::finalize_tasks`
- `crates/aicx-parser/src/chunker.rs::normalize_key`

## Display-Time Rules

After extraction, display filters run separately. They do not change the
underlying classification.

Display filters can:

- hide resolved intents
- collapse by session
- filter by agent
- filter by date
- sort newest/oldest
- apply limit

`--unresolved` has two modes:

- session mode: any `outcome` in a session resolves all `intent` records in that
  session
- intent mode: an `outcome` resolves an `intent` when project matches and
  significant word overlap is high enough

Source anchor: `src/intents/display.rs::apply_display_filters`.

## Known Semantic Fractures

These are not necessarily bugs in control flow. They are places where current
classification semantics diverge from ordinary operator language.

### `decision` vs `intent`

The decision heuristic used to be too broad: it promoted lines containing
`musi`, `musimy`, `trzeba`, or `ma byc` to `decision`.

Current behavior narrows `decision` to explicit tags and policy/constraint
markers, while generic operator pressure is treated as `intent`.

Better semantic distinction:

- `intent`: "we need/want/request/plan to do X"
- `decision`: "we chose X", "X is the default", "Y is forbidden", "from now on
  this constraint governs the work"

Examples:

| Text | Current likely bucket | Better semantic bucket |
|---|---|---|
| `musimy dodac parser dla obcych md` | `intent` | `intent` |
| `obce md importujemy tylko przez operator-md, bez zgadywania cwd` | `decision` | `decision` |
| `nie moze byc tak, ze ...` | `intent` | depends: often intent/bug pressure |

### `task` Is No Longer Checklist-Only

The current `task` bucket now captures open checklist items, typed task
directives, bare checkbox lines, and a narrow set of imperative work requests.
Checklist tasks still have stronger lifecycle behavior than raw semantic tasks.

Examples:

| Text | Current likely bucket | Better semantic bucket |
|---|---|---|
| `stworz prosze plik z zasadami klasyfikacji` | `task` | task/work item |
| `[ ] stworz prosze plik z zasadami klasyfikacji` | `task` | task/work item |
| `task: stworz plik` | `task` | task/work item |

### `outcome` Is Runtime Evidence, Not Merely "A Thing Mentioned Afterward"

The current outcome bucket is closest to "result/evidence/status report". It now
recognizes a narrow slice of natural-language completion/count reports, but it
still intentionally avoids broad "anything in past tense" inference.

Examples:

| Text | Current likely bucket | Better semantic bucket |
|---|---|---|
| `batch 10 plikow dal 66 records` | `outcome` | `outcome` |
| `cargo test passed, 0 warnings` | `outcome` | `outcome` |
| `gotowe` | ignored as bare acknowledgement | usually ignore |

### Rich Types Are Partly Invisible

The classifier can recognize `question`, `why`, `commitment`, `assumption`,
`insight`, and `argue`, but the visible 4-bucket CLI only keeps some of them:

- `question` and `why` become `intent`
- `commitment`, `assumption`, `insight`, and `argue` are dropped

Assumption markers include both accented and ASCII Polish forms, for example
`zakładam`, `zakladam`, `założenie:`, `zalozenie:`, plus English
`assumption:` and `hypothesis:`.

Commitment markers are intentionally narrow and head-anchored: `commitment:`,
`promise:`, `obietnica:`, `zrobie`, `zrobię`, `zajme sie`, `zajmę się`,
`i will`, and `i'll`.

This makes the 4-bucket output useful for quick retrieval, but incomplete as a
semantic model of the conversation.

## Practical Reading Contract

When reading current `aicx intents` output:

1. Treat `intent` as "operator/user wants, asks, questions, or motivates".
2. Treat `decision` as "tagged or forcefully phrased operator constraint",
   then verify whether it is truly a durable decision.
3. Treat `outcome` as "runtime/result/status evidence", not as a broad product
   outcome.
4. Treat `task` as "concrete work item"; remember that checklist tasks have
   lifecycle finalization while raw task candidates do not.
5. Treat `--strict` as a confidence filter, not as a semantic fix.
6. Treat display filters as presentation-only.

The strongest current fix direction remains semantic, not numeric: continue
narrowing `decision`, broaden `outcome` only around real evidence, and expose
the richer `EntryType` taxonomy instead of collapsing important conversation
types too early.
