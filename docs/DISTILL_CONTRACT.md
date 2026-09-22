# Distill contract — per-agent lane distillation (W0)

Status: W0 frozen contract for the `aicx-distill-per-agent-v1` plan. Rust
surface: `src/extraction/distill/mod.rs` (trait `AgentLaneDistiller` + types).
Differential oracle: `tests/tb_oracle_harness.rs` on the frozen TB package in
`tests/fixtures/tb_oracle/_shared/`.

Ta warstwa jest **projekcją** nad `SessionModel` — obowiązują oba istniejące
kontrakty:

- `OUTPUT_PROJECTION_CONTRACT.md` — destylat jest widokiem/overlayem; nigdy
  drugim reduktorem substratu. Distiller czyta model, niczego nie mutuje.
- `PARSER_NORMATIVE_CONTRACT.md` (C0A §1.1) — żadne pole heurystyczne nie
  wchodzi do canonical fingerprint. Destylat żyje w całości poza kernelem
  (`crates/aicx-parser` pozostaje nietknięty).

Wiążące decyzje projektowe: Design Contract w planie
`2026_0917_aicx-adopts-tb-wzorzec.md` (5 decyzji vc-grill 2026-09-17), w tym
decyzja 4: **brief per segment** dla `scope_status=mixed_candidate` — outcomes
nigdy nie są uśredniane między workstreamami. Stąd jednostką destylatu jest
`SegmentDistillate` (jeden per segment), a `AgentLaneDistiller::distill_segment`
przyjmuje `&SessionModel` + `&Segment`.

## Mapowanie pól TB→aicx

Portujemy **reguły i nazwy pól** ze schematu TB `transcript_builder.index_payload.v1`
(+ sekcje `human.md`), nie kod Pythona. TB pozostaje wyrocznią różnicową w
testach, nigdy zależnością runtime.

| TB (`index_payload.v1` / `human.md`) | aicx (`src/extraction/distill`) | Uwagi |
|---|---|---|
| `decision_candidates[]` | `SegmentDistillate::decision_candidates: Vec<DecisionCandidate>` | Kandydat, nie werdykt; `kind` to passthrough słownika TB. |
| `gates[]` | `SegmentDistillate::gates: Vec<GateObservation>` | `GateOutcome::Unknown` ≠ pass; werdykt czytany z substratu, nie zgadywany. |
| `open_questions[]` (`{kind, text, evidence_locator}`) | `SegmentDistillate::open_questions: Vec<OpenQuestion>` | Kształt itemu 1:1 z rekordu TB. |
| `human.md` „Handoff Signals" (`signal:`) | `SegmentDistillate::handoff_signals: Vec<HandoffSignal>` | Typowo ostatni `assistant_final` segmentu. |
| `agent_outcome` (`complete/partial/failed/unknown`) | `LaneOutcome::agent_outcome: AgentOutcome` | Default `Unknown` — upgrade tylko z dowodu. |
| `ending` (np. `interrupted`) | `LaneOutcome::ending: Option<String>` | Passthrough słownika TB (tail observation). |
| `evidence_locator` (`{segment_id, turn_idx}`) | `EvidenceLocator { segment_id, turn_idx, timestamp }` | Każde roszczenie destylatu ma lokator do substratu. |
| `segment_id` | `SegmentDistillate::segment_id` (`Segment::segment_id`) | |
| `agent` | `SegmentDistillate::agent: AgentKind` | Parserowy `AgentKind`, nie catalogowy. |
| — | `SegmentDistillate::scope_status` | Kopiowany z `Segment`, nigdy przeliczany w destylacie. |
| `schema` | `SegmentDistillate::schema = aicx.distill.segment_distillate.v1` | |

Pola TB świadomie **poza** W0 (wchodzą z W2/card.v3 albo wcale):
`deliverables`, `deliverable_evidence`, `topics`, `entities`, `footprint`,
`recall_value`, `quality_flags`, `freshness_contract` — nie są częścią
zamrożonego kontraktu lane'ów.

## Pola wspólne wyroczni (harness)

`tests/tb_oracle_harness.rs` diffuje zbiór pól, które obie strony znają o tej
samej sesji: `agent`, `map_id`, `cwd`, `branch`, `source_sha256`
(znormalizowany bez prefiksu `sha256:`), `segments` (liczność). W0 dowodzi
loader + diff na wewnętrznej spójności pakietu TB (human.md vs
index_payload.jsonl) w obu kierunkach (zieleń przy zgodzie, czerwień przy
rozstrojonym polu); W2 wpina stronę aicx w to samo `diff_common_fields`.

## Rejestr lane'ów

`LaneRegistry` mapuje `AgentKind → Box<dyn AgentLaneDistiller>`. Rejestr jest
fail-open na pustkę: agent bez zarejestrowanego lane'a dostaje `GenericLane`,
który zwraca `SegmentDistillate::empty` (puste wektory, `AgentOutcome::Unknown`)
— pusta odpowiedź jest uczciwa, panic nie jest.

## Reguła append-only (`src/extraction/distill/mod.rs`, salwa W1)

Siedmiu równoległych workerów W1 rozszerza jeden wspólny plik. Obowiązuje:

1. **Re-read przed edycją** — plik czytasz bezpośrednio przed każdą zmianą i
   adaptujesz się do cudzych dopisków; nigdy ich nie rewertujesz.
2. **Tylko append w strefach oznaczonych** `W1 append zone`: jedna linia
   `pub mod <agent>_lane;` na dole pliku oraz jedna linia
   `registry.register(Box::new(...))` w `LaneRegistry::with_default_lanes`.
3. **Zero zmian w zamrożonym kontrakcie** — trait, typy destylatu i sygnatury
   powyżej stref append są własnością W0; zmiana = nowy cut kontraktowy, nie
   „drobna poprawka" w locie.
4. Implementacja lane'a żyje we własnym pliku `src/extraction/distill/<agent>_lane.rs`
   — wspólny `mod.rs` dostaje wyłącznie rejestrację.

_𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI_
