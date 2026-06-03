use super::*;

#[test]
fn test_conversation_json_extract_stats_are_additive() {
    let dir = unique_test_dir("conversation_extract_stats");
    let path = dir.join("conversation.json");
    let messages = vec![ConversationMessage {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "sess-stats".to_string(),
        role: "user".to_string(),
        message: "Hello".to_string(),
        repo_project: "test".to_string(),
        source_path: None,
        branch: None,
        message_kind: crate::timeline::MessageKind::Conversation,
        collapse_stub_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 2,
        sessions: vec!["sess-stats".to_string()],
    };
    let stats = ConversationExtractStats {
        aicx_version: env!("CARGO_PKG_VERSION"),
        redaction_enabled: true,
        raw_entries: 2,
        conversation_messages: messages.len(),
        conversation_projection: "user_assistant_only",
        exact_short_duplicates_dropped: 1,
    };

    write_conversation_json(&path, &messages, &metadata, &stats).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(
        parsed["extract_stats"]["aicx_version"],
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(parsed["extract_stats"]["redaction_enabled"], true);
    assert_eq!(parsed["extract_stats"]["raw_entries"], 2);
    assert_eq!(parsed["extract_stats"]["conversation_messages"], 1);
    assert_eq!(
        parsed["extract_stats"]["conversation_projection"],
        "user_assistant_only"
    );
    assert_eq!(parsed["extract_stats"]["exact_short_duplicates_dropped"], 1);
    assert_eq!(
        parsed["extract_stats"]["conversation_messages"],
        parsed["messages"].as_array().unwrap().len()
    );
    assert!(
        parsed["extract_stats"]["raw_entries"].as_u64().unwrap()
            >= parsed["extract_stats"]["conversation_messages"]
                .as_u64()
                .unwrap()
    );

    cleanup(&dir);
}

#[test]
fn test_conversation_json_extract_stats_can_report_redaction_disabled() {
    let dir = unique_test_dir("conversation_extract_stats_redaction_disabled");
    let path = dir.join("conversation.json");
    let messages = vec![ConversationMessage {
        timestamp: Utc.with_ymd_and_hms(2026, 1, 23, 12, 0, 0).unwrap(),
        agent: "claude".to_string(),
        session_id: "sess-stats".to_string(),
        role: "user".to_string(),
        message: "Hello".to_string(),
        repo_project: "test".to_string(),
        source_path: None,
        branch: None,
        message_kind: crate::timeline::MessageKind::Conversation,
        collapse_stub_kind: None,
    }];
    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 1, 23, 13, 0, 0).unwrap(),
        project_filter: Some("test".to_string()),
        hours_back: 24,
        total_entries: 1,
        sessions: vec!["sess-stats".to_string()],
    };
    let stats = ConversationExtractStats {
        aicx_version: env!("CARGO_PKG_VERSION"),
        redaction_enabled: false,
        raw_entries: 1,
        conversation_messages: messages.len(),
        conversation_projection: "user_assistant_only",
        exact_short_duplicates_dropped: 0,
    };

    write_conversation_json(&path, &messages, &metadata, &stats).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["extract_stats"]["redaction_enabled"], false);

    cleanup(&dir);
}

#[test]
fn write_conversation_outputs_redact_by_default() {
    let dir = unique_test_dir("conversation_redact_default");
    let md_path = dir.join("conversation.md");
    let json_path = dir.join("conversation.json");
    let raw = "raw token sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX should not leak";
    let messages = vec![conversation_message(raw)];
    let metadata = conversation_metadata(messages.len());
    let stats = conversation_stats(true, messages.len());

    write_conversation_markdown(&md_path, &messages, &metadata).unwrap();
    write_conversation_json(&json_path, &messages, &metadata, &stats).unwrap();

    let md = fs::read_to_string(&md_path).unwrap();
    let json = fs::read_to_string(&json_path).unwrap();
    assert!(!md.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX"));
    assert!(!json.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX"));
    assert!(md.contains("[REDACTED_ANTHROPIC_KEY]"));
    assert!(json.contains("[REDACTED_ANTHROPIC_KEY]"));

    cleanup(&dir);
}

#[test]
fn write_conversation_outputs_can_preserve_raw_with_explicit_flag() {
    let dir = unique_test_dir("conversation_redact_opt_out");
    let md_path = dir.join("conversation.md");
    let json_path = dir.join("conversation.json");
    let raw = "raw token sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX preserved";
    let messages = vec![conversation_message(raw)];
    let metadata = conversation_metadata(messages.len());
    let stats = conversation_stats(false, messages.len());

    write_conversation_markdown_with_redaction(&md_path, &messages, &metadata, false).unwrap();
    write_conversation_json_with_redaction(&json_path, &messages, &metadata, &stats, false)
        .unwrap();

    let md = fs::read_to_string(&md_path).unwrap();
    let json = fs::read_to_string(&json_path).unwrap();
    assert!(md.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX"));
    assert!(json.contains("sk-ant-api03-ABCDEFGHIJKLMNOPQRSTUVWX"));

    cleanup(&dir);
}

// --- Loctree ---
