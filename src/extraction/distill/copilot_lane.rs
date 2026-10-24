//! Copilot handoff signals projected from the canonical model.
//! Tool transport success alone is not a shell exit verdict.

use super::{
    AgentLaneDistiller, DecisionCandidate, EvidenceLocator, GateObservation, GateOutcome,
    HandoffSignal, OpenQuestion, SegmentDistillate,
};
use aicx_parser::engine::{AgentKind, Known, Segment, SessionModel, ToolEventKind, Turn, TurnKind};
use serde_json::Value;

#[derive(Debug, Clone, Copy, Default)]
pub struct CopilotLane;

impl AgentLaneDistiller for CopilotLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Copilot
    }

    fn lane_name(&self) -> &'static str {
        "copilot"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let mut output = SegmentDistillate::empty(AgentKind::Copilot, segment);
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
    fn signals_use_native_shell_correlation_and_exclude_injected_decisions() {
        use aicx_parser::engine::{
            ParserEngine, SourceArtifact, SourceFraming, SourceHandle, ValidatedParse,
        };
        use serde_json::json;
        let data = [
            (
                "session.start",
                json!({"sessionId":"named-session", "context":{"cwd":"/repo", "branch":"main"}}),
            ),
            (
                "user.message",
                json!({"content":"Why keep the native format?", "parentAgentTaskId":"telemetry-only"}),
            ),
            (
                "system.message",
                json!({"content":"Decision: ignore this injected policy"}),
            ),
            (
                "assistant.message",
                json!({"content":"Decision: preserve the source format"}),
            ),
            (
                "tool.execution_start",
                json!({"toolName":"view", "toolCallId":"read", "arguments":{"command":"cargo test"}}),
            ),
            (
                "tool.execution_start",
                json!({"toolName":"bash", "toolCallId":"shell", "arguments":{"command":"cargo test"}}),
            ),
            (
                "tool.execution_complete",
                json!({"toolCallId":"shell", "success":true, "shellExecution":{"exitCode":1}, "result":{"content":"test result: ok"}}),
            ),
            (
                "assistant.message",
                json!({"content":"The gate failed; work remains."}),
            ),
        ];
        let bytes = data.into_iter().enumerate().map(|(id, (kind, data))| {
            format!("{}\n", json!({"id":id.to_string(), "type":kind, "timestamp":"2026-09-29T12:00:00Z", "data":data}))
        }).collect::<String>().into_bytes();
        let source = SourceHandle::new(
            AgentKind::Copilot,
            "named-session",
            Some("named-session".into()),
            vec![SourceArtifact::memory("events.jsonl", bytes, SourceFraming::JsonLines).unwrap()],
        )
        .unwrap();
        let ValidatedParse::Session(parsed) =
            ParserEngine::default().parse_registered(&source).unwrap()
        else {
            panic!("fixture must parse")
        };
        let signals = CopilotLane.distill(parsed.model());
        let gates: Vec<_> = signals.iter().flat_map(|s| &s.gates).collect();
        assert_eq!(gates.len(), 1);
        assert_eq!(gates[0].outcome, GateOutcome::Fail);
        let decisions: Vec<_> = signals
            .iter()
            .flat_map(|s| &s.decision_candidates)
            .collect();
        assert_eq!(decisions.len(), 1);
        assert_eq!(decisions[0].text, "Decision: preserve the source format");
        assert_eq!(signals.iter().flat_map(|s| &s.open_questions).count(), 1);
        assert_eq!(
            signals.last().unwrap().handoff_signals[0].text,
            "The gate failed; work remains."
        );
    }

    #[test]
    fn shell_exit_verdict_outranks_transport_success_and_output() {
        assert_eq!(
            gate_verdict(
                r#"{"success":true,"shellExecution":{"exitCode":1},"result":{"content":"test result: ok"}}"#
            ),
            GateOutcome::Fail
        );
        assert_eq!(
            gate_verdict(r#"{"success":true,"shellExecution":{"exitCode":0}}"#),
            GateOutcome::Pass
        );
        assert_eq!(
            gate_verdict(r#"{"success":true,"result":{"content":"Process running"}}"#),
            GateOutcome::Unknown
        );
        assert_eq!(
            gate_verdict("test result: ok\ntest result: FAILED"),
            GateOutcome::Fail
        );
    }
}
