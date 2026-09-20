//! Kimi lane distiller (W1-06).
//!
//! Reads the kimi [`SessionModel`] produced by the wire adapter
//! (`crates/aicx-parser/src/adapters/kimi.rs`) and projects one
//! [`SegmentDistillate`] per segment. Pure projection: the model is never
//! mutated and nothing here feeds the canonical fingerprint.
//!
//! Lane decisions (see `docs/DISTILL_CONTRACT.md`):
//!
//! - **Gates are shell-only.** Kimi's only shell-shaped tool is `Bash`; a
//!   `Read`/`Grep`/`Edit` call can never be a quality gate even when its
//!   payload mentions one. The verdict is read from the correlated
//!   `tool.result` body through explicit pass/fail markers — anything else is
//!   [`GateOutcome::Unknown`], never a guessed pass.
//! - **Decisions are marker-anchored.** Assistant replies and internal
//!   thoughts are scanned line-wise for explicit markers (`Decision:`,
//!   `Plan:`); unmarked prose is not promoted to a candidate.
//! - **Outcome stays `Unknown`.** The only evidence-based signal v1 emits is
//!   `ending: "interrupted"` when the substrate reports a malformed tail.

use super::{
    AgentLaneDistiller, AgentOutcome, DecisionCandidate, EvidenceLocator, GateObservation,
    GateOutcome, HandoffSignal, LaneOutcome, OpenQuestion, SegmentDistillate,
};
use aicx_parser::engine::{AgentKind, Known, Segment, SessionModel, ToolEventKind, Turn, TurnKind};

/// Kimi's shell-shaped tool — the only tool whose calls can carry a gate.
const SHELL_TOOL_NAME: &str = "Bash";

/// Command substrings that mark a shell invocation as a quality gate.
const GATE_COMMAND_MARKERS: &[&str] = &[
    "cargo test",
    "cargo clippy",
    "cargo fmt",
    "cargo build",
    "cargo check",
    "npm test",
    "npm run test",
    "make test",
    "make check",
];

/// Result-body markers that prove a gate failed.
const GATE_FAIL_MARKERS: &[&str] = &[
    "test result: FAILED",
    "Command failed with exit code",
    "error: could not compile",
    "error[E",
];

/// Result-body markers that prove a gate passed.
const GATE_PASS_MARKERS: &[&str] = &["test result: ok", "Finished `"];

/// Line prefixes that mark an assistant statement as a decision candidate.
const DECISION_MARKERS: &[(&str, &str)] = &[
    ("decision:", "explicit_choice"),
    ("plan:", "plan_commitment"),
];

/// Kimi wire lane distiller.
#[derive(Debug, Clone, Copy, Default)]
pub struct KimiLane;

impl AgentLaneDistiller for KimiLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Kimi
    }

    fn lane_name(&self) -> &'static str {
        "kimi"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let mut distillate = SegmentDistillate::empty(AgentKind::Kimi, segment);
        let turns: Vec<&Turn> = model
            .turns
            .iter()
            .filter(|turn| turn.segment_id == segment.segment_id)
            .collect();

        distillate.gates = self.gates(model, segment);
        for turn in &turns {
            match turn.kind {
                TurnKind::AgentReply | TurnKind::InternalThought => {
                    collect_decision_candidates(turn, segment, &mut distillate);
                }
                TurnKind::UserMsg => {
                    collect_open_questions(turn, segment, &mut distillate);
                }
                _ => {}
            }
        }
        if let Some(signal) = turns
            .iter()
            .rev()
            .find(|turn| turn.kind == TurnKind::AgentReply)
        {
            distillate.handoff_signals.push(HandoffSignal {
                text: signal.text.clone(),
                evidence: locator(segment, signal),
            });
        }
        distillate.outcome = LaneOutcome {
            agent_outcome: AgentOutcome::Unknown,
            ending: model
                .coverage
                .status
                .malformed_tail_present
                .then(|| "interrupted".to_owned()),
        };
        distillate
    }
}

impl KimiLane {
    /// Gate observations from shell-shaped tool calls only: a `Bash` call
    /// whose command matches the gate vocabulary, verdicted from the
    /// correlated `tool.result` body.
    fn gates(&self, model: &SessionModel, segment: &Segment) -> Vec<GateObservation> {
        let mut gates = Vec::new();
        for event in model
            .tool_events
            .iter()
            .filter(|event| event.kind == ToolEventKind::Call)
        {
            if event.tool_name != SHELL_TOOL_NAME {
                continue;
            }
            let Some(turn) = model.turns.get(event.turn_idx as usize) else {
                continue;
            };
            if turn.segment_id != segment.segment_id {
                continue;
            }
            let command = shell_command(&turn.text);
            if !is_gate_command(&command) {
                continue;
            }
            let outcome = model
                .tool_events
                .iter()
                .find(|candidate| {
                    candidate.kind == ToolEventKind::Result
                        && candidate.correlation_id == event.correlation_id
                })
                .and_then(|result| model.turns.get(result.turn_idx as usize))
                .map_or(GateOutcome::Unknown, |result_turn| {
                    gate_verdict(&result_turn.text)
                });
            gates.push(GateObservation {
                command,
                outcome,
                evidence: locator(segment, turn),
            });
        }
        gates
    }
}

/// The tool.call turn body is the JSON-serialized args object; the shell
/// command lives on `command`. A non-JSON body (the raw `ShellAction` path)
/// is taken as the command verbatim.
fn shell_command(body: &str) -> String {
    serde_json::from_str::<serde_json::Value>(body)
        .ok()
        .and_then(|args| args.get("command")?.as_str().map(str::to_owned))
        .unwrap_or_else(|| body.trim().to_owned())
}

fn is_gate_command(command: &str) -> bool {
    GATE_COMMAND_MARKERS
        .iter()
        .any(|marker| command.contains(marker))
}

/// Verdict from the result body: explicit fail markers win over pass markers
/// (a failing `cargo test` still prints `test result: ok` for earlier suites
/// in combined output), and no marker means [`GateOutcome::Unknown`].
fn gate_verdict(result_body: &str) -> GateOutcome {
    if GATE_FAIL_MARKERS
        .iter()
        .any(|marker| result_body.contains(marker))
    {
        GateOutcome::Fail
    } else if GATE_PASS_MARKERS
        .iter()
        .any(|marker| result_body.contains(marker))
    {
        GateOutcome::Pass
    } else {
        GateOutcome::Unknown
    }
}

fn collect_decision_candidates(turn: &Turn, segment: &Segment, distillate: &mut SegmentDistillate) {
    for line in turn.text.lines() {
        // Markdown list/bold prefixes (`1. **Decision:`) are punctuation, not
        // content — strip leading non-alphabetic characters before matching.
        let trimmed = line.trim().trim_start_matches(|c: char| !c.is_alphabetic());
        let lowered = trimmed.to_lowercase();
        if let Some((_, kind)) = DECISION_MARKERS
            .iter()
            .find(|(marker, _)| lowered.starts_with(marker))
        {
            distillate.decision_candidates.push(DecisionCandidate {
                text: trimmed.to_owned(),
                kind: (*kind).to_owned(),
                evidence: locator(segment, turn),
            });
        }
    }
}

fn collect_open_questions(turn: &Turn, segment: &Segment, distillate: &mut SegmentDistillate) {
    for line in turn.text.lines() {
        let trimmed = line.trim();
        if trimmed.ends_with('?') {
            distillate.open_questions.push(OpenQuestion {
                kind: "user_question".to_owned(),
                text: trimmed.to_owned(),
                evidence: locator(segment, turn),
            });
        }
    }
}

fn locator(segment: &Segment, turn: &Turn) -> EvidenceLocator {
    EvidenceLocator {
        segment_id: segment.segment_id,
        turn_idx: Some(turn.turn_idx),
        timestamp: known_str(&turn.timestamp),
    }
}

fn known_str(value: &Known<String>) -> Option<String> {
    match value {
        Known::Value(text) => Some(text.clone()),
        Known::Unknown(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicx_parser::engine::{
        BoundaryFlags, CoverageReport, Known, ParseStatus, ParserEngine, Provenance, ScopeStatus,
        SourceArtifact, SourceFraming, SourceHandle, ToolEvent, TurnRange, TurnRole,
        ValidatedParse, VisibleCompleteness,
    };

    /// The fixture is a redacted fragment of the real kimi session that
    /// implemented this lane, captured after `05bb14e` ("do not inherit a
    /// pre-kimi watermark", 2026-09-17T10:52:27+02:00) — the commit that made
    /// kimi sessions indexable at all.
    const KIMI_WATERMARK_UTC: &str = "2026-09-17T08:52:27Z";
    const FIXTURE: &[u8] = include_bytes!("../../../tests/fixtures/tb_oracle/kimi/wire.jsonl");
    const GOLDEN: &str =
        include_str!("../../../tests/fixtures/tb_oracle/kimi/segment_distillate.golden.json");

    fn parse_fixture() -> SessionModel {
        let source = SourceHandle::new(
            AgentKind::Kimi,
            "kimi-w1-06-fixture",
            Some("35ac45cf-7f33-4fc5-8381-8f5709b14f1e".to_owned()),
            vec![
                SourceArtifact::memory("wire.jsonl", FIXTURE.to_vec(), SourceFraming::JsonLines)
                    .unwrap(),
            ],
        )
        .unwrap();
        let ValidatedParse::Session(validated) = ParserEngine::default()
            .parse_registered(&source)
            .expect("kimi fixture parses")
        else {
            panic!("kimi fixture is a session");
        };
        validated.into_model()
    }

    #[test]
    fn kimi_lane_matches_golden() {
        let model = parse_fixture();
        let distillates = KimiLane.distill(&model);
        let actual = format!("{}\n", serde_json::to_string_pretty(&distillates).unwrap());
        // Bless path: `AICX_DISTILL_BLESS=1 cargo test -p aicx distill::kimi`
        // rewrites the golden after a deliberate, eyeball-verified change.
        if std::env::var_os("AICX_DISTILL_BLESS").is_some() {
            let path = concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/tests/fixtures/tb_oracle/kimi/segment_distillate.golden.json"
            );
            std::fs::write(path, &actual).unwrap();
        }
        assert_eq!(actual, GOLDEN);
    }

    #[test]
    fn kimi_fixture_is_post_watermark() {
        let model = parse_fixture();
        let Known::Value(started_at) = &model.provenance.started_at else {
            panic!("kimi fixture carries a started_at timestamp");
        };
        assert!(
            started_at.as_str() > KIMI_WATERMARK_UTC,
            "fixture session started_at {started_at} must postdate the kimi watermark {KIMI_WATERMARK_UTC} (commit 05bb14e)"
        );
        assert_eq!(model.provenance.agent, AgentKind::Kimi);
    }

    #[test]
    fn gate_observations_come_only_from_shell_tools() {
        // A Read call whose payload mentions `cargo test` is not a gate; only
        // the Bash call is.
        let mut model = synthetic_model(1);
        model.turns = vec![
            tool_turn(0, 0, "Read", r#"{"path": "cargo test"}"#),
            tool_turn(1, 0, "Bash", r#"{"command": "cargo test -p aicx"}"#),
            tool_turn(2, 0, "Bash", "test result: ok. 3 passed; 0 failed"),
        ];
        model.tool_events = vec![
            tool_event(ToolEventKind::Call, 0, "Read", "call-read"),
            tool_event(ToolEventKind::Call, 1, "Bash", "call-bash"),
            tool_event(ToolEventKind::Result, 2, "Bash", "call-bash"),
        ];
        let lane = KimiLane;
        let segment = model.segments[0].clone();
        let distillate = lane.distill_segment(&model, &segment);
        assert_eq!(distillate.gates.len(), 1);
        assert_eq!(distillate.gates[0].command, "cargo test -p aicx");
        assert_eq!(distillate.gates[0].outcome, GateOutcome::Pass);
    }

    #[test]
    fn mixed_scope_yields_one_distillate_per_segment() {
        let mut model = synthetic_model(2);
        model.turns = vec![
            text_turn(0, 0, TurnRole::User, TurnKind::UserMsg, "workstream one"),
            text_turn(1, 1, TurnRole::User, TurnKind::UserMsg, "workstream two"),
        ];
        let distillates = KimiLane.distill(&model);
        assert_eq!(distillates.len(), 2);
        assert_eq!(distillates[0].segment_id, 0);
        assert_eq!(distillates[1].segment_id, 1);
        // scope_status is copied from the segment, never recomputed.
        assert_eq!(distillates[0].scope_status, ScopeStatus::MixedCandidate);
        assert_eq!(distillates[1].scope_status, ScopeStatus::MixedCandidate);
    }

    #[test]
    fn empty_segment_distills_to_honest_empty() {
        let model = synthetic_model(1);
        let segment = model.segments[0].clone();
        let distillate = KimiLane.distill_segment(&model, &segment);
        assert!(distillate.decision_candidates.is_empty());
        assert!(distillate.gates.is_empty());
        assert!(distillate.open_questions.is_empty());
        assert!(distillate.handoff_signals.is_empty());
        assert_eq!(distillate.outcome.agent_outcome, AgentOutcome::Unknown);
        assert_eq!(distillate.outcome.ending, None);
    }

    fn synthetic_model(segment_count: u32) -> SessionModel {
        let provenance = Provenance {
            agent: AgentKind::Kimi,
            model: Known::unknown(),
            cli_version: Known::unknown(),
            cwd: Known::unknown(),
            branch: Known::unknown(),
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
            original_source_hash: "sha256:test".to_owned(),
            original_source_bytes: 0,
        };
        let coverage = CoverageReport {
            raw_line_count: 0,
            raw_unit_count: 0,
            consumed_count: 0,
            skipped_count: 0,
            consumed_ranges: Vec::new(),
            consumed: Vec::new(),
            skipped: Vec::new(),
            warnings: Vec::new(),
            status: ParseStatus {
                visible_completeness: VisibleCompleteness::CompleteVisible,
                boundary_flags: BoundaryFlags::default(),
                malformed_tail_present: false,
                visible_event_lost: false,
            },
            consumed_by_kind: Default::default(),
            known_skipped: Default::default(),
        };
        let mut model = SessionModel::new("kimi-lane-test", provenance, coverage);
        model.segments = (0..segment_count)
            .map(|segment_id| Segment {
                segment_id,
                cwd: Known::unknown(),
                branch: Known::unknown(),
                started_at: Known::unknown(),
                ended_at: Known::unknown(),
                turn_range: TurnRange { start: 0, end: 0 },
                scope_status: ScopeStatus::MixedCandidate,
            })
            .collect();
        model
    }

    fn text_turn(
        turn_idx: u64,
        segment_id: u32,
        role: TurnRole,
        kind: TurnKind,
        text: &str,
    ) -> Turn {
        Turn {
            turn_idx,
            role,
            timestamp: Known::unknown(),
            kind,
            text_hash: format!("sha256:test-{turn_idx}"),
            text_chars: text.chars().count() as u64,
            text: text.to_owned(),
            tool_name: Known::unknown(),
            segment_id,
            raw_unit_refs: Vec::new(),
            frame_class: None,
        }
    }

    fn tool_turn(turn_idx: u64, segment_id: u32, tool_name: &str, body: &str) -> Turn {
        let mut turn = text_turn(
            turn_idx,
            segment_id,
            TurnRole::Tool,
            TurnKind::ToolCall,
            body,
        );
        turn.tool_name = Known::value(tool_name.to_owned());
        turn
    }

    fn tool_event(
        kind: ToolEventKind,
        turn_idx: u64,
        tool_name: &str,
        correlation_id: &str,
    ) -> ToolEvent {
        ToolEvent {
            kind,
            turn_idx,
            tool_name: tool_name.to_owned(),
            correlation_id: Known::value(correlation_id.to_owned()),
            payload_hash: "sha256:test".to_owned(),
            payload_bytes: 0,
            raw_unit_refs: Vec::new(),
        }
    }
}
