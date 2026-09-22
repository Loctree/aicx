//! Grok lane distiller — fan-in of assistant text, JSON-string `tool_calls`,
//! and opaque reasoning into one [`SegmentDistillate`] per segment.
//!
//! Gates are read from tool_calls / tool results, never from reasoning.
//! `encrypted_content` is opaque and never copied into the distillate.
//! When `summary.json` was consumed, it is the LaneOutcome *source* and is
//! marked via a `kind = "summary_json"` decision candidate (W0
//! [`LaneOutcome`] has no provenance field — see the W1-04 report).

use super::{
    AgentLaneDistiller, AgentOutcome, DecisionCandidate, EvidenceLocator, GateObservation,
    GateOutcome, HandoffSignal, LaneOutcome, OpenQuestion, SegmentDistillate,
};
use aicx_parser::engine::{AgentKind, Segment, SessionModel, Turn, TurnKind, TurnRole};

/// Sentinel used in the redacted fixture; also treated as opaque in production
/// if a substrate leak ever carries the same token.
const OPAQUE_SENTINEL: &str = "OPAQUE_ENCRYPTED_SENTINEL_DO_NOT_DISTILL";

/// Grok per-agent distiller.
#[derive(Debug, Clone, Copy, Default)]
pub struct GrokLane;

impl AgentLaneDistiller for GrokLane {
    fn agent(&self) -> AgentKind {
        AgentKind::Grok
    }

    fn lane_name(&self) -> &'static str {
        "grok"
    }

    fn distill_segment(&self, model: &SessionModel, segment: &Segment) -> SegmentDistillate {
        let turns: Vec<&Turn> = model
            .turns
            .iter()
            .filter(|turn| in_segment(turn, segment))
            .collect();

        let mut distillate = SegmentDistillate::empty(AgentKind::Grok, segment);
        distillate.gates = gates_from_tool_calls(&turns);
        distillate.decision_candidates = decisions_from_assistant(&turns);
        distillate.open_questions = open_questions_from_users(&turns);
        distillate.handoff_signals = handoff_from_last_assistant(&turns);

        let (outcome, provenance) = outcome_with_provenance(model, segment, &turns, &distillate);
        distillate.outcome = outcome;
        if let Some(candidate) = provenance {
            distillate.decision_candidates.push(candidate);
        }
        strip_opaque_from_distillate(&mut distillate);
        distillate
    }
}

fn in_segment(turn: &Turn, segment: &Segment) -> bool {
    turn.turn_idx >= segment.turn_range.start && turn.turn_idx <= segment.turn_range.end
}

fn is_opaque_turn(turn: &Turn) -> bool {
    if turn.kind == TurnKind::InternalThought {
        return true;
    }
    if turn
        .raw_unit_refs
        .iter()
        .any(|r| r.unit_kind == "reasoning" || r.unit_kind == "encrypted_reasoning")
    {
        return true;
    }
    !text_is_safe(&turn.text)
}

fn text_is_safe(text: &str) -> bool {
    if text.contains("encrypted_content") || text.contains(OPAQUE_SENTINEL) {
        return false;
    }
    !looks_like_ciphertext(text)
}

fn looks_like_ciphertext(text: &str) -> bool {
    let trimmed = text.trim();
    if trimmed.len() < 80 || trimmed.contains(' ') || trimmed.contains('\n') {
        return false;
    }
    let alphabet = trimmed
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '+' || *c == '/' || *c == '=' || *c == '-')
        .count();
    alphabet * 100 / trimmed.len() >= 95
}

fn is_summary_source(turn: &Turn) -> bool {
    turn.raw_unit_refs.iter().any(|r| {
        r.unit_kind == "summary"
            || r.artifact == "summary.json"
            || r.artifact.ends_with("/summary.json")
            || r.artifact.ends_with("summary.json")
    })
}

fn coverage_consumed_summary(model: &SessionModel) -> bool {
    model
        .coverage
        .consumed
        .iter()
        .any(|unit| unit.kind == "summary")
        || model
            .coverage
            .consumed_by_kind
            .get("summary")
            .is_some_and(|n| *n > 0)
}

fn locator_for(turn: &Turn) -> EvidenceLocator {
    EvidenceLocator {
        segment_id: turn.segment_id,
        turn_idx: Some(turn.turn_idx),
        timestamp: match &turn.timestamp {
            aicx_parser::engine::Known::Value(ts) => Some(ts.clone()),
            aicx_parser::engine::Known::Unknown(_) => None,
        },
    }
}

fn locator_for_segment(segment: &Segment) -> EvidenceLocator {
    EvidenceLocator {
        segment_id: segment.segment_id,
        turn_idx: None,
        timestamp: match &segment.started_at {
            aicx_parser::engine::Known::Value(ts) => Some(ts.clone()),
            aicx_parser::engine::Known::Unknown(_) => None,
        },
    }
}

fn gates_from_tool_calls(turns: &[&Turn]) -> Vec<GateObservation> {
    let mut pending: Vec<(String, EvidenceLocator)> = Vec::new();
    let mut gates = Vec::new();
    for turn in turns {
        if is_opaque_turn(turn) {
            continue;
        }
        let commands = gate_commands_from_turn(turn);
        if !commands.is_empty() && matches!(turn.kind, TurnKind::ToolCall | TurnKind::AgentReply) {
            for command in commands {
                pending.push((command, locator_for(turn)));
            }
            continue;
        }
        if matches!(turn.kind, TurnKind::ToolResult | TurnKind::ToolCall)
            && !pending.is_empty()
            && looks_like_gate_result(&turn.text)
        {
            let (command, evidence) = pending.remove(0);
            gates.push(GateObservation {
                command,
                outcome: gate_outcome_from_result(&turn.text),
                evidence,
            });
        }
    }
    for (command, evidence) in pending {
        gates.push(GateObservation {
            command,
            outcome: GateOutcome::Unknown,
            evidence,
        });
    }
    gates
}

fn gate_commands_from_turn(turn: &Turn) -> Vec<String> {
    let tool_name = match &turn.tool_name {
        aicx_parser::engine::Known::Value(name) => Some(name.as_str()),
        aicx_parser::engine::Known::Unknown(_) => None,
    };
    let mut commands = commands_from_jsonish(&turn.text);
    if commands.is_empty() {
        if let Some(name) = tool_name
            && is_shell_tool(name)
            && is_gate_command(&turn.text)
        {
            commands.push(first_command_line(&turn.text));
        } else if is_gate_command(&turn.text) && turn.kind == TurnKind::ToolCall {
            commands.push(first_command_line(&turn.text));
        }
    }
    commands.retain(|cmd| is_gate_command(cmd));
    commands
}

fn is_shell_tool(name: &str) -> bool {
    matches!(
        name,
        "run_terminal_command" | "shell" | "Bash" | "bash" | "terminal"
    )
}

fn commands_from_jsonish(text: &str) -> Vec<String> {
    let parsed: serde_json::Value = match serde_json::from_str(text.trim()) {
        Ok(value) => value,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    match parsed {
        serde_json::Value::Array(items) => {
            for item in items {
                if let Some(cmd) = command_from_tool_object(&item) {
                    out.push(cmd);
                }
            }
        }
        other => {
            if let Some(cmd) = command_from_tool_object(&other) {
                out.push(cmd);
            }
        }
    }
    out
}

fn command_from_tool_object(value: &serde_json::Value) -> Option<String> {
    if let Some(cmd) = value.get("command").and_then(serde_json::Value::as_str) {
        return Some(cmd.to_owned());
    }
    let args = value.get("arguments").or_else(|| value.get("args"));
    let args_obj = match args {
        Some(serde_json::Value::String(raw)) => serde_json::from_str(raw).ok(),
        Some(obj) if obj.is_object() => Some(obj.clone()),
        _ => None,
    };
    args_obj
        .as_ref()
        .and_then(|obj| obj.get("command"))
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
}

fn is_gate_command(cmd: &str) -> bool {
    let lower = cmd.to_ascii_lowercase();
    lower.contains("cargo test")
        || lower.contains("cargo clippy")
        || lower.contains("cargo fmt")
        || lower.contains("cargo check")
        || lower.contains("pytest")
        || lower.contains("npm test")
        || lower.contains("make test")
        || lower.contains("make check")
}

fn first_command_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(text)
        .to_owned()
}

fn looks_like_gate_result(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    lower.contains("test result")
        || lower.contains("finished")
        || lower.contains("exit")
        || lower.contains("error")
        || lower.contains("failed")
        || lower.contains("ok")
        || lower.contains("fmt=")
        || lower.contains("passed")
        || lower.contains("clippy")
}

fn gate_outcome_from_result(text: &str) -> GateOutcome {
    let lower = text.to_ascii_lowercase();
    let failed = lower.contains("test result: failed")
        || lower.contains("error[")
        || lower.contains("exit_code: 1")
        || lower.contains("exit 1")
        || lower.contains("fmt=1")
        || (lower.contains("failed") && lower.contains("aborting"));
    if failed {
        return GateOutcome::Fail;
    }
    let passed = lower.contains("test result: ok")
        || lower.contains("0 failed")
        || lower.contains("exit_code: 0")
        || lower.contains("exit 0")
        || lower.contains("fmt=0")
        || lower.contains("finished `test`")
        || (lower.contains("ok") && lower.contains("passed"));
    if passed {
        GateOutcome::Pass
    } else {
        GateOutcome::Unknown
    }
}

fn decisions_from_assistant(turns: &[&Turn]) -> Vec<DecisionCandidate> {
    let mut out = Vec::new();
    for turn in turns {
        if is_opaque_turn(turn) {
            continue;
        }
        if turn.kind != TurnKind::AgentReply || turn.role != TurnRole::Assistant {
            continue;
        }
        if is_summary_source(turn) {
            continue;
        }
        for line in turn.text.lines() {
            let line = line.trim();
            if !is_decision_line(line) || !text_is_safe(line) {
                continue;
            }
            out.push(DecisionCandidate {
                text: line.to_owned(),
                kind: decision_kind(line).to_owned(),
                evidence: locator_for(turn),
            });
        }
    }
    out
}

fn is_decision_line(line: &str) -> bool {
    if line.len() < 12 {
        return false;
    }
    let lower = line.to_ascii_lowercase();
    lower.starts_with("i will ")
        || lower.starts_with("i'll ")
        || lower.starts_with("using ")
        || lower.starts_with("decided ")
        || lower.contains(" mapping is gone")
        || lower.contains("now goes through")
        || lower.contains("i am implementing")
}

fn decision_kind(line: &str) -> &'static str {
    let lower = line.to_ascii_lowercase();
    if lower.contains(" mapping is gone") || lower.starts_with("using ") {
        "explicit_choice"
    } else {
        "plan_commitment"
    }
}

fn open_questions_from_users(turns: &[&Turn]) -> Vec<OpenQuestion> {
    let mut out = Vec::new();
    for (idx, turn) in turns.iter().enumerate() {
        if is_opaque_turn(turn) || turn.kind != TurnKind::UserMsg {
            continue;
        }
        let question = question_text(&turn.text);
        if question.is_none() {
            continue;
        }
        let answered = turns[idx + 1..].iter().any(|later| {
            !is_opaque_turn(later)
                && later.kind == TurnKind::AgentReply
                && later.role == TurnRole::Assistant
        });
        if answered {
            continue;
        }
        let text = question.unwrap();
        if !text_is_safe(&text) {
            continue;
        }
        out.push(OpenQuestion {
            kind: "user_report".to_owned(),
            text,
            evidence: locator_for(turn),
        });
    }
    out
}

fn question_text(text: &str) -> Option<String> {
    let stripped = text
        .replace("<user_query>", "")
        .replace("</user_query>", "");
    let candidate = stripped
        .lines()
        .map(str::trim)
        .find(|line| line.contains('?') && !line.starts_with('<') && line.len() > 8)?;
    Some(candidate.to_owned())
}

fn handoff_from_last_assistant(turns: &[&Turn]) -> Vec<HandoffSignal> {
    let Some(last) = turns.iter().rev().find(|turn| {
        !is_opaque_turn(turn)
            && turn.kind == TurnKind::AgentReply
            && turn.role == TurnRole::Assistant
            && text_is_safe(&turn.text)
            && !turn.text.trim().is_empty()
    }) else {
        return Vec::new();
    };
    let mut text = last.text.trim().to_owned();
    if text.chars().count() > 480 {
        text = text.chars().take(480).collect();
    }
    vec![HandoffSignal {
        text,
        evidence: locator_for(last),
    }]
}

fn outcome_with_provenance(
    model: &SessionModel,
    segment: &Segment,
    turns: &[&Turn],
    distillate: &SegmentDistillate,
) -> (LaneOutcome, Option<DecisionCandidate>) {
    let summary_turn = turns.iter().copied().find(|turn| is_summary_source(turn));
    let summary_present = summary_turn.is_some() || coverage_consumed_summary(model);

    let from_summary = summary_turn.and_then(|turn| parse_agent_outcome(&turn.text));
    let from_handoff = distillate
        .handoff_signals
        .first()
        .and_then(|signal| parse_agent_outcome(&signal.text));
    let from_gates = infer_outcome_from_gates(&distillate.gates);

    let agent_outcome = from_summary
        .or(from_handoff)
        .or(from_gates)
        .unwrap_or(AgentOutcome::Unknown);

    let ending = if last_turn_looks_interrupted(turns) {
        Some("interrupted".to_owned())
    } else {
        None
    };

    let provenance = if summary_present {
        let evidence = summary_turn
            .map(locator_for)
            .unwrap_or_else(|| locator_for_segment(segment));
        Some(DecisionCandidate {
            text: "LaneOutcome sourced from summary.json".to_owned(),
            kind: "summary_json".to_owned(),
            evidence,
        })
    } else {
        None
    };

    (
        LaneOutcome {
            agent_outcome,
            ending,
        },
        provenance,
    )
}

fn parse_agent_outcome(text: &str) -> Option<AgentOutcome> {
    let lower = text.to_ascii_lowercase();
    if lower.contains("agent_outcome: failed")
        || lower.contains("agent outcome: failed")
        || lower.contains("gate=red")
    {
        return Some(AgentOutcome::Failed);
    }
    if lower.contains("agent_outcome: partial")
        || lower.contains("agent outcome: partial")
        || lower.contains("not_assessed")
        || lower.contains("not run")
    {
        return Some(AgentOutcome::Partial);
    }
    if lower.contains("agent_outcome: complete")
        || lower.contains("agent outcome: complete")
        || (lower.contains("gate=green") && !lower.contains("not_assessed"))
    {
        return Some(AgentOutcome::Complete);
    }
    None
}

fn infer_outcome_from_gates(gates: &[GateObservation]) -> Option<AgentOutcome> {
    if gates.is_empty() {
        return None;
    }
    if gates.iter().any(|g| g.outcome == GateOutcome::Fail) {
        return Some(AgentOutcome::Failed);
    }
    if gates.iter().all(|g| g.outcome == GateOutcome::Pass) {
        return Some(AgentOutcome::Complete);
    }
    Some(AgentOutcome::Partial)
}

fn last_turn_looks_interrupted(turns: &[&Turn]) -> bool {
    match turns.last() {
        Some(turn) => {
            matches!(turn.kind, TurnKind::ToolCall | TurnKind::ToolResult) && !is_opaque_turn(turn)
        }
        None => false,
    }
}

fn strip_opaque_from_distillate(distillate: &mut SegmentDistillate) {
    distillate
        .decision_candidates
        .retain(|c| text_is_safe(&c.text) && text_is_safe(&c.kind));
    distillate.gates.retain(|g| text_is_safe(&g.command));
    distillate
        .open_questions
        .retain(|q| text_is_safe(&q.text) && text_is_safe(&q.kind));
    distillate.handoff_signals.retain(|h| text_is_safe(&h.text));
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicx_parser::engine::{
        BoundaryFlags, CoverageReport, Known, ParseStatus, Provenance, RawUnitRef, ScopeStatus,
        TurnRange, VisibleCompleteness,
    };

    const SENTINEL: &str = OPAQUE_SENTINEL;
    const CWD: &str =
        "/Users/polyversai/.vibecrafted/worktrees/Loctree/aicx/2026_0827/W2-T9-grok-on-throne";
    const BRANCH: &str = "cut/W2-T9-grok-on-throne";
    const TS: &str = "2026-08-27T18:51:24Z";
    const SESSION: &str = "01a04490-036f-7c50-bb60-3f51bb3afcf7";

    #[test]
    fn grok_lane_matches_golden() {
        let model = fixture_model();
        let lane = GrokLane;
        let distillates = lane.distill(&model);
        assert_eq!(
            distillates.len(),
            1,
            "single-segment fixture yields one distillate"
        );
        let got = serde_json::to_value(&distillates[0]).expect("serialize distillate");
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/tb_oracle/grok/golden.json");
        let expected: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display())),
        )
        .expect("golden.json parses");
        assert_eq!(got, expected, "distillate drifted from golden.json");
    }

    #[test]
    fn grok_opaque_reasoning_absent_from_distillate() {
        let model = fixture_model();
        let blob = serde_json::to_string(&GrokLane.distill(&model)).expect("json");
        assert!(
            !blob.contains(SENTINEL),
            "encrypted_content sentinel leaked into distillate: {blob}"
        );
        assert!(
            !blob.contains("encrypted_content"),
            "encrypted_content key leaked into distillate"
        );
        assert!(
            !blob.contains("The user wants me to implement a specific cut"),
            "reasoning summary leaked into distillate"
        );
    }

    #[test]
    fn grok_mixed_session_yields_n_distillates() {
        let model = mixed_model();
        let distillates = GrokLane.distill(&model);
        assert_eq!(distillates.len(), 2);
        assert_eq!(distillates[0].segment_id, 0);
        assert_eq!(distillates[1].segment_id, 1);
        assert_eq!(distillates[0].scope_status, ScopeStatus::NoDriftObserved);
        assert_eq!(distillates[1].scope_status, ScopeStatus::MixedCandidate);
        assert_ne!(
            distillates[0].handoff_signals, distillates[1].handoff_signals,
            "per-segment distillates must not average workstreams"
        );
    }

    #[test]
    fn grok_summary_json_marks_outcome_provenance() {
        let model = fixture_model();
        let distillate = GrokLane.distill_segment(&model, &model.segments[0]);
        let provenance = distillate
            .decision_candidates
            .iter()
            .find(|c| c.kind == "summary_json")
            .expect("summary.json provenance candidate");
        assert_eq!(provenance.text, "LaneOutcome sourced from summary.json");
        assert_eq!(distillate.outcome.agent_outcome, AgentOutcome::Partial);
    }

    #[test]
    fn grok_coverage_summary_without_summary_turn_still_marks_provenance() {
        // Live GrokAdapter consumes summary.json but drops session_summary
        // (`_title`). Provenance must still fire from coverage.consumed.
        let mut model = mixed_model();
        model
            .coverage
            .consumed
            .push(consumed("summary", "summary.json", 1));
        model.coverage.consumed_count = 1;
        model
            .coverage
            .consumed_by_kind
            .insert("summary".to_owned(), 1);
        let distillate = GrokLane.distill_segment(&model, &model.segments[0]);
        let provenance = distillate
            .decision_candidates
            .iter()
            .find(|c| c.kind == "summary_json")
            .expect("coverage-only summary.json provenance");
        assert_eq!(provenance.evidence.turn_idx, None);
        assert_eq!(provenance.evidence.segment_id, 0);
    }

    #[test]
    fn grok_gates_come_from_tool_calls_not_reasoning() {
        let model = fixture_model();
        let distillate = GrokLane.distill_segment(&model, &model.segments[0]);
        assert_eq!(distillate.gates.len(), 1);
        assert!(
            distillate.gates[0].command.contains("cargo test"),
            "gate command: {}",
            distillate.gates[0].command
        );
        assert_eq!(distillate.gates[0].outcome, GateOutcome::Pass);
        assert_eq!(distillate.gates[0].evidence.turn_idx, Some(3));
    }

    #[test]
    fn grok_lane_is_registered() {
        let registry = super::super::LaneRegistry::with_default_lanes();
        let lane = registry.lane_for(AgentKind::Grok);
        assert_eq!(lane.lane_name(), "grok");
        assert_eq!(lane.agent(), AgentKind::Grok);
    }

    fn fixture_model() -> SessionModel {
        let mut model = base_model(CWD, BRANCH);
        model
            .coverage
            .consumed
            .push(consumed("summary", "summary.json", 1));
        model.coverage.consumed_count = 1;
        model
            .coverage
            .consumed_by_kind
            .insert("summary".to_owned(), 1);
        model.coverage.raw_unit_count = 1;
        model
            .coverage
            .status
            .boundary_flags
            .opaque_reasoning_present = true;

        model.turns = vec![
            turn(
                0,
                TurnRole::System,
                TurnKind::SystemNote,
                "W2-T9 grok-on-throne taxonomy fusion implement. agent_outcome: partial.",
                "summary",
                "summary.json",
                None,
            ),
            turn(
                1,
                TurnRole::User,
                TurnKind::UserMsg,
                "<user_query>Implement W2-T9 grok-on-throne: speech class through the taxonomy throne.</user_query>",
                "user",
                "chat_history.jsonl",
                None,
            ),
            turn(
                2,
                TurnRole::Tool,
                TurnKind::InternalThought,
                SENTINEL,
                "reasoning",
                "chat_history.jsonl",
                None,
            ),
            turn(
                3,
                TurnRole::Assistant,
                TurnKind::ToolCall,
                r#"[{"name":"run_terminal_command","arguments":"{\"command\":\"cargo test -p aicx-parser --test grok_adapter\"}"}]"#,
                "assistant",
                "chat_history.jsonl",
                Some("run_terminal_command"),
            ),
            turn(
                4,
                TurnRole::Tool,
                TurnKind::ToolResult,
                "running 12 tests\ntest result: ok. 12 passed; 0 failed; 0 ignored\nFinished `test` profile",
                "tool_result",
                "chat_history.jsonl",
                Some("run_terminal_command"),
            ),
            turn(
                5,
                TurnRole::Assistant,
                TurnKind::AgentReply,
                "Grok speech class now goes through the taxonomy throne. Local `type=user` → `UserMsg` mapping is gone.\n\n**Commit:** `252caef` on `cut/W2-T9-grok-on-throne` — not pushed.\nGATE=green PHASE=W2 BUILD/LINT/TEST=NOT_ASSESSED (embargo W2)",
                "assistant",
                "chat_history.jsonl",
                None,
            ),
            turn(
                6,
                TurnRole::User,
                TurnKind::UserMsg,
                "Do we still need a W2-T12 extract-level refusal for degenerate 019fdeca?",
                "user",
                "chat_history.jsonl",
                None,
            ),
        ];
        model.segments = vec![Segment {
            segment_id: 0,
            cwd: Known::value(CWD.to_owned()),
            branch: Known::value(BRANCH.to_owned()),
            started_at: Known::value(TS.to_owned()),
            ended_at: Known::value("2026-08-27T19:06:15Z".to_owned()),
            turn_range: TurnRange { start: 0, end: 6 },
            scope_status: ScopeStatus::NoDriftObserved,
            scope_conflict: false,
            scope_root: None,
        }];
        model
    }

    fn mixed_model() -> SessionModel {
        let mut model = base_model(CWD, BRANCH);
        model.turns = vec![
            turn(
                0,
                TurnRole::User,
                TurnKind::UserMsg,
                "Work the aicx parser throne.",
                "user",
                "chat_history.jsonl",
                None,
            ),
            turn(
                1,
                TurnRole::Assistant,
                TurnKind::AgentReply,
                "I will port grok through frames::classify.",
                "assistant",
                "chat_history.jsonl",
                None,
            ),
            turn(
                2,
                TurnRole::User,
                TurnKind::UserMsg,
                "Now switch to pensieve docs?",
                "user",
                "chat_history.jsonl",
                None,
            ),
            turn(
                3,
                TurnRole::Assistant,
                TurnKind::AgentReply,
                "Using a second workstream for pensieve; not averaging outcomes.",
                "assistant",
                "chat_history.jsonl",
                None,
            ),
        ];
        model.turns[2].segment_id = 1;
        model.turns[3].segment_id = 1;
        model.segments = vec![
            Segment {
                segment_id: 0,
                cwd: Known::value(CWD.to_owned()),
                branch: Known::value(BRANCH.to_owned()),
                started_at: Known::value(TS.to_owned()),
                ended_at: Known::value(TS.to_owned()),
                turn_range: TurnRange { start: 0, end: 1 },
                scope_status: ScopeStatus::NoDriftObserved,
                scope_conflict: false,
                scope_root: None,
            },
            Segment {
                segment_id: 1,
                cwd: Known::value("/Volumes/vc-workspace/vetcoders/pensieve".to_owned()),
                branch: Known::value("feat/swift-6-transformation".to_owned()),
                started_at: Known::value(TS.to_owned()),
                ended_at: Known::value(TS.to_owned()),
                turn_range: TurnRange { start: 2, end: 3 },
                scope_status: ScopeStatus::MixedCandidate,
                scope_conflict: false,
                scope_root: None,
            },
        ];
        model
    }

    fn base_model(cwd: &str, branch: &str) -> SessionModel {
        let provenance = Provenance {
            agent: AgentKind::Grok,
            model: Known::value("grok-4.6".to_owned()),
            cli_version: Known::unknown(),
            cwd: Known::value(cwd.to_owned()),
            branch: Known::value(branch.to_owned()),
            started_at: Known::value(TS.to_owned()),
            ended_at: Known::value("2026-08-27T19:06:15Z".to_owned()),
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
        SessionModel::new(SESSION, provenance, coverage)
    }

    fn consumed(kind: &str, artifact: &str, ordinal: u64) -> aicx_parser::engine::ConsumedUnit {
        aicx_parser::engine::ConsumedUnit {
            ordinal,
            kind: kind.to_owned(),
            evidence: raw_ref(kind, artifact, ordinal),
        }
    }

    fn raw_ref(kind: &str, artifact: &str, ordinal: u64) -> RawUnitRef {
        RawUnitRef {
            evidence_event_id: format!("ev1:grok:{SESSION}:{ordinal}"),
            coverage_ordinal: ordinal,
            physical_ordinal: ordinal,
            locator: format!("u{ordinal}"),
            unit_kind: kind.to_owned(),
            artifact: artifact.to_owned(),
            content_hash: "sha256:fixture".to_owned(),
            original_bytes: 0,
        }
    }

    fn turn(
        idx: u64,
        role: TurnRole,
        kind: TurnKind,
        text: &str,
        unit_kind: &str,
        artifact: &str,
        tool_name: Option<&str>,
    ) -> Turn {
        Turn {
            turn_idx: idx,
            role,
            timestamp: Known::value(TS.to_owned()),
            kind,
            text: text.to_owned(),
            text_hash: "sha256:fixture-turn".to_owned(),
            text_chars: text.chars().count() as u64,
            tool_name: match tool_name {
                Some(name) => Known::value(name.to_owned()),
                None => Known::unknown(),
            },
            segment_id: 0,
            raw_unit_refs: vec![raw_ref(unit_kind, artifact, idx + 1)],
            frame_class: None,
        }
    }
}
