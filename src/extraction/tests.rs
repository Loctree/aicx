use super::*;
use chrono::TimeZone;

#[test]
fn test_conversation_first_excludes_reasoning() {
    let entries = vec![
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess1".to_string(),
            role: "user".to_string(),
            message: "Fix the auth middleware".to_string(),
            branch: Some("main".to_string()),
            cwd: Some("/home/user/myrepo".to_string()),
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 30).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess1".to_string(),
            role: "assistant".to_string(),
            message: "I'll refactor the auth module to use JWT tokens.".to_string(),
            branch: Some("main".to_string()),
            cwd: Some("/home/user/myrepo".to_string()),
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 1, 0).unwrap(),
            agent: "codex".to_string(),
            session_id: "sess2".to_string(),
            role: "reasoning".to_string(),
            message: "Thinking about the best approach...".to_string(),
            branch: None,
            cwd: Some("/home/user/myrepo".to_string()),
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 1, 30).unwrap(),
            agent: "gemini".to_string(),
            session_id: "sess3".to_string(),
            role: "reasoning".to_string(),
            message: "**Analysis**: Checking dependencies".to_string(),
            branch: None,
            cwd: Some("/home/user/myrepo".to_string()),
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
            frame_kind: None,
        },
    ];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 2);
    assert_eq!(conv[0].role, "user");
    assert_eq!(conv[0].message, "Fix the auth middleware");
    assert_eq!(conv[1].role, "assistant");
    assert_eq!(
        conv[1].message,
        "I'll refactor the auth module to use JWT tokens."
    );
    assert!(conv.iter().all(|m| m.role != "reasoning"));
}

#[test]
fn test_conversation_first_preserves_full_messages() {
    let long_msg = "A".repeat(50_000);
    let entries = vec![TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "sess1".to_string(),
        role: "user".to_string(),
        message: long_msg.clone(),
        branch: None,
        cwd: None,
        timestamp_source: None,
        source_path: None,
        source_sha256: None,
        source_line_span: None,
        frame_kind: None,
    }];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 1);
    assert_eq!(conv[0].message.len(), 50_000);
    assert_eq!(conv[0].message, long_msg);
}

#[test]
fn test_conversation_first_repo_project_identity() {
    let entries = vec![
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess1".to_string(),
            role: "user".to_string(),
            message: "hello".to_string(),
            branch: None,
            cwd: Some("/Users/user/hosted/Vetcoders/ai-contexters".to_string()),
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 1, 0).unwrap(),
            agent: "codex".to_string(),
            session_id: "sess2".to_string(),
            role: "assistant".to_string(),
            message: "world".to_string(),
            branch: None,
            cwd: None,
            timestamp_source: None,
            source_path: None,
            source_sha256: None,
            source_line_span: None,
            frame_kind: None,
        },
    ];

    let conv = to_conversation(&entries, &["ai-contexters".to_string()]);
    assert_eq!(conv[0].repo_project, "ai-contexters");
    assert_eq!(conv[1].repo_project, "ai-contexters");
    assert_eq!(
        conv[0].source_path.as_deref(),
        Some("/Users/user/hosted/Vetcoders/ai-contexters")
    );
    assert!(conv[1].source_path.is_none());
}

#[test]
fn test_conversation_first_preserves_provenance() {
    let entries = vec![TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 14, 30, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "abc12345-6789-session-uuid".to_string(),
        role: "user".to_string(),
        message: "Deploy to production".to_string(),
        branch: Some("release/v2".to_string()),
        cwd: Some("/home/user/project".to_string()),
        timestamp_source: None,
        source_path: None,
        source_sha256: None,
        source_line_span: None,
        frame_kind: None,
    }];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 1);
    let msg = &conv[0];
    assert_eq!(msg.session_id, "abc12345-6789-session-uuid");
    assert_eq!(msg.agent, "claude");
    assert_eq!(msg.branch.as_deref(), Some("release/v2"));
    assert_eq!(
        msg.timestamp,
        Utc.with_ymd_and_hms(2026, 3, 21, 14, 30, 0).unwrap()
    );
}

fn conversation_entry(session_id: &str, role: &str, message: &str, second: u32) -> TimelineEntry {
    conversation_entry_agent("claude", session_id, role, message, second)
}

fn conversation_entry_agent(
    agent: &str,
    session_id: &str,
    role: &str,
    message: &str,
    second: u32,
) -> TimelineEntry {
    TimelineEntry {
        timestamp: Utc.with_ymd_and_hms(2026, 3, 21, 10, 0, second).unwrap(),
        agent: agent.to_string(),
        session_id: session_id.to_string(),
        role: role.to_string(),
        message: message.to_string(),
        branch: None,
        cwd: None,
        timestamp_source: None,
        source_path: None,
        source_sha256: None,
        source_line_span: None,
        frame_kind: None,
    }
}

#[test]
fn test_conversation_exact_short_duplicate_user_within_2s_is_dropped() {
    let entries = vec![
        conversation_entry("sess1", "user", "Hello agent", 0),
        conversation_entry("sess1", "user", "  Hello agent  ", 1),
    ];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 1);
    assert_eq!(conv[0].message, "Hello agent");
}

#[test]
fn test_conversation_extract_stats_counts_exact_short_duplicate_drop() {
    let entries = vec![
        conversation_entry("sess1", "user", "Hello agent", 0),
        conversation_entry("sess1", "user", "Hello agent", 1),
    ];

    let projection = to_conversation_with_stats(&entries, &[]);
    assert_eq!(projection.messages.len(), 1);
    assert_eq!(projection.exact_short_duplicates_dropped, 1);
}

#[test]
fn test_conversation_extract_stats_zero_without_duplicate() {
    let entries = vec![
        conversation_entry("sess1", "user", "Hello agent", 0),
        conversation_entry("sess1", "assistant", "Hello user", 1),
    ];

    let projection = to_conversation_with_stats(&entries, &[]);
    assert_eq!(projection.messages.len(), 2);
    assert_eq!(projection.exact_short_duplicates_dropped, 0);
}

#[test]
fn test_conversation_exact_short_duplicate_user_after_2s_is_kept() {
    let entries = vec![
        conversation_entry("sess1", "user", "Hello agent", 0),
        conversation_entry("sess1", "user", "Hello agent", 3),
    ];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 2);
}

#[test]
fn test_conversation_different_short_user_within_2s_is_kept() {
    let entries = vec![
        conversation_entry("sess1", "user", "Hello agent", 0),
        conversation_entry("sess1", "user", "Hello other agent", 1),
    ];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 2);
}

#[test]
fn test_conversation_long_exact_duplicate_user_within_2s_is_kept() {
    let long = "A".repeat(1001);
    let entries = vec![
        conversation_entry("sess1", "user", &long, 0),
        conversation_entry("sess1", "user", &long, 1),
    ];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 2);
}

#[test]
fn test_conversation_exact_short_duplicate_assistant_within_2s_is_kept() {
    let entries = vec![
        conversation_entry("sess1", "assistant", "Sure.", 0),
        conversation_entry("sess1", "assistant", "Sure.", 1),
    ];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 2);
}

#[test]
fn test_conversation_message_kind_defaults_to_conversation() {
    let entries = vec![conversation_entry("sess1", "user", "Hello agent", 0)];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv[0].message_kind, MessageKind::Conversation);
    assert_eq!(conv[0].collapse_stub_kind, None);
}

#[test]
fn test_conversation_message_kind_serializes_to_json_metadata() {
    let entries = vec![conversation_entry("sess1", "user", "Hello agent", 0)];

    let conv = to_conversation(&entries, &[]);
    let value = serde_json::to_value(&conv[0]).unwrap();
    assert_eq!(value["message_kind"], "conversation");
    assert!(value.get("collapse_stub_kind").is_none());
}

#[test]
fn test_conversation_message_kind_detects_workflow_prompt() {
    let message = "run_id: run-1\nprompt_id: prompt-1\nstatus: prompt\nPerform the vc-review task\nReport path: /tmp/report.md";
    let entries = vec![conversation_entry("sess1", "user", message, 0)];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv[0].message_kind, MessageKind::WorkflowPrompt);
    assert_eq!(conv[0].collapse_stub_kind, None);
}

#[test]
fn test_conversation_message_kind_detects_continuation_summary() {
    let entries = vec![conversation_entry(
        "sess1",
        "user",
        "This session is being continued from an earlier conversation.",
        0,
    )];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv[0].message_kind, MessageKind::ContinuationSummary);
    assert_eq!(conv[0].collapse_stub_kind, None);
}

#[test]
fn test_conversation_message_kind_detects_skill_ref_stub() {
    let entries = vec![conversation_entry(
        "sess1",
        "user",
        "  <skill-ref: repeated content>",
        0,
    )];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv[0].message_kind, MessageKind::CollapseStub);
    assert_eq!(conv[0].collapse_stub_kind, Some(CollapseStubKind::SkillRef));
}

#[test]
fn test_conversation_message_kind_detects_dedup_ref_stub() {
    let entries = vec![conversation_entry(
        "sess1",
        "assistant",
        "<dedup-ref: repeated content>",
        0,
    )];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv[0].message_kind, MessageKind::CollapseStub);
    assert_eq!(conv[0].collapse_stub_kind, Some(CollapseStubKind::DedupRef));
}

#[test]
fn test_conversation_message_kind_does_not_mark_inline_dedup_ref_as_stub() {
    let entries = vec![conversation_entry(
        "sess1",
        "user",
        "Normal text mentioning <dedup-ref: but not as a stub.",
        0,
    )];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv[0].message_kind, MessageKind::Conversation);
    assert_eq!(conv[0].collapse_stub_kind, None);
}

#[test]
fn test_conversation_exact_short_duplicate_key_includes_agent() {
    // Two extractors can emit the same fallback session id (for example
    // claude history and codex history both fall back to "history" when
    // no `sessionId` is present). Without the agent in the dedup key,
    // identical short prompts within 2 s from unrelated agent streams
    // would be merged. Verify both messages survive.
    let entries = vec![
        conversation_entry_agent("claude", "history", "user", "ping", 0),
        conversation_entry_agent("codex", "history", "user", "ping", 1),
    ];

    let conv = to_conversation(&entries, &[]);
    assert_eq!(conv.len(), 2, "agent must be part of the dedup key");
}
