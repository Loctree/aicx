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
            cwd: Some("/Users/maciejgad/hosted/VetCoders/ai-contexters".to_string()),
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
            frame_kind: None,
        },
    ];

    let conv = to_conversation(&entries, &["ai-contexters".to_string()]);
    assert_eq!(conv[0].repo_project, "ai-contexters");
    assert_eq!(conv[1].repo_project, "ai-contexters");
    assert_eq!(
        conv[0].source_path.as_deref(),
        Some("/Users/maciejgad/hosted/VetCoders/ai-contexters")
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

#[test]
fn test_extract_claude_excludes_tool_blocks_then_conversation_clean() {
    use std::fs;
    let tmp = std::env::temp_dir().join(format!(
        "ai-ctx-conv-tool-blocks-{}-{}.jsonl",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ));
    let _ = fs::remove_file(&tmp);

    let content = concat!(
        r#"{"type":"user","message":{"role":"user","content":"Hello agent"},"timestamp":"2026-03-21T10:00:00.000Z","sessionId":"s1","cwd":"/tmp"}"#,
        "\n",
        r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Let me check."},{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"ls"}},{"type":"text","text":"Here are the files."}]},"timestamp":"2026-03-21T10:00:01.000Z","sessionId":"s1"}"#,
        "\n",
        r#"{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]},"timestamp":"2026-03-21T10:00:02.000Z","sessionId":"s1"}"#
    );
    fs::write(&tmp, content).unwrap();

    let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff,
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    assert!(
        entries.len() >= 2,
        "expected at least user + assistant entries, got {}",
        entries.len()
    );
    let user_msgs: Vec<_> = entries
        .iter()
        .filter(|e| e.frame_kind == Some(FrameKind::UserMsg))
        .collect();
    let agent_msgs: Vec<_> = entries
        .iter()
        .filter(|e| e.frame_kind == Some(FrameKind::AgentReply))
        .collect();
    assert!(!user_msgs.is_empty());
    assert!(!agent_msgs.is_empty());
    assert_eq!(user_msgs[0].message, "Hello agent");
    assert!(
        agent_msgs
            .iter()
            .any(|e| e.message.contains("Let me check"))
    );

    let conv = to_conversation(&entries, &[]);
    assert!(
        conv.len() >= 2,
        "conversation should have at least user + assistant, got {}",
        conv.len()
    );
    assert_eq!(conv[0].message, "Hello agent");

    let _ = fs::remove_file(&tmp);
}
