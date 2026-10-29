//! Antigravity CLI handoff signals projected from the canonical model.
//! A rendered tool result is not a shell exit verdict by itself.

use super::{
    AgentLaneDistiller, DecisionCandidate, EvidenceLocator, GateObservation, GateOutcome,
    HandoffSignal, OpenQuestion, SegmentDistillate,
};
use aicx_parser::engine::{AgentKind, Known, Segment, SessionModel, ToolEventKind, Turn, TurnKind};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default)]
pub struct AgyLane;

impl AgentLaneDistiller for AgyLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Agy
    }

    fn lane_name(&self) -> &'static str {
        "agy"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let mut output = SegmentDistillate::empty(AgentKind::Agy, segment);
        let mut last_reply = None;
        for turn in model
            .turns
            .iter()
            .filter(|turn| turn.segment_id == segment.segment_id)
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
                    for line in turn.text.lines().filter(|line| line.trim().ends_with('?')) {
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
            .filter(|event| event.kind == ToolEventKind::Call)
        {
            if !matches!(
                call.tool_name.to_ascii_lowercase().as_str(),
                "bash" | "powershell" | "shell" | "run_command"
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
                .and_then(|value| value.get("command"))
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
                .find(|event| {
                    event.kind == ToolEventKind::Result
                        && matches!(&call.correlation_id, Known::Value(_))
                        && event.turn_idx > call.turn_idx
                        && event.correlation_id == call.correlation_id
                })
                .and_then(|event| model.turns.get(event.turn_idx as usize));
            output.gates.push(GateObservation {
                command,
                outcome: result.map_or(GateOutcome::Unknown, |turn| gate_verdict(&turn.text)),
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
            Known::Value(value) => Some(value.clone()),
            Known::Unknown(_) => None,
        },
    }
}

fn gate_verdict(body: &str) -> GateOutcome {
    let json = serde_json::from_str::<Value>(body).ok();
    let exit_code = json
        .as_ref()
        .and_then(|value| {
            value
                .pointer("/shellExecution/exitCode")
                .or_else(|| value.pointer("/result/exitCode"))
                .or_else(|| value.get("exitCode"))
                .or_else(|| value.get("exit_code"))
        })
        .and_then(Value::as_i64);
    if exit_code.is_some_and(|code| code != 0)
        || json
            .as_ref()
            .and_then(|value| value.get("success"))
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
    use aicx_parser::engine::{ParserEngine, SourceArtifact, SourceFraming, SourceHandle};

    #[test]
    fn fixture_projects_decision_question_and_run_command_gate() {
        let bytes = include_bytes!("../../../tests/fixtures/parser_engine/agy/cli.jsonl");
        let source = SourceHandle::new(
            AgentKind::Agy,
            "11111111-1111-4111-8111-111111111111",
            Some("11111111-1111-4111-8111-111111111111".into()),
            vec![
                SourceArtifact::memory("cli.jsonl", bytes.to_vec(), SourceFraming::JsonLines)
                    .unwrap(),
            ],
        )
        .unwrap();
        let parsed = ParserEngine::default().parse_registered(&source).unwrap();
        let aicx_parser::engine::ValidatedParse::Session(session) = parsed else {
            panic!("agy fixture must retain a model");
        };
        let model = session.model();
        let segment = model.segments.first().expect("fixture keeps one window");
        let distillate = AgyLane.distill_segment(model, segment);
        assert!(
            distillate
                .decision_candidates
                .iter()
                .any(|candidate| candidate.text.contains("agypipelineinitial91dbea")),
            "{distillate:?}"
        );
        assert!(
            distillate
                .open_questions
                .iter()
                .any(|question| question.text.contains("lexical search"))
        );
        assert!(
            distillate.gates.iter().any(|gate| {
                gate.command.contains("cargo test") && gate.outcome == GateOutcome::Pass
            }),
            "{distillate:?}"
        );
        assert!(
            distillate
                .open_questions
                .iter()
                .all(|question| !question.text.contains("AGY_DISPATCH_NOT_INTENT"))
        );
    }
}
