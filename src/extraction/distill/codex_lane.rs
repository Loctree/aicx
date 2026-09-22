//! Codex lane distiller (W1-02).
//!
//! Projects a parsed Codex rollout ([`SessionModel`]) into per-segment
//! [`SegmentDistillate`]s under the W0 contract (`docs/DISTILL_CONTRACT.md`).
//! Lane-specific rules, all read from the substrate, never re-guessed:
//!
//! * `<user_shell_command>` envelopes arrive as `ToolCall` turns whose
//!   `frame_class` is `ShellAction { executor: Human }` (adapter contract) —
//!   they are the operator's own runs and NEVER become [`GateObservation`]s.
//! * `reasoning` / `encrypted_reasoning` arrive as `InternalThought` turns —
//!   they are never handoff signals and never decision evidence.
//! * Agent tool calls (`exec`, `function_call`, `custom_tool_call`) become
//!   gate observations only when the recorded command actually starts a known
//!   quality gate; a gate name quoted inside an `rg` pattern is not a run.
//! * Gate verdicts are read from the correlated tool result
//!   (`SessionModel::tool_events` pairing); no result → `Unknown`, not pass.
//! * `turn_aborted` (SystemNote `"turn aborted: …"`) is the TB `ending`
//!   vocabulary passthrough `interrupted`; an aborted segment keeps
//!   `AgentOutcome::Unknown` — the abort says the tail is missing, not what
//!   the work amounted to.

use super::{
    AgentLaneDistiller, AgentOutcome, DecisionCandidate, EvidenceLocator, GateObservation,
    GateOutcome, HandoffSignal, LaneOutcome, OpenQuestion, SegmentDistillate,
};
use aicx_parser::engine::{
    AgentKind, FrameClass, Known, Segment, SessionModel, ShellExecutor, ToolEventKind, Turn,
    TurnKind,
};

/// Quality-gate command prefixes this lane recognizes. Matched only at a
/// command start (whole command or after `&&` / `;`), never as a substring —
/// `rg -n 'pytest'` searches for a gate, it does not run one.
const GATE_MARKERS: [&str; 13] = [
    "cargo test",
    "cargo clippy",
    "cargo check",
    "cargo build",
    "cargo fmt",
    "pytest",
    "pnpm test",
    "pnpm check",
    "npm test",
    "yarn test",
    "go test",
    "make test",
    "semgrep",
];

/// The codex per-agent distiller lane.
#[derive(Debug, Clone, Copy)]
pub struct CodexLane;

impl AgentLaneDistiller for CodexLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Codex
    }

    fn lane_name(&self) -> &'static str {
        "codex"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let mut distillate = SegmentDistillate::empty(AgentKind::Codex, segment);
        let turns: Vec<&Turn> = model
            .turns
            .iter()
            .filter(|turn| turn.segment_id == segment.segment_id)
            .collect();

        let mut aborted = false;
        for turn in &turns {
            match turn.kind {
                TurnKind::ToolCall if !is_human_shell(turn) => {
                    if let Some(command) = gate_command(&turn.text) {
                        distillate.gates.push(GateObservation {
                            outcome: gate_outcome(model, turn.turn_idx),
                            command,
                            evidence: locator(segment, turn),
                        });
                    }
                }
                TurnKind::AgentReply => {
                    collect_decisions(&mut distillate.decision_candidates, segment, turn);
                }
                TurnKind::SystemNote if turn.text.starts_with("turn aborted") => {
                    aborted = true;
                }
                // InternalThought (reasoning / encrypted_reasoning) is
                // deliberately not read: never a handoff signal (C0A).
                _ => {}
            }
        }

        // Handoff signal: the segment's last assistant reply, verbatim.
        let last_reply = turns
            .iter()
            .rev()
            .find(|turn| turn.kind == TurnKind::AgentReply && !turn.text.trim().is_empty());
        if let Some(turn) = last_reply {
            distillate.handoff_signals.push(HandoffSignal {
                text: turn.text.clone(),
                evidence: locator(segment, turn),
            });
            if let Some(question) = trailing_question(&turn.text) {
                distillate.open_questions.push(OpenQuestion {
                    kind: "agent_question".to_owned(),
                    text: question,
                    evidence: locator(segment, turn),
                });
            }
        }

        distillate.outcome = LaneOutcome {
            ending: aborted.then(|| "interrupted".to_owned()),
            agent_outcome: segment_outcome(&turns, aborted),
        };
        distillate
    }
}

/// True for the `<user_shell_command>` envelope: a shell action the *human*
/// ran. The frame class is the authority; the adapter's `tool_name` marker is
/// the fallback for turns parsed before the executor axis existed.
fn is_human_shell(turn: &Turn) -> bool {
    if let Some(FrameClass::ShellAction { executor, .. }) = &turn.frame_class {
        return *executor == ShellExecutor::Human;
    }
    matches!(&turn.tool_name, Known::Value(name) if name == "user_shell_command")
}

/// Extract the shell command an agent tool call recorded, if any, and return
/// it when it starts a known quality gate. Codex serializes exec calls as
/// `tools.exec_command({"cmd":"…"})` (or `cmd:"…"`); when no `cmd` payload is
/// present the whole turn body is tested as the command.
fn gate_command(text: &str) -> Option<String> {
    let command = extract_cmd(text).unwrap_or_else(|| text.to_owned());
    is_gate(&command).then_some(command)
}

/// Pull the `cmd` string literal out of a serialized exec payload,
/// JSON-unescaping the common sequences the substrate uses.
fn extract_cmd(text: &str) -> Option<String> {
    let start = text
        .find("\"cmd\":\"")
        .map(|idx| idx + 7)
        .or_else(|| text.find("cmd:\"").map(|idx| idx + 5))?;
    let raw = &text[start..];
    let mut command = String::new();
    let mut chars = raw.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '"' => break,
            '\\' => match chars.next() {
                Some('n') => command.push('\n'),
                Some('t') => command.push('\t'),
                Some(other) => command.push(other),
                None => break,
            },
            other => command.push(other),
        }
    }
    (!command.is_empty()).then_some(command)
}

/// A command is a gate only when a gate marker opens the command or one of
/// its `&&` / `;` stages (leading `ENV=value` assignments stripped). A pipe
/// stage is not a boundary: `rg 'pytest|cargo test'` must not match.
fn is_gate(command: &str) -> bool {
    command
        .split(&['\n'][..])
        .flat_map(|line| line.split("&&"))
        .flat_map(|stage| stage.split(';'))
        .any(|stage| {
            let mut stage = stage.trim();
            while let Some((head, rest)) = stage.split_once(char::is_whitespace) {
                let is_env = head.split_once('=').is_some_and(|(name, _)| {
                    !name.is_empty()
                        && name
                            .chars()
                            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
                });
                if is_env {
                    stage = rest.trim_start();
                } else {
                    break;
                }
            }
            GATE_MARKERS.iter().any(|marker| stage.starts_with(marker))
        })
}

/// Read the gate verdict from the tool result correlated with the call at
/// `call_turn_idx`. The verdict is read, never guessed: no correlated result
/// or no readable exit signal → `Unknown`.
fn gate_outcome(model: &SessionModel, call_turn_idx: u64) -> GateOutcome {
    let Some(call) = model
        .tool_events
        .iter()
        .find(|event| event.kind == ToolEventKind::Call && event.turn_idx == call_turn_idx)
    else {
        return GateOutcome::Unknown;
    };
    let Known::Value(correlation) = &call.correlation_id else {
        return GateOutcome::Unknown;
    };
    let Some(result) = model.tool_events.iter().find(|event| {
        event.kind == ToolEventKind::Result
            && matches!(&event.correlation_id, Known::Value(id) if id == correlation)
    }) else {
        return GateOutcome::Unknown;
    };
    model
        .turns
        .get(result.turn_idx as usize)
        .map_or(GateOutcome::Unknown, |turn| read_verdict(&turn.text))
}

/// Read a pass/fail verdict from result text. Recognized signals: an exit
/// code (`"exit_code":N` / `Exit code: N`) and the cargo test summary line.
fn read_verdict(text: &str) -> GateOutcome {
    if let Some(code) = exit_code(text) {
        return if code == 0 {
            GateOutcome::Pass
        } else {
            GateOutcome::Fail
        };
    }
    if text.contains("test result: FAILED") {
        return GateOutcome::Fail;
    }
    if text.contains("test result: ok") {
        return GateOutcome::Pass;
    }
    GateOutcome::Unknown
}

fn exit_code(text: &str) -> Option<i64> {
    for marker in ["\"exit_code\":", "Exit code: "] {
        if let Some(idx) = text.find(marker) {
            let digits: String = text[idx + marker.len()..]
                .trim_start()
                .chars()
                .take_while(|ch| ch.is_ascii_digit() || *ch == '-')
                .collect();
            if let Ok(code) = digits.parse() {
                return Some(code);
            }
        }
    }
    None
}

/// Surface explicit decision statements from an assistant reply. Candidate,
/// not verdict: only lines that announce themselves as decisions are taken
/// (TB vocabulary: `explicit_choice`, `plan_commitment`).
fn collect_decisions(candidates: &mut Vec<DecisionCandidate>, segment: &Segment, turn: &Turn) {
    for line in turn.text.lines() {
        let line = line.trim().trim_start_matches(['-', '*', ' ']);
        let kind = if line.starts_with("Decyzja:") || line.starts_with("Decision:") {
            "explicit_choice"
        } else if line.starts_with("Plan:") {
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

/// The trailing question of a reply, when the reply ends by asking one.
fn trailing_question(text: &str) -> Option<String> {
    let last = text.lines().rev().find(|line| !line.trim().is_empty())?;
    let last = last.trim();
    last.ends_with('?').then(|| last.to_owned())
}

/// Segment outcome from the tail of the segment: an abort keeps `Unknown`
/// (the tail is missing, not judged); a segment whose final speech turn is a
/// non-empty assistant reply completed its handoff.
fn segment_outcome(turns: &[&Turn], aborted: bool) -> AgentOutcome {
    if aborted {
        return AgentOutcome::Unknown;
    }
    let last_speech = turns.iter().rev().find(|turn| {
        matches!(
            turn.kind,
            TurnKind::AgentReply | TurnKind::UserMsg | TurnKind::ToolCall | TurnKind::ToolResult
        )
    });
    match last_speech {
        Some(turn) if turn.kind == TurnKind::AgentReply && !turn.text.trim().is_empty() => {
            AgentOutcome::Complete
        }
        _ => AgentOutcome::Unknown,
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

#[cfg(test)]
mod tests {
    use super::super::SEGMENT_DISTILLATE_SCHEMA;
    use super::*;
    use aicx_parser::engine::{
        BoundaryFlags, CoverageReport, FrameClass, ParseStatus, Provenance, RawUnitRef, Retained,
        ScopeStatus, ShellExecutor, ToolEvent, TurnRange, TurnRole, VisibleCompleteness,
        sha256_hex,
    };

    fn model_with(turns: Vec<Turn>, tool_events: Vec<ToolEvent>, segments: u32) -> SessionModel {
        let provenance = Provenance {
            agent: AgentKind::Codex,
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
        let mut model = SessionModel::new("codex-lane-test", provenance, coverage);
        model.segments = (0..segments)
            .map(|segment_id| Segment {
                segment_id,
                cwd: Known::unknown(),
                branch: Known::unknown(),
                started_at: Known::unknown(),
                ended_at: Known::unknown(),
                turn_range: TurnRange { start: 0, end: 0 },
                scope_status: ScopeStatus::Unknown,
                scope_conflict: false,
            })
            .collect();
        model.turns = turns;
        model.tool_events = tool_events;
        model
    }

    fn turn(
        turn_idx: u64,
        segment_id: u32,
        kind: TurnKind,
        text: &str,
        tool_name: Known<String>,
        frame_class: Option<FrameClass>,
    ) -> Turn {
        Turn {
            turn_idx,
            role: TurnRole::Tool,
            timestamp: Known::unknown(),
            kind,
            text: text.to_owned(),
            text_hash: sha256_hex(text.as_bytes()),
            text_chars: text.chars().count() as u64,
            tool_name,
            segment_id,
            raw_unit_refs: Vec::new(),
            frame_class,
        }
    }

    fn tool_event(kind: ToolEventKind, turn_idx: u64, correlation: &str) -> ToolEvent {
        ToolEvent {
            kind,
            turn_idx,
            tool_name: "exec".to_owned(),
            correlation_id: Known::value(correlation.to_owned()),
            payload_hash: "sha256:test".to_owned(),
            payload_bytes: 0,
            raw_unit_refs: vec![RawUnitRef {
                evidence_event_id: "test".to_owned(),
                coverage_ordinal: 0,
                physical_ordinal: 0,
                locator: "L0".to_owned(),
                unit_kind: "test".to_owned(),
                artifact: "test".to_owned(),
                content_hash: "sha256:test".to_owned(),
                original_bytes: 0,
            }],
        }
    }

    #[test]
    fn human_shell_command_is_never_a_gate_observation() {
        let human_shell = FrameClass::ShellAction {
            cmd: "cargo test --workspace".to_owned(),
            result: Retained {
                text: "Exit code: 0".to_owned(),
                chars: 12,
                hash: sha256_hex(b"Exit code: 0"),
            },
            executor: ShellExecutor::Human,
        };
        let turns = vec![
            // The operator ran a gate command by hand — evidence of a human
            // run, not an agent gate.
            turn(
                0,
                0,
                TurnKind::ToolCall,
                "$ cargo test --workspace",
                Known::value("user_shell_command".to_owned()),
                Some(human_shell),
            ),
            // The agent ran the same gate through exec.
            turn(
                1,
                0,
                TurnKind::ToolCall,
                r#"await tools.exec_command({"cmd":"cargo test --workspace"})"#,
                Known::value("exec".to_owned()),
                None,
            ),
            turn(
                2,
                0,
                TurnKind::ToolResult,
                "test result: ok. 3 passed",
                Known::value("exec".to_owned()),
                None,
            ),
        ];
        let events = vec![
            tool_event(ToolEventKind::Call, 1, "call_1"),
            tool_event(ToolEventKind::Result, 2, "call_1"),
        ];
        let model = model_with(turns, events, 1);
        let distillate = CodexLane.distill_segment(&model, &model.segments[0]);
        assert_eq!(distillate.gates.len(), 1, "only the agent run is a gate");
        assert_eq!(distillate.gates[0].command, "cargo test --workspace");
        assert_eq!(distillate.gates[0].outcome, GateOutcome::Pass);
        assert_eq!(distillate.gates[0].evidence.turn_idx, Some(1));
    }

    #[test]
    fn gate_marker_quoted_in_search_pattern_is_not_a_gate() {
        let turns = vec![turn(
            0,
            0,
            TurnKind::ToolCall,
            r#"await tools.exec_command({"cmd":"rg -n 'pytest|cargo test' docs"})"#,
            Known::value("exec".to_owned()),
            None,
        )];
        let model = model_with(turns, Vec::new(), 1);
        let distillate = CodexLane.distill_segment(&model, &model.segments[0]);
        assert!(distillate.gates.is_empty(), "{:?}", distillate.gates);
    }

    #[test]
    fn gate_without_correlated_result_reads_unknown_not_pass() {
        let turns = vec![turn(
            0,
            0,
            TurnKind::ToolCall,
            r#"await tools.exec_command({"cmd":"FOO=1 cargo clippy -- -D warnings"})"#,
            Known::value("exec".to_owned()),
            None,
        )];
        let events = vec![tool_event(ToolEventKind::Call, 0, "call_1")];
        let model = model_with(turns, events, 1);
        let distillate = CodexLane.distill_segment(&model, &model.segments[0]);
        assert_eq!(distillate.gates.len(), 1);
        assert_eq!(distillate.gates[0].outcome, GateOutcome::Unknown);
    }

    #[test]
    fn failing_exit_code_reads_fail() {
        let turns = vec![
            turn(
                0,
                0,
                TurnKind::ToolCall,
                r#"await tools.exec_command({"cmd":"cargo test -p aicx"})"#,
                Known::value("exec".to_owned()),
                None,
            ),
            turn(
                1,
                0,
                TurnKind::ToolResult,
                r#"{"exit_code":101,"output":"test result: FAILED"}"#,
                Known::value("exec".to_owned()),
                None,
            ),
        ];
        let events = vec![
            tool_event(ToolEventKind::Call, 0, "call_1"),
            tool_event(ToolEventKind::Result, 1, "call_1"),
        ];
        let model = model_with(turns, events, 1);
        let distillate = CodexLane.distill_segment(&model, &model.segments[0]);
        assert_eq!(distillate.gates[0].outcome, GateOutcome::Fail);
    }

    #[test]
    fn reasoning_is_never_a_handoff_signal() {
        let turns = vec![
            turn(
                0,
                0,
                TurnKind::AgentReply,
                "Zamykam: kontrakt spełniony.",
                Known::unknown(),
                Some(FrameClass::AssistantFinal),
            ),
            // Reasoning arrives after the reply in the stream; it must not
            // displace the reply as the handoff signal.
            turn(
                1,
                0,
                TurnKind::InternalThought,
                "internal chain of thought",
                Known::unknown(),
                None,
            ),
        ];
        let model = model_with(turns, Vec::new(), 1);
        let distillate = CodexLane.distill_segment(&model, &model.segments[0]);
        assert_eq!(distillate.handoff_signals.len(), 1);
        assert_eq!(
            distillate.handoff_signals[0].text,
            "Zamykam: kontrakt spełniony."
        );
        assert_eq!(distillate.outcome.agent_outcome, AgentOutcome::Complete);
    }

    #[test]
    fn mixed_session_distills_per_segment_never_joined() {
        let turns = vec![
            turn(
                0,
                0,
                TurnKind::AgentReply,
                "Segment zero done.",
                Known::unknown(),
                Some(FrameClass::AssistantFinal),
            ),
            turn(
                1,
                1,
                TurnKind::ToolCall,
                r#"await tools.exec_command({"cmd":"cargo check"})"#,
                Known::value("exec".to_owned()),
                None,
            ),
            turn(
                2,
                1,
                TurnKind::SystemNote,
                "turn aborted: user interrupt",
                Known::unknown(),
                None,
            ),
        ];
        let model = model_with(turns, Vec::new(), 2);
        let distillates = CodexLane.distill(&model);
        assert_eq!(distillates.len(), 2, "one distillate per segment");
        // Segment 0: a completed handoff, no gates, no abort.
        assert_eq!(distillates[0].segment_id, 0);
        assert_eq!(distillates[0].handoff_signals.len(), 1);
        assert!(distillates[0].gates.is_empty());
        assert_eq!(distillates[0].outcome.ending, None);
        assert_eq!(distillates[0].outcome.agent_outcome, AgentOutcome::Complete);
        // Segment 1: an aborted gate run — nothing leaks from segment 0.
        assert_eq!(distillates[1].segment_id, 1);
        assert!(distillates[1].handoff_signals.is_empty());
        assert_eq!(distillates[1].gates.len(), 1);
        assert_eq!(
            distillates[1].outcome.ending.as_deref(),
            Some("interrupted")
        );
        assert_eq!(distillates[1].outcome.agent_outcome, AgentOutcome::Unknown);
        for distillate in &distillates {
            assert_eq!(distillate.schema, SEGMENT_DISTILLATE_SCHEMA);
            assert_eq!(distillate.agent, AgentKind::Codex);
        }
    }

    #[test]
    fn decision_and_trailing_question_are_surfaced_with_evidence() {
        let text = "Decyzja: zostaje registry fail-open.\nPlan: golden test na fixture.\n\nCzy dodać też lane gemini?";
        let turns = vec![turn(
            0,
            0,
            TurnKind::AgentReply,
            text,
            Known::unknown(),
            Some(FrameClass::AssistantFinal),
        )];
        let model = model_with(turns, Vec::new(), 1);
        let distillate = CodexLane.distill_segment(&model, &model.segments[0]);
        let kinds: Vec<&str> = distillate
            .decision_candidates
            .iter()
            .map(|candidate| candidate.kind.as_str())
            .collect();
        assert_eq!(kinds, ["explicit_choice", "plan_commitment"]);
        assert_eq!(distillate.open_questions.len(), 1);
        assert_eq!(distillate.open_questions[0].kind, "agent_question");
        assert_eq!(
            distillate.open_questions[0].text,
            "Czy dodać też lane gemini?"
        );
        assert_eq!(distillate.open_questions[0].evidence.segment_id, 0);
        assert_eq!(distillate.open_questions[0].evidence.turn_idx, Some(0));
    }

    /// Golden: the full distillate of the real (redacted) codex rollout
    /// fixture. Regenerate with `AICX_BLESS=1 cargo test -p aicx
    /// distill::codex_lane::tests::codex_lane_matches_golden`.
    #[test]
    fn codex_lane_matches_golden() {
        use aicx_parser::engine::{
            ParserEngine, SourceArtifact, SourceFraming, SourceHandle, ValidatedParse,
        };
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let fixture = root.join("tests/fixtures/tb_oracle/codex/rollout-2026-08-04-019fc9dd.jsonl");
        let golden_path = root.join("tests/fixtures/tb_oracle/codex/019fc9dd_golden.json");
        // Memory artifact: `validated_file` enforces the runtime allowed-roots
        // policy and rejects checkouts outside the session-store roots
        // (e.g. /Volumes/...), which would tie this test to the clone path.
        let body = std::fs::read(&fixture)
            .unwrap_or_else(|error| panic!("cannot read fixture {}: {error}", fixture.display()));
        let artifact = SourceArtifact::memory(
            "rollout-2026-08-04-019fc9dd.jsonl",
            body,
            SourceFraming::JsonLines,
        )
        .expect("fixture readable");
        let handle = SourceHandle::new(
            AgentKind::Codex,
            "019fc9dd-codex-fixture",
            None,
            vec![artifact],
        )
        .expect("valid handle");
        let parse = ParserEngine::default()
            .parse_registered(&handle)
            .expect("fixture parses");
        let ValidatedParse::Session(session) = parse else {
            panic!("fixture must parse to a session, got fatal parse");
        };
        let model = session.into_model();
        let distillates = CodexLane.distill(&model);
        assert!(!distillates.is_empty());
        let rendered =
            serde_json::to_string_pretty(&distillates).expect("distillate serializes") + "\n";
        if std::env::var_os("AICX_BLESS").is_some() {
            std::fs::write(&golden_path, &rendered).expect("golden written");
            return;
        }
        let golden = std::fs::read_to_string(&golden_path)
            .expect("golden fixture exists (AICX_BLESS=1 to regenerate)");
        assert_eq!(
            rendered, golden,
            "codex lane distillate drifted from golden"
        );
    }
}
