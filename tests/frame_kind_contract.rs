//! Frozen projection contract between the typed parser model and the stable
//! `FrameKind` retrieval vocabulary (C7 model-backed output layer).
//!
//! Conversation/report rendering and the store pipeline consume these
//! mappings; changing any string or arm here is a breaking contract change
//! and must be classified through the C0A normative matrix first.

use aicx::output::{
    conversation_messages_from_model, frame_kind_for_turn, role_str_for_turn,
    timeline_entries_from_model,
};
use aicx::parser::engine::{
    BoundaryFlags, CoverageReport, Known, ParseStatus, Provenance, Segment, SessionModel, Turn,
    TurnKind, TurnRange, TurnRole, VisibleCompleteness,
};
use aicx::timeline::FrameKind;

#[test]
fn frame_kind_strings_are_frozen() {
    let expected = [
        (FrameKind::UserMsg, "user_msg"),
        (FrameKind::AgentReply, "agent_reply"),
        (FrameKind::InternalThought, "internal_thought"),
        (FrameKind::ToolCall, "tool_call"),
        (FrameKind::SystemNote, "system_note"),
    ];
    for (kind, rendered) in expected {
        assert_eq!(kind.as_str(), rendered);
        assert_eq!(
            FrameKind::parse(rendered),
            Some(kind),
            "frame kind string `{rendered}` must round-trip"
        );
    }
}

#[test]
fn turn_kind_to_frame_kind_mapping_is_frozen() {
    let expected = [
        (TurnKind::UserMsg, FrameKind::UserMsg),
        (TurnKind::AgentReply, FrameKind::AgentReply),
        (TurnKind::InternalThought, FrameKind::InternalThought),
        (TurnKind::ToolCall, FrameKind::ToolCall),
        (TurnKind::ToolResult, FrameKind::ToolCall),
        (TurnKind::SystemNote, FrameKind::SystemNote),
    ];
    for (turn_kind, frame_kind) in expected {
        assert_eq!(
            frame_kind_for_turn(turn_kind),
            frame_kind,
            "TurnKind::{turn_kind:?} projection drifted"
        );
    }
}

#[test]
fn turn_role_strings_are_frozen() {
    assert_eq!(role_str_for_turn(TurnRole::User), "user");
    assert_eq!(role_str_for_turn(TurnRole::Assistant), "assistant");
    assert_eq!(role_str_for_turn(TurnRole::System), "system");
    assert_eq!(role_str_for_turn(TurnRole::Tool), "tool");
}

fn synthetic_model() -> SessionModel {
    let provenance = Provenance {
        agent: aicx::parser::engine::AgentKind::Codex,
        model: Known::unknown(),
        cli_version: Known::unknown(),
        cwd: Known::value("/work/space/aicx".to_owned()),
        branch: Known::value("fix/aicx-daily-usefulness".to_owned()),
        started_at: Known::value("2026-07-13T04:00:00Z".to_owned()),
        ended_at: Known::unknown(),
        original_source_hash: "a".repeat(64),
        original_source_bytes: 512,
    };
    let coverage = CoverageReport::with_raw_line_count(
        0,
        0,
        Vec::new(),
        Vec::new(),
        Vec::new(),
        ParseStatus {
            visible_completeness: VisibleCompleteness::CompleteVisible,
            boundary_flags: BoundaryFlags::default(),
            malformed_tail_present: false,
            visible_event_lost: false,
        },
    );
    let mut model = SessionModel::new("019f0000-1111-7111-8111-000000000001", provenance, coverage);
    model.segments.push(Segment {
        segment_id: 1,
        cwd: Known::value("/work/space/aicx".to_owned()),
        branch: Known::value("fix/aicx-daily-usefulness".to_owned()),
        started_at: Known::value("2026-07-13T04:00:00Z".to_owned()),
        ended_at: Known::unknown(),
        turn_range: TurnRange { start: 0, end: 3 },
    });
    let turn = |idx: u64, role: TurnRole, kind: TurnKind, text: &str, ts: Known<String>| Turn {
        turn_idx: idx,
        role,
        timestamp: ts,
        kind,
        text: text.to_owned(),
        text_hash: "b".repeat(64),
        text_chars: text.chars().count() as u64,
        tool_name: Known::unknown(),
        segment_id: 1,
        raw_unit_refs: Vec::new(),
    };
    model.turns = vec![
        turn(
            0,
            TurnRole::User,
            TurnKind::UserMsg,
            "please fix the parser",
            Known::value("2026-07-13T04:00:01Z".to_owned()),
        ),
        turn(
            1,
            TurnRole::Assistant,
            TurnKind::InternalThought,
            "thinking about the shape",
            Known::value("2026-07-13T04:00:02Z".to_owned()),
        ),
        turn(
            2,
            TurnRole::Assistant,
            TurnKind::ToolCall,
            "cargo test",
            Known::value("2026-07-13T04:00:03Z".to_owned()),
        ),
        // Unknown per-turn timestamp: must fall back to session provenance
        // start, flagged via timestamp_source — never a fabricated time.
        turn(
            3,
            TurnRole::Assistant,
            TurnKind::AgentReply,
            "done, gates green",
            Known::unknown(),
        ),
    ];
    model
}

#[test]
fn timeline_projection_is_pure_and_complete() {
    let model = synthetic_model();
    let entries = timeline_entries_from_model(&model);

    assert_eq!(entries.len(), model.turns.len(), "projection drops no turn");
    for entry in &entries {
        assert_eq!(entry.agent, "codex");
        assert_eq!(entry.session_id, model.session_id);
        assert!(
            entry.source_path.is_none(),
            "typed model is path-free; projections must not invent paths"
        );
        assert_eq!(
            entry.source_sha256.as_deref(),
            Some(model.provenance.original_source_hash.as_str())
        );
    }
    assert_eq!(entries[0].frame_kind, Some(FrameKind::UserMsg));
    assert_eq!(entries[1].frame_kind, Some(FrameKind::InternalThought));
    assert_eq!(entries[2].frame_kind, Some(FrameKind::ToolCall));
    assert_eq!(entries[3].frame_kind, Some(FrameKind::AgentReply));
    assert_eq!(
        entries[3].timestamp_source.as_deref(),
        Some("session_provenance"),
        "unknown turn timestamp falls back to session start, explicitly flagged"
    );
    assert_eq!(
        entries[3].timestamp.to_rfc3339(),
        "2026-07-13T04:00:00+00:00"
    );

    // Determinism: the projection is a pure function of the model.
    let first = serde_json::to_string(&entries).expect("serialize projection");
    let second =
        serde_json::to_string(&timeline_entries_from_model(&model)).expect("serialize again");
    assert_eq!(first, second, "projection must be deterministic");
}

#[test]
fn conversation_projection_keeps_only_user_and_agent_frames() {
    let model = synthetic_model();
    let messages = conversation_messages_from_model(&model, None);

    assert_eq!(
        messages.len(),
        2,
        "reasoning and tool frames must not enter the conversation projection"
    );
    assert_eq!(messages[0].role, "user");
    assert_eq!(messages[0].message, "please fix the parser");
    assert_eq!(messages[1].role, "assistant");
    assert_eq!(messages[1].message, "done, gates green");
    assert_eq!(
        messages[0].repo_project, "aicx",
        "repo identity derives from the model's segment cwd"
    );

    let overridden = conversation_messages_from_model(&model, Some("vetcoders"));
    assert!(overridden.iter().all(|m| m.repo_project == "vetcoders"));
}
