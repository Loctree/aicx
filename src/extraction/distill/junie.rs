//! Junie agent lane distiller (`W1-05`).
//!
//! Extracts per-segment distillates (`DecisionCandidate`, `GateObservation`,
//! `OpenQuestion`, `HandoffSignal`, `LaneOutcome`) from a Junie `SessionModel`.
//!
//! Junie-specific rules:
//! - Logical units in the session model reflect the final snapshot per `(stepId, kind)`;
//!   intermediate `IN_PROGRESS` snapshots are excluded.
//! - `UserPromptEvent` carries user intent and open questions.
//! - `SystemMessageEvent` is an injected context frame and must NEVER be treated as a handoff signal.
//! - Per-segment distillation for mixed or multi-turn sessions.

use crate::extraction::distill::{
    AgentLaneDistiller, AgentOutcome, DecisionCandidate, EvidenceLocator, GateObservation,
    GateOutcome, HandoffSignal, LaneOutcome, OpenQuestion, SEGMENT_DISTILLATE_SCHEMA,
    SegmentDistillate,
};
use aicx_parser::engine::{AgentKind, Known, Segment, SessionModel, Turn, TurnKind, TurnRole};

/// Lane distiller for Junie sessions.
#[derive(Debug, Clone, Copy, Default)]
pub struct JunieLane;

impl JunieLane {
    pub fn new() -> Self {
        Self
    }
}

impl AgentLaneDistiller for JunieLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Junie
    }

    fn lane_name(&self) -> &'static str {
        "junie"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let start = segment.turn_range.start as usize;
        let end = (segment.turn_range.end as usize).min(model.turns.len().saturating_sub(1));

        if start > end || model.turns.is_empty() {
            return SegmentDistillate::empty(AgentKind::Junie, segment);
        }

        let segment_turns = &model.turns[start..=end];

        let decision_candidates = extract_decision_candidates(segment.segment_id, segment_turns);
        let gates = extract_gate_observations(segment.segment_id, segment_turns);
        let open_questions = extract_open_questions(segment.segment_id, segment_turns);
        let handoff_signals = extract_handoff_signals(segment.segment_id, segment_turns);
        let outcome =
            determine_lane_outcome(model, segment_turns, &gates, !handoff_signals.is_empty());

        SegmentDistillate {
            schema: SEGMENT_DISTILLATE_SCHEMA.to_owned(),
            segment_id: segment.segment_id,
            agent: AgentKind::Junie,
            scope_status: segment.scope_status,
            decision_candidates,
            gates,
            open_questions,
            handoff_signals,
            outcome,
        }
    }
}

fn extract_decision_candidates(segment_id: u32, turns: &[Turn]) -> Vec<DecisionCandidate> {
    let mut candidates = Vec::new();

    for turn in turns {
        if turn.role != TurnRole::Assistant {
            continue;
        }

        for line in turn.text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            let lower = trimmed.to_lowercase();
            let decision_text = if let Some(idx) = lower.find("[decision]") {
                let after = &trimmed[idx + "[decision]".len()..];
                after.trim_start_matches([':', ' ']).trim()
            } else if lower.starts_with("decision:") {
                trimmed["decision:".len()..].trim()
            } else if lower.starts_with("decyzja:") {
                trimmed["decyzja:".len()..].trim()
            } else if lower.starts_with("decided to ") || lower.starts_with("zdecydowano ") {
                trimmed
            } else {
                continue;
            };

            if decision_text.is_empty() {
                continue;
            }

            if candidates
                .iter()
                .any(|c: &DecisionCandidate| c.text == decision_text)
            {
                continue;
            }

            candidates.push(DecisionCandidate {
                text: decision_text.to_owned(),
                kind: "explicit_choice".to_owned(),
                evidence: EvidenceLocator {
                    segment_id,
                    turn_idx: Some(turn.turn_idx),
                    timestamp: match &turn.timestamp {
                        Known::Value(ts) => Some(ts.clone()),
                        Known::Unknown(_) => None,
                    },
                },
            });
        }
    }

    candidates
}

fn is_gate_command(cmd: &str) -> bool {
    let lower = cmd.to_lowercase();
    lower.contains("cargo test")
        || lower.contains("cargo clippy")
        || lower.contains("cargo check")
        || lower.contains("cargo fmt")
        || lower.contains("cargo build")
        || lower.contains("pytest")
        || lower.contains("npm test")
        || lower.contains("pnpm test")
        || lower.contains("yarn test")
        || lower.contains("make test")
        || lower.contains("make check")
        || lower.contains("ruff ")
        || lower.contains("eslint")
        || lower.contains("mypy")
        || lower.contains("go test")
}

fn classify_gate_outcome(output: &str) -> GateOutcome {
    let lower = output.to_lowercase();
    if output.trim().is_empty() {
        return GateOutcome::Unknown;
    }

    if lower.contains("failures:")
        || lower.contains("error[")
        || lower.contains("error:")
        || lower.contains("fatal:")
        || lower.contains("exit code 1")
        || (lower.contains("failed")
            && !lower.contains("0 failed")
            && !lower.contains("zero failed"))
    {
        return GateOutcome::Fail;
    }

    if lower.contains("test result: ok")
        || lower.contains("0 failed")
        || lower.contains("passed")
        || lower.contains("all checks passed")
        || lower.contains("all tests passed")
        || lower.contains("finished `test` profile")
        || lower.contains("finished `dev` profile")
        || lower.contains("exit code was 0")
    {
        return GateOutcome::Pass;
    }

    GateOutcome::Unknown
}

fn extract_gate_observations(segment_id: u32, turns: &[Turn]) -> Vec<GateObservation> {
    let mut observations = Vec::new();

    for (i, turn) in turns.iter().enumerate() {
        if turn.kind != TurnKind::ToolCall && turn.role != TurnRole::Tool {
            continue;
        }
        let cmd = turn.text.trim();
        if !is_gate_command(cmd) {
            continue;
        }

        let result_text = turns[i + 1..]
            .iter()
            .find(|t| t.kind == TurnKind::ToolResult)
            .map(|t| t.text.as_str())
            .unwrap_or("");

        let outcome = classify_gate_outcome(result_text);

        observations.push(GateObservation {
            command: cmd.to_owned(),
            outcome,
            evidence: EvidenceLocator {
                segment_id,
                turn_idx: Some(turn.turn_idx),
                timestamp: match &turn.timestamp {
                    Known::Value(ts) => Some(ts.clone()),
                    Known::Unknown(_) => None,
                },
            },
        });
    }

    observations
}

fn extract_open_questions(segment_id: u32, turns: &[Turn]) -> Vec<OpenQuestion> {
    let mut questions = Vec::new();

    for turn in turns {
        if turn.role != TurnRole::User && turn.kind != TurnKind::UserMsg {
            continue;
        }

        for line in turn.text.lines() {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }

            if trimmed.ends_with('?') || trimmed.to_lowercase().starts_with("question:") {
                let q_text = trimmed
                    .strip_prefix("Question:")
                    .or_else(|| trimmed.strip_prefix("question:"))
                    .unwrap_or(trimmed)
                    .trim();

                if !q_text.is_empty() && !questions.iter().any(|q: &OpenQuestion| q.text == q_text)
                {
                    questions.push(OpenQuestion {
                        kind: "unanswered_question".to_owned(),
                        text: q_text.to_owned(),
                        evidence: EvidenceLocator {
                            segment_id,
                            turn_idx: Some(turn.turn_idx),
                            timestamp: match &turn.timestamp {
                                Known::Value(ts) => Some(ts.clone()),
                                Known::Unknown(_) => None,
                            },
                        },
                    });
                }
            }
        }
    }

    questions
}

fn extract_handoff_signals(segment_id: u32, turns: &[Turn]) -> Vec<HandoffSignal> {
    let last_reply = turns
        .iter()
        .rev()
        .find(|t| t.role == TurnRole::Assistant && t.kind == TurnKind::AgentReply);

    let mut signals = Vec::new();
    if let Some(turn) = last_reply {
        let text = turn.text.trim();
        if !text.is_empty() {
            signals.push(HandoffSignal {
                text: text.to_owned(),
                evidence: EvidenceLocator {
                    segment_id,
                    turn_idx: Some(turn.turn_idx),
                    timestamp: match &turn.timestamp {
                        Known::Value(ts) => Some(ts.clone()),
                        Known::Unknown(_) => None,
                    },
                },
            });
        }
    }

    signals
}

fn determine_lane_outcome(
    model: &SessionModel,
    turns: &[Turn],
    gates: &[GateObservation],
    has_handoff: bool,
) -> LaneOutcome {
    let mut outcome = AgentOutcome::Unknown;

    let has_failing_gate = gates.iter().any(|g| g.outcome == GateOutcome::Fail);
    let has_passing_gate = gates.iter().any(|g| g.outcome == GateOutcome::Pass);

    if has_failing_gate {
        outcome = AgentOutcome::Failed;
    } else if has_handoff || has_passing_gate {
        outcome = AgentOutcome::Complete;
    } else if !turns.is_empty() {
        outcome = AgentOutcome::Partial;
    }

    let ending = if model.coverage.status.malformed_tail_present
        || model.coverage.status.visible_completeness
            == aicx_parser::engine::VisibleCompleteness::PartialVisible
    {
        Some("interrupted".to_owned())
    } else {
        None
    };

    LaneOutcome {
        agent_outcome: outcome,
        ending,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicx_parser::adapters::{AgentAdapter, JunieAdapter};
    use aicx_parser::engine::{
        RawUnitReader, ReaderPolicy, SourceArtifact, SourceFraming, SourceHandle, ValidatedParse,
        validate_parse,
    };

    fn parse_junie_events(name: &str, session_id: &str, events_jsonl: &str) -> SessionModel {
        let source = SourceHandle::new(
            AgentKind::Junie,
            format!("junie-{name}"),
            Some(session_id.to_owned()),
            vec![
                SourceArtifact::memory(
                    "events.jsonl",
                    events_jsonl.as_bytes(),
                    SourceFraming::JsonLines,
                )
                .expect("source artifact"),
            ],
        )
        .expect("source handle");

        let read = RawUnitReader::new(ReaderPolicy::default())
            .read(&source)
            .expect("read units");
        let adapter = JunieAdapter;
        let classified = adapter.classify(&source, &read).expect("classify");
        let unvalidated = adapter
            .assemble(&source, &read, classified)
            .expect("assemble");
        match validate_parse(unvalidated).expect("validate parse") {
            ValidatedParse::Session(session) => session.model().clone(),
            ValidatedParse::Fatal(_) => panic!("unexpected fatal parse"),
        }
    }

    #[test]
    fn junie_lane_matches_golden() {
        let fixture_str = include_str!("../../../tests/fixtures/tb_oracle/junie/events.jsonl");
        let model = parse_junie_events("fixture", "session-260917-175728-59hg", fixture_str);
        let lane = JunieLane::new();
        let actual = lane.distill(&model);

        let golden_str = include_str!("../../../tests/fixtures/tb_oracle/junie/golden.json");
        let expected: Vec<SegmentDistillate> =
            serde_json::from_str(golden_str).expect("parse golden.json");

        assert_eq!(actual, expected);
    }

    #[test]
    fn distillation_from_final_snapshots_only_not_in_progress() {
        let events = r#"
{"kind":"UserPromptEvent","requestId":"p-1","prompt":"Can we make a decision?","timestampMs":1789660000000}
{"kind":"TaskStartedEvent","taskId":"t-1","timestampMs":1789660000010}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"AgentThoughtBlockUpdatedEvent","stepId":"step-1","status":"IN_PROGRESS","text":"Intermediate reasoning: [decision] Rejected old path"}},"timestampMs":1789660001000}
{"kind":"SessionA2uxEvent","event":{"state":"COMPLETED","agentEvent":{"kind":"AgentThoughtBlockUpdatedEvent","stepId":"step-1","status":"COMPLETED","text":"Final reasoning: [decision] Accepted final path"}},"timestampMs":1789660002000}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"step-2","status":"IN_PROGRESS","command":"cargo test","output":""}},"timestampMs":1789660003000}
{"kind":"SessionA2uxEvent","event":{"state":"COMPLETED","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"step-2","status":"COMPLETED","command":"cargo test","output":"test result: ok. 4 passed; 0 failed"}},"timestampMs":1789660004000}
{"kind":"SessionA2uxEvent","event":{"state":"COMPLETED","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-3","status":"COMPLETED","result":"All done successfully"}},"timestampMs":1789660005000}
{"kind":"TaskState","state":"COMPLETED","timestampMs":1789660006000}
"#;
        let model = parse_junie_events("in-progress-test", "session-260917-test-inp", events);
        let lane = JunieLane::new();
        let distillates = lane.distill(&model);

        assert_eq!(distillates.len(), 1);
        let d = &distillates[0];

        // Must distill from final snapshot, not IN_PROGRESS
        assert_eq!(d.decision_candidates.len(), 1);
        assert_eq!(d.decision_candidates[0].text, "Accepted final path");
        assert!(
            !d.decision_candidates
                .iter()
                .any(|c| c.text.contains("Rejected old path"))
        );

        // Gate must have final passing outcome
        assert_eq!(d.gates.len(), 1);
        assert_eq!(d.gates[0].command, "cargo test");
        assert_eq!(d.gates[0].outcome, GateOutcome::Pass);

        // Handoff signal from final ResultBlock
        assert_eq!(d.handoff_signals.len(), 1);
        assert_eq!(d.handoff_signals[0].text, "All done successfully");
    }

    #[test]
    fn system_message_event_excluded_from_handoff_signal() {
        let events = r#"
{"kind":"SystemMessageEvent","message":"You are Junie, an autonomous programmer. Follow instructions carefully."}
{"kind":"UserPromptEvent","requestId":"p-sys","prompt":"Hello"}
{"kind":"TaskStartedEvent","taskId":"t-sys","timestampMs":1789660000010}
{"kind":"SessionA2uxEvent","event":{"state":"COMPLETED","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-res","status":"COMPLETED","result":"Handoff message to user."}},"timestampMs":1789660002000}
{"kind":"TaskState","state":"COMPLETED","timestampMs":1789660003000}
"#;
        let model = parse_junie_events("sys-test", "session-260917-test-sys", events);
        let lane = JunieLane::new();
        let distillates = lane.distill(&model);

        assert_eq!(distillates.len(), 1);
        let d = &distillates[0];

        // Handoff signals must contain only agent final response, NOT SystemMessageEvent!
        assert_eq!(d.handoff_signals.len(), 1);
        assert_eq!(d.handoff_signals[0].text, "Handoff message to user.");
        assert!(
            !d.handoff_signals
                .iter()
                .any(|h| h.text.contains("You are Junie"))
        );
    }

    #[test]
    fn mixed_sessions_produce_n_distillates() {
        use aicx_parser::engine::{ScopeStatus, TurnRange};

        let fixture_str = include_str!("../../../tests/fixtures/tb_oracle/junie/events.jsonl");
        let mut model = parse_junie_events("multi-segment", "session-260917-multi", fixture_str);

        let mid = model.turns.len() / 2;
        model.segments = vec![
            Segment {
                segment_id: 0,
                scope_status: ScopeStatus::NoDriftObserved,
                scope_conflict: false,
                scope_root: None,
                cwd: model.provenance.cwd.clone(),
                branch: model.provenance.branch.clone(),
                started_at: model.provenance.started_at.clone(),
                ended_at: model.provenance.ended_at.clone(),
                turn_range: TurnRange {
                    start: 0,
                    end: mid.saturating_sub(1) as u64,
                },
            },
            Segment {
                segment_id: 1,
                scope_status: ScopeStatus::NoDriftObserved,
                scope_conflict: false,
                scope_root: None,
                cwd: model.provenance.cwd.clone(),
                branch: model.provenance.branch.clone(),
                started_at: model.provenance.started_at.clone(),
                ended_at: model.provenance.ended_at.clone(),
                turn_range: TurnRange {
                    start: mid as u64,
                    end: (model.turns.len() - 1) as u64,
                },
            },
        ];

        let lane = JunieLane::new();
        let distillates = lane.distill(&model);

        assert_eq!(distillates.len(), 2);
        assert_eq!(distillates[0].segment_id, 0);
        assert_eq!(distillates[1].segment_id, 1);
        assert_eq!(distillates[0].agent, AgentKind::Junie);
        assert_eq!(distillates[1].agent, AgentKind::Junie);
    }

    #[test]
    fn walk_around_real_session_if_present() {
        let home = std::env::var("HOME").unwrap_or_default();
        let path = format!("{home}/.junie/sessions/session-260917-175728-59hg/events.jsonl");
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };
        let model = parse_junie_events("full-real", "session-260917-175728-59hg", &content);
        let lane = JunieLane::new();
        let distillates = lane.distill(&model);
        assert!(!distillates.is_empty());
        for d in &distillates {
            for cand in &d.decision_candidates {
                assert!(
                    !cand.text.contains("IN_PROGRESS"),
                    "leaked IN_PROGRESS in decision"
                );
            }
            for gate in &d.gates {
                assert!(
                    !gate.command.contains("IN_PROGRESS"),
                    "leaked IN_PROGRESS in gate command"
                );
            }
            for sig in &d.handoff_signals {
                assert!(
                    !sig.text.contains("IN_PROGRESS"),
                    "leaked IN_PROGRESS in handoff signal"
                );
            }
        }
    }
}
