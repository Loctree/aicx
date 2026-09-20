//! Claude Code lane distiller (W1-01).
//!
//! Reads the parsed [`SessionModel`] of a Claude Code session and projects
//! per-segment handoff signals per `docs/DISTILL_CONTRACT.md`. Lane-specific
//! substrate facts this implementation leans on:
//!
//! - Bash commands travel as the *body* of `ToolCall` turns since
//!   `claude-adapter-v2` (`8139e8d`); gate detection reads `Turn::text`, not
//!   tool payloads.
//! - `frame_class` may be `None` (tool lanes are class-less); selection works
//!   on `TurnKind`, with `FrameClass::AssistantFinal` accepted when present.
//! - A Claude sidechain is a whole-file property: the adapter surfaces
//!   `agentId` as `ProviderConversationRef::Claude { agent_id: Some(_) }`.
//!   Sub-agent transcripts never emit handoff signals — a sidechain's closing
//!   words are a worker's report to its parent, not a session handoff.
//! - Gate verdicts are read from the correlated `ToolResult` retained text
//!   (`tool_events` correlation), never guessed from the command line alone.

use super::{
    AgentLaneDistiller, AgentOutcome, DecisionCandidate, EvidenceLocator, GateObservation,
    GateOutcome, HandoffSignal, LaneOutcome, OpenQuestion, SegmentDistillate,
};
use aicx_parser::engine::{
    AgentKind, Known, ProviderConversationRef, Segment, SessionModel, ToolEventKind, Turn,
    TurnKind, TurnRole,
};

/// Command stems that count as quality-gate runs when they appear in a Bash
/// `ToolCall` body. Substring match on the recorded command, case-sensitive —
/// these are literal invocations, not prose.
const GATE_COMMAND_STEMS: [&str; 8] = [
    "cargo fmt",
    "cargo clippy",
    "cargo test",
    "cargo build",
    "pytest",
    "make ",
    "pnpm ",
    "npm test",
];

/// Failure signals in a gate's retained result text. Checked before pass
/// signals: a run that prints both has failed.
const GATE_FAIL_SIGNALS: [&str; 6] = [
    "test result: FAILED",
    "error[E",
    "error: could not compile",
    "assertion failed",
    "FAILED. ",
    "Diff in ",
];

/// Positive success signals. Anything without an explicit signal stays
/// `GateOutcome::Unknown` — the honest non-verdict.
const GATE_PASS_SIGNALS: [&str; 3] = ["test result: ok", "Finished `", "0 failed"];

/// Decision-leaning line markers in assistant finals, TB `kind` per group.
/// Lowercased comparison; verbatim-leaning extraction keeps the line as
/// written.
const EXPLICIT_CHOICE_MARKERS: [&str; 9] = [
    "decyzja:",
    "decyduję",
    "wybieram",
    "zakładam",
    "stawiam na",
    "idę w",
    "i'll use",
    "going with",
    "decided to",
];
const PLAN_COMMITMENT_MARKERS: [&str; 4] = ["ruszam z", "plan:", "i'll start", "najpierw"];

/// Claude Code writes this literal into the transcript when the operator
/// interrupts a running turn.
const INTERRUPT_MARKER: &str = "[Request interrupted";

/// TB `ending` vocabulary token for an interrupted tail.
const ENDING_INTERRUPTED: &str = "interrupted";

#[derive(Debug, Clone, Copy, Default)]
pub struct ClaudeLane;

impl AgentLaneDistiller for ClaudeLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Claude
    }

    fn lane_name(&self) -> &'static str {
        "claude"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let turns: Vec<&Turn> = model
            .turns
            .iter()
            .filter(|turn| turn.segment_id == segment.segment_id)
            .collect();
        if turns.is_empty() {
            return SegmentDistillate::empty(AgentKind::Claude, segment);
        }

        let decision_candidates = distill_decisions(segment, &turns);
        let gates = distill_gates(model, segment, &turns);
        let open_questions = distill_open_questions(segment, &turns);
        let handoff_signals = if is_sidechain(model) {
            Vec::new()
        } else {
            distill_handoff(segment, &turns)
        };
        let outcome = distill_outcome(&turns, &gates, &handoff_signals, &decision_candidates);

        SegmentDistillate {
            decision_candidates,
            gates,
            open_questions,
            handoff_signals,
            outcome,
            ..SegmentDistillate::empty(AgentKind::Claude, segment)
        }
    }
}

/// A Claude sub-agent transcript announces itself through `agentId` rows,
/// which the adapter lifts to the conversation ref. Per-turn `isSidechain`
/// is not modelled by the substrate; the file-level lane is the evidence.
fn is_sidechain(model: &SessionModel) -> bool {
    matches!(
        &model.conversation,
        ProviderConversationRef::Claude {
            agent_id: Some(_),
            ..
        }
    )
}

fn locator(segment: &Segment, turn: &Turn) -> EvidenceLocator {
    EvidenceLocator {
        segment_id: segment.segment_id,
        turn_idx: Some(turn.turn_idx),
        timestamp: match &turn.timestamp {
            Known::Value(value) => Some(value.clone()),
            Known::Unknown(_) => None,
        },
    }
}

/// The assistant-final lane: `AgentReply` turns. `frame_class` is accepted as
/// confirmation when present but never required — class-less Claude turns
/// distill from `TurnKind` alone (brief §3).
fn is_assistant_final(turn: &Turn) -> bool {
    turn.kind == TurnKind::AgentReply && turn.role == TurnRole::Assistant
}

fn distill_decisions(segment: &Segment, turns: &[&Turn]) -> Vec<DecisionCandidate> {
    let mut candidates = Vec::new();
    for turn in turns.iter().filter(|turn| is_assistant_final(turn)) {
        for line in turn.text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let lower = line.to_lowercase();
            let kind = if EXPLICIT_CHOICE_MARKERS
                .iter()
                .any(|marker| lower.contains(marker))
            {
                "explicit_choice"
            } else if PLAN_COMMITMENT_MARKERS
                .iter()
                .any(|marker| lower.contains(marker))
            {
                "plan_commitment"
            } else {
                continue;
            };
            candidates.push(DecisionCandidate {
                text: line.to_owned(),
                kind: kind.to_owned(),
                evidence: locator(segment, turn),
            });
        }
    }
    candidates
}

/// Gate runs: Bash `ToolCall` turns whose recorded command carries a gate
/// stem. The verdict comes from the correlated `ToolResult` turn's retained
/// text; a gate without a readable result stays `Unknown`.
fn distill_gates(model: &SessionModel, segment: &Segment, turns: &[&Turn]) -> Vec<GateObservation> {
    let mut gates = Vec::new();
    for turn in turns {
        if turn.kind != TurnKind::ToolCall {
            continue;
        }
        let is_bash = matches!(&turn.tool_name, Known::Value(name) if name == "Bash");
        if !is_bash || turn.text.is_empty() {
            continue;
        }
        // Stems are matched on the first line only: a gate mention inside a
        // heredoc body (e.g. a commit message quoting `cargo test`) is prose,
        // not a run.
        let first_line = turn.text.lines().next().unwrap_or_default();
        if !GATE_COMMAND_STEMS
            .iter()
            .any(|stem| first_line.contains(stem))
        {
            continue;
        }
        let outcome = correlated_result_text(model, turn.turn_idx)
            .map_or(GateOutcome::Unknown, read_gate_verdict);
        gates.push(GateObservation {
            command: turn.text.clone(),
            outcome,
            evidence: locator(segment, turn),
        });
    }
    gates
}

/// Follow the tool-event correlation from a call turn to its result turn and
/// return the retained result text.
fn correlated_result_text(model: &SessionModel, call_turn_idx: u64) -> Option<&str> {
    let call = model
        .tool_events
        .iter()
        .find(|event| event.kind == ToolEventKind::Call && event.turn_idx == call_turn_idx)?;
    let correlation = match &call.correlation_id {
        Known::Value(value) => value,
        Known::Unknown(_) => return None,
    };
    let result = model.tool_events.iter().find(|event| {
        event.kind == ToolEventKind::Result
            && matches!(&event.correlation_id, Known::Value(value) if value == correlation)
    })?;
    model
        .turns
        .iter()
        .find(|turn| turn.turn_idx == result.turn_idx)
        .map(|turn| turn.text.as_str())
}

fn read_gate_verdict(result: &str) -> GateOutcome {
    if GATE_FAIL_SIGNALS
        .iter()
        .any(|signal| result.contains(signal))
    {
        GateOutcome::Fail
    } else if GATE_PASS_SIGNALS
        .iter()
        .any(|signal| result.contains(signal))
    {
        GateOutcome::Pass
    } else {
        GateOutcome::Unknown
    }
}

/// Questions left open in the segment tail: a `?`-terminated line in speech
/// after the counterpart's last chance to answer inside this segment.
/// A user question is open when no assistant final follows it; an assistant
/// question is open when no user message follows it.
fn distill_open_questions(segment: &Segment, turns: &[&Turn]) -> Vec<OpenQuestion> {
    let last_assistant_final = turns.iter().rposition(|turn| is_assistant_final(turn));
    let last_user_msg = turns
        .iter()
        .rposition(|turn| turn.kind == TurnKind::UserMsg);
    let mut questions = Vec::new();
    for (position, turn) in turns.iter().enumerate() {
        let kind = match turn.kind {
            TurnKind::UserMsg if last_assistant_final.is_none_or(|last| last < position) => {
                "user_report"
            }
            TurnKind::AgentReply if last_user_msg.is_none_or(|last| last < position) => {
                "agent_question"
            }
            _ => continue,
        };
        for line in turn.text.lines() {
            let line = line.trim();
            if line.ends_with('?') && !line.starts_with('#') {
                questions.push(OpenQuestion {
                    kind: kind.to_owned(),
                    text: line.to_owned(),
                    evidence: locator(segment, turn),
                });
            }
        }
    }
    questions
}

/// The handoff signal is the segment's last assistant final, verbatim.
fn distill_handoff(segment: &Segment, turns: &[&Turn]) -> Vec<HandoffSignal> {
    turns
        .iter()
        .rev()
        .find(|turn| is_assistant_final(turn) && !turn.text.trim().is_empty())
        .map(|turn| HandoffSignal {
            text: turn.text.trim().to_owned(),
            evidence: locator(segment, turn),
        })
        .into_iter()
        .collect()
}

/// Outcome heuristic, most-evidence-first; `Unknown` is the resting state:
/// 1. the last observed gate failed → `Failed`;
/// 2. no gate failed, at least one passed, and a handoff exists → `Complete`
///    (`Unknown` gates like a silent `cargo fmt --check` don't veto — they
///    are just unreadable verdicts, and the positive evidence stands);
/// 3. an interrupted tail over real work (gates or decisions) → `Partial`.
fn distill_outcome(
    turns: &[&Turn],
    gates: &[GateObservation],
    handoff: &[HandoffSignal],
    decisions: &[DecisionCandidate],
) -> LaneOutcome {
    let interrupted = turns
        .last()
        .is_some_and(|turn| turn.text.contains(INTERRUPT_MARKER) || turn.kind == TurnKind::UserMsg);
    let ending = interrupted.then(|| ENDING_INTERRUPTED.to_owned());
    let agent_outcome = if gates
        .last()
        .is_some_and(|gate| gate.outcome == GateOutcome::Fail)
    {
        AgentOutcome::Failed
    } else if gates.iter().any(|gate| gate.outcome == GateOutcome::Pass)
        && gates.iter().all(|gate| gate.outcome != GateOutcome::Fail)
        && !handoff.is_empty()
    {
        AgentOutcome::Complete
    } else if interrupted && (!gates.is_empty() || !decisions.is_empty()) {
        AgentOutcome::Partial
    } else {
        AgentOutcome::Unknown
    };
    LaneOutcome {
        agent_outcome,
        ending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicx_parser::adapters::registered_adapter;
    use aicx_parser::engine::{
        RawUnitReader, ReaderPolicy, ScopeStatus, SourceArtifact, SourceFraming, SourceHandle,
        ValidatedParse, validate_parse,
    };
    use std::path::{Path, PathBuf};

    fn fixture_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/tb_oracle/claude")
    }

    /// Parse a real (redacted) Claude Code JSONL fixture through the public
    /// kernel surface — the same path production extraction takes.
    fn parse_fixture(name: &str) -> SessionModel {
        let path = fixture_dir().join(name);
        let body = std::fs::read(&path)
            .unwrap_or_else(|error| panic!("cannot read fixture {}: {error}", path.display()));
        let artifact = SourceArtifact::memory("session.jsonl", body, SourceFraming::JsonLines)
            .expect("memory artifact");
        let session_id = "6abdbe22-43e0-4544-a5ef-7314ece85078";
        let source = SourceHandle::new(
            AgentKind::Claude,
            session_id,
            Some(session_id.to_owned()),
            vec![artifact],
        )
        .expect("source handle");
        let read = RawUnitReader::new(ReaderPolicy::default())
            .read(&source)
            .expect("bounded read");
        let adapter = registered_adapter(AgentKind::Claude);
        let classified = adapter.classify(&source, &read).expect("classification");
        let parse = adapter
            .assemble(&source, &read, classified)
            .expect("assembly");
        match validate_parse(parse).expect("kernel validation") {
            ValidatedParse::Session(session) => session.into_model(),
            ValidatedParse::Fatal(fatal) => {
                panic!("unexpected fatal parse: {:?}", fatal.coverage().status)
            }
        }
    }

    /// Golden test over the real-session fixture. Regenerate deliberately with
    /// `AICX_BLESS=1 cargo test -p aicx claude_lane_matches_golden` and read
    /// the diff — the golden is a reviewed artifact, not a cache.
    #[test]
    fn claude_lane_matches_golden() {
        let model = parse_fixture("6abdbe22_session.jsonl");
        let distillates = ClaudeLane.distill(&model);
        let rendered =
            serde_json::to_string_pretty(&distillates).expect("distillate serializes") + "\n";
        let golden_path = fixture_dir().join("6abdbe22_golden-distillate.json");
        if std::env::var_os("AICX_BLESS").is_some() {
            std::fs::write(&golden_path, &rendered).expect("write golden");
        }
        let golden = std::fs::read_to_string(&golden_path).unwrap_or_else(|error| {
            panic!("cannot read golden {}: {error}", golden_path.display())
        });
        assert_eq!(
            rendered, golden,
            "distillate drifted from golden; if intentional, bless with AICX_BLESS=1"
        );
    }

    /// Sub-agent (sidechain) transcripts never contribute handoff signals.
    /// The non-sidechain fixture proves the selector is not vacuous.
    #[test]
    fn sidechain_turns_excluded_from_handoff_signals() {
        let sidechain = parse_fixture("sidechain.jsonl");
        assert!(
            is_sidechain(&sidechain),
            "fixture must parse as a sub-agent lane (agentId rows)"
        );
        let distillates = ClaudeLane.distill(&sidechain);
        assert!(!distillates.is_empty());
        assert!(
            distillates.iter().all(|d| d.handoff_signals.is_empty()),
            "sidechain segments must emit no handoff signals"
        );

        let operator = parse_fixture("6abdbe22_session.jsonl");
        assert!(!is_sidechain(&operator));
        let operator_distillates = ClaudeLane.distill(&operator);
        assert!(
            operator_distillates
                .iter()
                .any(|d| !d.handoff_signals.is_empty()),
            "operator session must emit a handoff signal (guard against a vacuous filter)"
        );
    }

    /// `scope_status = mixed_candidate` sessions distill per segment — one
    /// distillate per segment, outcomes never merged (Design Contract 4).
    #[test]
    fn mixed_session_yields_one_distillate_per_segment() {
        let model = parse_fixture("mixed_two_segments.jsonl");
        assert_eq!(model.segments.len(), 2, "fixture must split on cwd change");
        assert_eq!(model.scope_status(), ScopeStatus::MixedCandidate);
        let distillates = ClaudeLane.distill(&model);
        assert_eq!(distillates.len(), 2);
        let ids: Vec<u32> = distillates.iter().map(|d| d.segment_id).collect();
        assert_eq!(ids, vec![0, 1]);
        for (distillate, segment) in distillates.iter().zip(&model.segments) {
            assert_eq!(distillate.scope_status, segment.scope_status);
            for signal in &distillate.handoff_signals {
                assert_eq!(
                    signal.evidence.segment_id, distillate.segment_id,
                    "handoff evidence must stay inside its own segment"
                );
            }
        }
    }

    /// Registry wiring: the default registry resolves Claude to this lane.
    #[test]
    fn registry_resolves_claude_lane() {
        let registry = super::super::LaneRegistry::with_default_lanes();
        assert_eq!(registry.lane_for(AgentKind::Claude).lane_name(), "claude");
    }
}
