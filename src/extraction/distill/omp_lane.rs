//! oh-my-pi / pi handoff signals projected from the canonical model.
//! Tool transport success alone is not a shell exit verdict.

use super::{
    AgentLaneDistiller, DecisionCandidate, EvidenceLocator, GateObservation, GateOutcome,
    HandoffSignal, OpenQuestion, SegmentDistillate,
};
use aicx_parser::engine::{AgentKind, Known, Segment, SessionModel, ToolEventKind, Turn, TurnKind};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default)]
pub struct OmpLane;

impl AgentLaneDistiller for OmpLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Omp
    }

    fn lane_name(&self) -> &'static str {
        "omp"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let mut output = SegmentDistillate::empty(AgentKind::Omp, segment);
        let mut last_reply = None;
        for turn in model
            .turns
            .iter()
            .filter(|t| t.segment_id == segment.segment_id)
        {
            match turn.kind {
                TurnKind::AgentReply | TurnKind::InternalThought => {
                    for line in turn.text.lines() {
                        let text = line.trim().trim_start_matches(|c: char| !c.is_alphabetic());
                        let lower = text.to_lowercase();
                        let kind = if lower.starts_with("decision:") {
                            Some("explicit_choice")
                        } else if lower.starts_with("plan:") {
                            Some("plan_commitment")
                        } else {
                            None
                        };
                        if let Some(kind) = kind {
                            output.decision_candidates.push(DecisionCandidate {
                                text: text.to_owned(),
                                kind: kind.to_owned(),
                                evidence: locator(segment, turn),
                            });
                        }
                    }
                    if turn.kind == TurnKind::AgentReply && !turn.text.trim().is_empty() {
                        last_reply = Some(turn);
                    }
                }
                TurnKind::UserMsg => {
                    for line in turn.text.lines().filter(|l| l.trim().ends_with('?')) {
                        output.open_questions.push(OpenQuestion {
                            kind: "user_question".into(),
                            text: line.trim().to_owned(),
                            evidence: locator(segment, turn),
                        });
                    }
                }
                _ => {}
            }
        }
        if let Some(turn) = last_reply {
            output.handoff_signals.push(HandoffSignal {
                text: turn.text.clone(),
                evidence: locator(segment, turn),
            });
        }
        for call in model
            .tool_events
            .iter()
            .filter(|e| e.kind == ToolEventKind::Call)
        {
            if !matches!(
                call.tool_name.to_ascii_lowercase().as_str(),
                "bash" | "powershell" | "shell"
            ) {
                continue;
            }
            let Some(turn) = model.turns.get(call.turn_idx as usize) else {
                continue;
            };
            if turn.segment_id != segment.segment_id {
                continue;
            }
            let args = serde_json::from_str::<Value>(&turn.text).ok();
            let command = args
                .as_ref()
                .and_then(|v| v.get("command"))
                .and_then(Value::as_str)
                .map(str::to_owned)
                .unwrap_or_else(|| turn.text.trim().to_owned());
            if ![
                "cargo test",
                "cargo clippy",
                "cargo check",
                "cargo build",
                "cargo fmt",
                "npm test",
                "npm run test",
                "pnpm test",
                "pytest",
                "make test",
                "make check",
            ]
            .iter()
            .any(|marker| command.contains(marker))
            {
                continue;
            }
            let result = model
                .tool_events
                .iter()
                .find(|e| {
                    e.kind == ToolEventKind::Result
                        && matches!(&call.correlation_id, Known::Value(_))
                        && e.turn_idx > call.turn_idx
                        && e.correlation_id == call.correlation_id
                })
                .and_then(|e| model.turns.get(e.turn_idx as usize));
            output.gates.push(GateObservation {
                command,
                outcome: result.map_or(GateOutcome::Unknown, |t| gate_verdict(&t.text)),
                evidence: locator(segment, turn),
            });
        }
        if model.coverage.status.malformed_tail_present {
            output.outcome.ending = Some("interrupted".into());
        }
        output
    }
}

fn locator(segment: &Segment, turn: &Turn) -> EvidenceLocator {
    EvidenceLocator {
        segment_id: segment.segment_id,
        turn_idx: Some(turn.turn_idx),
        timestamp: match &turn.timestamp {
            Known::Value(v) => Some(v.clone()),
            Known::Unknown(_) => None,
        },
    }
}

fn gate_verdict(body: &str) -> GateOutcome {
    let json = serde_json::from_str::<Value>(body).ok();
    let exit_code = json
        .as_ref()
        .and_then(|v| {
            v.pointer("/shellExecution/exitCode")
                .or_else(|| v.pointer("/result/exitCode"))
                .or_else(|| v.get("exitCode"))
                .or_else(|| v.get("exit_code"))
        })
        .and_then(Value::as_i64);
    if exit_code.is_some_and(|code| code != 0)
        || json
            .as_ref()
            .and_then(|v| v.get("success"))
            .and_then(Value::as_bool)
            == Some(false)
        || [
            "test result: FAILED",
            "error: could not compile",
            "error[E",
            "Command failed with exit code",
        ]
        .iter()
        .any(|marker| body.contains(marker))
    {
        GateOutcome::Fail
    } else if exit_code == Some(0) || body.contains("test result: ok") {
        GateOutcome::Pass
    } else {
        GateOutcome::Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_shell_gate_passes_and_injected_user_text_is_not_a_decision() {
        use aicx_parser::engine::{
            ParserEngine, SourceArtifact, SourceFraming, SourceHandle, ValidatedParse,
        };
        let bytes = include_bytes!("../../../tests/fixtures/parser_engine/omp/session.jsonl");
        let source = SourceHandle::new(
            AgentKind::Omp,
            "01999999-aaaa-7666-8666-111111111111",
            Some("01999999-aaaa-7666-8666-111111111111".into()),
            vec![
                SourceArtifact::memory("session.jsonl", bytes.to_vec(), SourceFraming::JsonLines)
                    .unwrap(),
            ],
        )
        .unwrap();
        let ValidatedParse::Session(parsed) =
            ParserEngine::default().parse_registered(&source).unwrap()
        else {
            panic!("fixture must parse")
        };
        let signals = OmpLane.distill(parsed.model());
        let gates: Vec<_> = signals.iter().flat_map(|s| &s.gates).collect();
        let passed = gates
            .iter()
            .find(|gate| gate.command.contains("cargo test"));
        let passed = passed.expect("cargo test gate");
        assert_eq!(passed.outcome, GateOutcome::Pass);
        assert!(
            gates
                .iter()
                .any(|gate| gate.command.contains("cargo fmt") && gate.outcome == GateOutcome::Fail),
            "bashExecution exit code stays a gate: {gates:?}"
        );
        let decisions: Vec<_> = signals
            .iter()
            .flat_map(|s| &s.decision_candidates)
            .map(|candidate| candidate.text.as_str())
            .collect();
        assert!(
            !decisions.iter().any(|text| text.contains("Vibecrafted")),
            "dispatch brief is not a decision: {decisions:?}"
        );
    }
}
