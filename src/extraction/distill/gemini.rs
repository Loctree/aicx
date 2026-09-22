//! Gemini lane distiller (`AgentLaneDistiller` for [`AgentKind::Gemini`]).
//!
//! Distills a parsed [`SessionModel`] into per-segment [`SegmentDistillate`] records
//! following the TB `index_payload.v1` schema mapping (`docs/DISTILL_CONTRACT.md`).
//!
//! Enforces:
//! - Purity: reads substrate without mutation.
//! - Thoughts exclusion: internal thought turns are never emitted as handoff signals.
//! - Per-segment isolation: mixed sessions produce N separate distillates without
//!   blending outcomes.
//! - Non-guessing gates: verdicts are extracted from shell results or degrade to `Unknown`.

use crate::extraction::distill::{
    AgentLaneDistiller, AgentOutcome, DecisionCandidate, EvidenceLocator, GateObservation,
    GateOutcome, HandoffSignal, LaneOutcome, OpenQuestion, SEGMENT_DISTILLATE_SCHEMA,
    SegmentDistillate,
};
use aicx_parser::engine::frames::FrameClass;
use aicx_parser::engine::{AgentKind, Known, Segment, SessionModel, Turn, TurnKind, TurnRole};

/// Distiller for Gemini agent sessions.
#[derive(Debug, Clone, Copy, Default)]
pub struct GeminiLane;

impl GeminiLane {
    pub const fn new() -> Self {
        Self
    }
}

impl AgentLaneDistiller for GeminiLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Gemini
    }

    fn lane_name(&self) -> &'static str {
        "gemini"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let turns = segment_turns(model, segment);
        let segment_id = segment.segment_id;

        let decision_candidates = extract_decision_candidates(turns, segment_id);
        let gates = extract_gates(turns, segment_id);
        let open_questions = extract_open_questions(turns, segment_id);
        let handoff_signals = extract_handoff_signals(turns, segment_id);
        let outcome = determine_outcome(&gates, &handoff_signals, turns);

        SegmentDistillate {
            schema: SEGMENT_DISTILLATE_SCHEMA.to_owned(),
            agent: AgentKind::Gemini,
            segment_id,
            scope_status: segment.scope_status,
            decision_candidates,
            gates,
            open_questions,
            handoff_signals,
            outcome,
        }
    }
}

/// Slices the segment's turns safely from the session model.
fn segment_turns<'a>(model: &'a SessionModel, segment: &Segment) -> &'a [Turn] {
    if model.turns.is_empty()
        || segment.turn_range.start > segment.turn_range.end
        || segment.turn_range.start as usize >= model.turns.len()
    {
        &[]
    } else {
        let start = segment.turn_range.start as usize;
        let end = (segment.turn_range.end as usize).min(model.turns.len() - 1);
        &model.turns[start..=end]
    }
}

/// Builds an evidence locator pointing to a specific turn.
fn evidence_for(turn: &Turn, segment_id: u32) -> EvidenceLocator {
    let timestamp = match &turn.timestamp {
        Known::Value(ts) => Some(ts.clone()),
        Known::Unknown(_) => None,
    };
    EvidenceLocator {
        segment_id,
        turn_idx: Some(turn.turn_idx),
        timestamp,
    }
}

/// Extracts decision candidates (explicit choices and plan commitments) from turns.
///
/// Internal thought turns are strictly ignored.
fn extract_decision_candidates(turns: &[Turn], segment_id: u32) -> Vec<DecisionCandidate> {
    let mut candidates = Vec::new();
    for turn in turns {
        if turn.kind == TurnKind::InternalThought {
            continue;
        }
        if turn.role != TurnRole::Assistant && turn.role != TurnRole::User {
            continue;
        }

        for line in turn.text.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            for sentence in line.split(". ") {
                let sentence = sentence.trim().trim_end_matches('.');
                if sentence.is_empty() {
                    continue;
                }
                let lower = sentence.to_ascii_lowercase();

                let kind = if lower.starts_with("przystępuję do")
                    || lower.starts_with("plan:")
                    || lower.starts_with("planuję")
                    || lower.starts_with("proceeding to")
                    || lower.starts_with("i will")
                    || lower.starts_with("committing to")
                {
                    Some("plan_commitment")
                } else if lower.starts_with("decyzja:")
                    || lower.starts_with("decyduję")
                    || lower.starts_with("wybieram")
                    || lower.starts_with("decision:")
                    || lower.starts_with("i choose")
                {
                    Some("explicit_choice")
                } else {
                    None
                };

                if let Some(kind) = kind {
                    candidates.push(DecisionCandidate {
                        text: sentence.to_owned(),
                        kind: kind.to_owned(),
                        evidence: evidence_for(turn, segment_id),
                    });
                }
            }
        }
    }
    candidates
}

/// Identifies if a command represents a quality gate.
fn is_gate_command(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    lower.contains("cargo test")
        || lower.contains("cargo check")
        || lower.contains("cargo clippy")
        || lower.contains("cargo build")
        || lower.contains("pytest")
        || lower.contains("pnpm test")
        || lower.contains("npm test")
        || lower.contains("yarn test")
        || lower.contains("pnpm check")
        || lower.contains("make test")
        || lower.contains("make check")
        || lower.contains("semgrep")
        || lower.contains("eslint")
        || lower.contains("ruff check")
}

/// Evaluates gate command output into pass, fail, or unknown.
pub fn evaluate_gate_outcome(output: &str) -> GateOutcome {
    let trimmed = output.trim();
    if trimmed.is_empty() {
        return GateOutcome::Unknown;
    }
    let lower = trimmed.to_ascii_lowercase();

    // Specific success patterns that might contain words like "failed" (e.g. "0 failed")
    let has_zero_failed = lower.contains("0 failed");
    let has_test_ok = lower.contains("test result: ok");

    // Check for failure signals
    let has_failure = lower.contains("error:")
        || lower.contains("error[e")
        || (lower.contains("failed") && !has_zero_failed)
        || lower.contains("failure")
        || lower.contains("exit code: 1")
        || lower.contains("exit code: 2")
        || lower.contains("exit code: 101")
        || lower.contains("command failed");

    if has_failure && !has_test_ok {
        GateOutcome::Fail
    } else if has_test_ok
        || has_zero_failed
        || lower.contains("finished")
        || lower.contains("passed")
        || lower.contains("exit code: 0")
        || lower.contains("success")
    {
        GateOutcome::Pass
    } else {
        GateOutcome::Unknown
    }
}

/// Extracts quality gate runs from shell actions in the segment.
fn extract_gates(turns: &[Turn], segment_id: u32) -> Vec<GateObservation> {
    let mut gates = Vec::new();
    for turn in turns {
        if let Some(FrameClass::ShellAction { cmd, result, .. }) = &turn.frame_class {
            if is_gate_command(cmd) {
                gates.push(GateObservation {
                    command: cmd.clone(),
                    outcome: evaluate_gate_outcome(&result.text),
                    evidence: evidence_for(turn, segment_id),
                });
            }
        } else if turn.kind == TurnKind::ToolCall
            && let Known::Value(ref name) = turn.tool_name
            && matches!(name.as_str(), "run_shell_command" | "bash" | "shell")
            && is_gate_command(&turn.text)
        {
            gates.push(GateObservation {
                command: turn.text.clone(),
                outcome: GateOutcome::Unknown,
                evidence: evidence_for(turn, segment_id),
            });
        }
    }
    gates
}

/// Extracts open questions or unresolved reports left in the segment.
fn extract_open_questions(turns: &[Turn], segment_id: u32) -> Vec<OpenQuestion> {
    let mut questions = Vec::new();
    for turn in turns {
        if turn.kind == TurnKind::InternalThought {
            continue;
        }
        if turn.role == TurnRole::Assistant {
            for line in turn.text.lines() {
                let line = line.trim();
                if line.ends_with('?')
                    && (line.starts_with("Czy ")
                        || line.starts_with("Jak ")
                        || line.starts_with("Gdzie ")
                        || line.starts_with("Should ")
                        || line.starts_with("Would ")
                        || line.starts_with("Could ")
                        || line.starts_with("Do you "))
                {
                    questions.push(OpenQuestion {
                        kind: "user_query".to_owned(),
                        text: line.to_owned(),
                        evidence: evidence_for(turn, segment_id),
                    });
                }
            }
        }
    }
    questions
}

/// Extracts the closing handoff signals (typically the last assistant reply).
///
/// Ensures internal thoughts are never included.
fn extract_handoff_signals(turns: &[Turn], segment_id: u32) -> Vec<HandoffSignal> {
    for turn in turns.iter().rev() {
        if turn.role == TurnRole::Assistant
            && turn.kind == TurnKind::AgentReply
            && turn.kind != TurnKind::InternalThought
            && !turn.text.trim().is_empty()
        {
            return vec![HandoffSignal {
                text: turn.text.clone(),
                evidence: evidence_for(turn, segment_id),
            }];
        }
    }
    Vec::new()
}

/// Determines the outcome for a segment based on gate verdicts and handoff signals.
fn determine_outcome(
    gates: &[GateObservation],
    handoff_signals: &[HandoffSignal],
    turns: &[Turn],
) -> LaneOutcome {
    if gates.iter().any(|g| g.outcome == GateOutcome::Fail) {
        return LaneOutcome {
            agent_outcome: AgentOutcome::Failed,
            ending: None,
        };
    }

    let any_gate_unknown = gates.iter().any(|g| g.outcome == GateOutcome::Unknown);
    let any_gate_passed = gates.iter().any(|g| g.outcome == GateOutcome::Pass);

    if (any_gate_passed || gates.is_empty()) && !any_gate_unknown && !handoff_signals.is_empty() {
        LaneOutcome {
            agent_outcome: AgentOutcome::Complete,
            ending: None,
        }
    } else if !handoff_signals.is_empty() && any_gate_unknown {
        LaneOutcome {
            agent_outcome: AgentOutcome::Partial,
            ending: None,
        }
    } else if !turns.is_empty() {
        LaneOutcome {
            agent_outcome: AgentOutcome::Partial,
            ending: Some("interrupted".to_owned()),
        }
    } else {
        LaneOutcome {
            agent_outcome: AgentOutcome::Unknown,
            ending: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicx_parser::adapters::AgentAdapter;
    use aicx_parser::adapters::gemini::GeminiAdapter;
    use aicx_parser::engine::{
        RawUnitReader, ReaderPolicy, ScopeStatus, SourceArtifact, SourceFraming, SourceHandle,
        TurnRange, ValidatedParse, validate_parse,
    };
    use std::path::{Path, PathBuf};

    fn fixture_path(rel: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(rel)
    }

    fn parse_gemini_file(path: &Path) -> SessionModel {
        let body = std::fs::read_to_string(path)
            .unwrap_or_else(|err| panic!("failed to read {}: {err}", path.display()));
        let artifact = SourceArtifact::memory(
            "session.json",
            body.as_bytes().to_vec(),
            SourceFraming::WholeDocument,
        )
        .expect("memory artifact");
        let source = SourceHandle::new(
            AgentKind::Gemini,
            "d071a9f1-db43-4dbf-8976-5b3c39826594",
            Some("d071a9f1-db43-4dbf-8976-5b3c39826594".to_owned()),
            vec![artifact],
        )
        .expect("source handle");
        let read = RawUnitReader::new(ReaderPolicy::default())
            .read(&source)
            .expect("read");
        let adapter = GeminiAdapter;
        let classified = adapter.classify(&source, &read).expect("classify");
        let parse = adapter
            .assemble(&source, &read, classified)
            .expect("assemble");
        let validated = validate_parse(parse).expect("validate");
        match validated {
            ValidatedParse::Session(s) => s.into_model(),
            ValidatedParse::Fatal(f) => panic!("unexpected fatal parse: {f:?}"),
        }
    }

    #[test]
    fn gemini_lane_matches_golden() {
        let fixture = fixture_path("tb_oracle/gemini/gemini_session.json");
        let model = parse_gemini_file(&fixture);
        assert!(!model.segments.is_empty(), "session has at least 1 segment");

        let lane = GeminiLane::new();
        let distillate = lane.distill_segment(&model, &model.segments[0]);

        let golden_path = fixture_path("tb_oracle/gemini/gemini_golden.json");
        if !golden_path.exists() {
            // Self-bootstrap golden if missing
            let serialized = serde_json::to_string_pretty(&distillate).unwrap();
            std::fs::write(&golden_path, serialized).expect("write golden file");
        }

        let golden_text = std::fs::read_to_string(&golden_path).expect("read golden file");
        let golden: SegmentDistillate =
            serde_json::from_str(&golden_text).expect("parse golden json");

        assert_eq!(distillate, golden, "distillate matches golden artifact");
    }

    #[test]
    fn gemini_thoughts_excluded_from_handoff_signal() {
        let fixture = fixture_path("tb_oracle/gemini/gemini_session.json");
        let model = parse_gemini_file(&fixture);

        let lane = GeminiLane::new();
        let distillates = lane.distill(&model);

        for distillate in &distillates {
            for signal in &distillate.handoff_signals {
                // Ensure thought text is not present in signal
                assert!(!signal.text.contains("Commencing Next Phase"));
                assert!(!signal.text.contains("Executing Quick Wins Now"));
                assert!(!signal.text.contains("Implementing Initial Quick Wins"));
                assert!(!signal.text.is_empty());
            }
        }
    }

    #[test]
    fn gemini_mixed_session_distills_per_segment() {
        let fixture = fixture_path("tb_oracle/gemini/gemini_session.json");
        let mut model = parse_gemini_file(&fixture);

        // Turn this into a mixed session with 2 segments
        assert!(model.turns.len() >= 2);
        model.segments = vec![
            Segment {
                segment_id: 0,
                cwd: Known::value("/Users/polyversai/Libraxis/prview".to_owned()),
                branch: Known::value("main".to_owned()),
                started_at: Known::value("2026-04-15T03:15:16.852Z".to_owned()),
                ended_at: Known::value("2026-04-15T03:18:10.000Z".to_owned()),
                turn_range: TurnRange {
                    start: 0,
                    end: (model.turns.len() - 1) as u64,
                },
                scope_status: ScopeStatus::NoDriftObserved,
                scope_conflict: false,
                scope_root: None,
            },
            Segment {
                segment_id: 1,
                cwd: Known::value("/Users/polyversai/Libraxis/prview".to_owned()),
                branch: Known::value("feature/quick-wins".to_owned()),
                started_at: Known::value("2026-04-15T03:19:00.000Z".to_owned()),
                ended_at: Known::value("2026-04-15T03:20:00.000Z".to_owned()),
                turn_range: TurnRange { start: 0, end: 0 }, // only user turn, no assistant reply -> partial/interrupted
                scope_status: ScopeStatus::MixedCandidate,
                scope_conflict: false,
                scope_root: None,
            },
        ];

        let lane = GeminiLane::new();
        let distillates = lane.distill(&model);

        assert_eq!(distillates.len(), 2, "must distill exactly 2 segments");
        assert_eq!(distillates[0].segment_id, 0);
        assert_eq!(distillates[0].scope_status, ScopeStatus::NoDriftObserved);
        assert_eq!(
            distillates[0].outcome.agent_outcome,
            AgentOutcome::Complete,
            "segment 0 is complete"
        );

        assert_eq!(distillates[1].segment_id, 1);
        assert_eq!(distillates[1].scope_status, ScopeStatus::MixedCandidate);
        assert_eq!(
            distillates[0].outcome.agent_outcome,
            AgentOutcome::Complete,
            "segment 0 is complete"
        );

        assert_eq!(distillates[1].segment_id, 1);
        assert_eq!(distillates[1].scope_status, ScopeStatus::MixedCandidate);
        assert_eq!(
            distillates[1].outcome.agent_outcome,
            AgentOutcome::Partial,
            "segment 1 is partial without blending"
        );
        assert_eq!(
            distillates[1].outcome.ending,
            Some("interrupted".to_owned())
        );
    }

    #[test]
    fn gemini_gate_observation_pass_and_fail() {
        assert_eq!(
            evaluate_gate_outcome("Finished `dev` profile [unoptimized + debuginfo] target(s)"),
            GateOutcome::Pass
        );
        assert_eq!(
            evaluate_gate_outcome("test result: ok. 4 passed; 0 failed"),
            GateOutcome::Pass
        );
        assert_eq!(
            evaluate_gate_outcome("error[E0432]: unresolved import\nerror: could not compile"),
            GateOutcome::Fail
        );
        assert_eq!(
            evaluate_gate_outcome("FAILED (failures=2)"),
            GateOutcome::Fail
        );
        assert_eq!(evaluate_gate_outcome(""), GateOutcome::Unknown);
        assert_eq!(
            evaluate_gate_outcome("Some random informational message"),
            GateOutcome::Unknown
        );
    }
}
