use super::*;
use filetime::{FileTime, set_file_mtime};
use std::fs;
use std::path::PathBuf;

const CLAUDE_FRAME_KIND_FIXTURE: &str =
    include_str!("../../tests/fixtures/frame_kind/claude_session.jsonl");
const CODEX_FRAME_KIND_FIXTURE: &str =
    include_str!("../../tests/fixtures/frame_kind/codex_session.jsonl");
const GEMINI_FRAME_KIND_FIXTURE: &str =
    include_str!("../../tests/fixtures/frame_kind/gemini_session.json");
const GEMINI_ANTIGRAVITY_FRAME_KIND_FIXTURE: &str =
    include_str!("../../tests/fixtures/frame_kind/gemini_antigravity_conversation.json");

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "ai-contexters-{name}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, content).unwrap();
}

fn set_mtime(path: &Path, unix_seconds: i64) {
    set_file_mtime(path, FileTime::from_unix_time(unix_seconds, 0)).unwrap();
}

fn frame_kinds(entries: &[TimelineEntry]) -> Vec<Option<FrameKind>> {
    entries.iter().map(|entry| entry.frame_kind).collect()
}

#[test]
fn test_repo_name_from_cwd() {
    // Fallback behavior
    assert_eq!(
        repo_name_from_cwd(Some("/Users/polyversai/Libraxis/lbrx-services"), &[]),
        "lbrx-services"
    );
    assert_eq!(
        repo_name_from_cwd(Some("/Users/polyversai/Libraxis/mlx-batch-runner"), &[]),
        "mlx-batch-runner"
    );
    assert_eq!(repo_name_from_cwd(None, &[]), "unknown");
    assert_eq!(repo_name_from_cwd(Some("/"), &[]), "unknown");
    assert_eq!(repo_name_from_cwd(Some(""), &[]), "unknown");

    // Single project filter
    assert_eq!(
        repo_name_from_cwd(
            Some("/Users/polyversai/Libraxis/lbrx-services/subfolder"),
            &["lbrx".to_string()]
        ),
        "lbrx"
    );

    // Multiple project filters
    let filters = vec!["lbrx-services".to_string(), "foo".to_string()];
    assert_eq!(
        repo_name_from_cwd(
            Some("/Users/polyversai/Libraxis/lbrx-services/subfolder"),
            &filters
        ),
        "lbrx-services"
    );
}

#[test]
fn test_decode_claude_project_path_with_leading_dash() {
    let encoded = "-Users-maciejgad-hosted-VetCoders-CodeScribe";
    let decoded = decode_claude_project_path(encoded);
    assert_eq!(decoded, "Users/maciejgad/hosted/VetCoders/CodeScribe");
}

#[test]
fn test_decode_claude_project_path_without_leading_dash() {
    let encoded = "Users-maciejgad-projects-foo";
    let decoded = decode_claude_project_path(encoded);
    assert_eq!(decoded, "Users/maciejgad/projects/foo");
}

#[test]
fn test_decode_claude_project_path_single_segment() {
    let encoded = "-home";
    let decoded = decode_claude_project_path(encoded);
    assert_eq!(decoded, "home");
}

#[test]
fn test_decode_claude_project_path_empty() {
    let decoded = decode_claude_project_path("");
    assert_eq!(decoded, "");
}

#[test]
fn test_decode_claude_project_path_deep_nesting() {
    let encoded = "-a-b-c-d-e-f";
    let decoded = decode_claude_project_path(encoded);
    assert_eq!(decoded, "a/b/c/d/e/f");
}

#[test]
fn test_extract_claude_file_parses_text_only_blocks() {
    let root = unique_test_dir("claude-direct");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"type":"user","message":{"role":"user","content":"Hello"},"timestamp":"2026-02-09T22:03:06.765Z","sessionId":"sess123","gitBranch":"main","cwd":"/tmp"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Hi"}]},"timestamp":"2026-02-09T22:03:07.765Z","sessionId":"sess123"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"toolu_1","name":"Bash","input":{"command":"echo hi"}}]},"timestamp":"2026-02-09T22:03:08.765Z","sessionId":"sess123"}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_1","content":"ok"}]},"timestamp":"2026-02-09T22:03:09.765Z","sessionId":"sess123"}"#;
    write_file(&tmp, content);

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
        "expected at least user + assistant text entries, got {}",
        entries.len()
    );
    let user_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.frame_kind == Some(FrameKind::UserMsg))
        .collect();
    let agent_entries: Vec<_> = entries
        .iter()
        .filter(|e| e.frame_kind == Some(FrameKind::AgentReply))
        .collect();
    assert!(!user_entries.is_empty(), "expected at least one user entry");
    assert!(
        !agent_entries.is_empty(),
        "expected at least one agent reply"
    );
    assert_eq!(user_entries[0].message, "Hello");
    assert_eq!(agent_entries[0].message, "Hi");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_claude_file_classifies_frame_kinds_from_fixture() {
    let root = unique_test_dir("claude-frame-kind");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    write_file(&tmp, CLAUDE_FRAME_KIND_FIXTURE);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    assert_eq!(
        frame_kinds(&entries),
        vec![
            Some(FrameKind::UserMsg),
            Some(FrameKind::AgentReply),
            Some(FrameKind::InternalThought),
            Some(FrameKind::ToolCall),
            Some(FrameKind::ToolCall),
        ]
    );
    assert_eq!(entries[0].message, "User asks for frame separation");
    assert_eq!(entries[1].message, "Visible assistant reply");
    assert!(entries[2].message.contains("Hidden chain of thought"));
    assert!(entries[3].message.contains("rg frame_kind"));
    assert!(entries[4].message.contains("tool output here"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_codex_file_history_format() {
    let root = unique_test_dir("codex-direct-history");
    let tmp = root.join("history.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"session_id":"s1","text":"hello","ts":1000,"role":"user","cwd":"/tmp/a"}
{"session_id":"s1","text":"hi back","ts":1001,"role":"assistant","cwd":"/tmp/a"}
{"session_id":"s2","text":"unrelated","ts":2000,"role":"user","cwd":"/tmp/b"}"#;
    write_file(&tmp, content);

    let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff,
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_codex_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].agent, "codex");
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[1].role, "assistant");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_codex_file_session_format_detects() {
    let root = unique_test_dir("codex-direct-session");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    // Minimal session file (no event_msg) should parse and yield 0 entries.
    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"sess","cwd":"/tmp/x"}}"#;
    write_file(&tmp, content);

    let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff,
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_codex_file(&tmp, &config).unwrap();
    assert!(entries.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_codex_file_classifies_frame_kinds_from_fixture() {
    let root = unique_test_dir("codex-frame-kind");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    write_file(&tmp, CODEX_FRAME_KIND_FIXTURE);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_codex_file(&tmp, &config).unwrap();
    assert_eq!(
        frame_kinds(&entries),
        vec![
            Some(FrameKind::UserMsg),
            Some(FrameKind::AgentReply),
            Some(FrameKind::InternalThought),
            Some(FrameKind::ToolCall),
        ]
    );
    assert_eq!(entries[0].message, "User asks for frame separation");
    assert_eq!(entries[1].message, "Visible assistant reply");
    assert_eq!(entries[2].message, "Hidden chain of thought");
    assert!(entries[3].message.contains("searchDocs"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_file_session_json() {
    let root = unique_test_dir("gemini-direct");
    let tmp = root.join("session.json");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{
  "sessionId": "sess-1",
  "projectHash": "hash-1",
  "messages": [
    {"type":"user","content":"hi","timestamp":"2026-02-01T00:00:00Z","thoughts":[]},
    {"type":"gemini","content":"hello","timestamp":"2026-02-01T00:00:01Z","thoughts":[]},
    {"type":"info","content":"skip me","timestamp":"2026-02-01T00:00:02Z","thoughts":[]}
  ]
}"#;
    write_file(&tmp, content);

    let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff,
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].agent, "gemini");
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[1].role, "assistant");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_file_classifies_frame_kinds_from_fixture() {
    let root = unique_test_dir("gemini-frame-kind");
    let tmp = root.join("session.json");
    let _ = fs::remove_dir_all(&root);
    write_file(&tmp, GEMINI_FRAME_KIND_FIXTURE);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_file(&tmp, &config).unwrap();
    assert_eq!(
        frame_kinds(&entries),
        vec![
            Some(FrameKind::UserMsg),
            Some(FrameKind::AgentReply),
            Some(FrameKind::InternalThought),
            Some(FrameKind::ToolCall),
        ]
    );
    assert_eq!(entries[0].message, "User asks for frame separation");
    assert_eq!(entries[1].message, "Visible assistant reply");
    assert_eq!(entries[2].message, "Hidden chain of thought");
    assert!(entries[3].message.contains("searchDocs"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_junie_file_keeps_conversation_truth_and_dedups_results() {
    let root = unique_test_dir("junie-direct");
    let session_dir = root.join("session-260408-214715-abcd");
    let tmp = session_dir.join("events.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"CurrentDirectoryUpdatedEvent","currentDirectory":"/tmp/repo"}}}
{"kind":"UserPromptEvent","requestId":"prompt-260408-214823-br8l","prompt":"vc-init","presentablePrompt":"vc-init"}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-1","cancelled":false,"result":"Initial plan","changes":[],"errorCode":"Submit"}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-1","cancelled":false,"result":"Initial plan","changes":[],"errorCode":"Submit"}}}
{"kind":"UserResponseEvent","prompt":"jedziemy","isChoice":true}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-1","cancelled":false,"result":"Refined plan","changes":[],"errorCode":"Submit"}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","command":"rg foo","output":"this should stay ignored"}}}"#;
    write_file(&tmp, content);

    let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff,
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_junie_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 4);
    assert_eq!(entries[0].agent, "junie");
    assert_eq!(entries[0].session_id, "260408-214715-abcd");
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].message, "vc-init");
    assert_eq!(entries[1].role, "assistant");
    assert_eq!(entries[1].message, "Initial plan");
    assert_eq!(entries[2].role, "user");
    assert_eq!(entries[2].message, "jedziemy");
    assert_eq!(entries[3].role, "assistant");
    assert_eq!(entries[3].message, "Refined plan");
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0].timestamp < pair[1].timestamp)
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.cwd.as_deref() == Some("/tmp/repo"))
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_junie_file_honors_user_only_mode() {
    let root = unique_test_dir("junie-direct-user-only");
    let session_dir = root.join("session-260408-214715-efgh");
    let tmp = session_dir.join("events.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"kind":"UserPromptEvent","requestId":"prompt-260408-214823-br8l","prompt":"hello","presentablePrompt":"hello"}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"step-1","cancelled":false,"result":"assistant reply","changes":[],"errorCode":"Submit"}}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: false,
        watermark: None,
    };

    let entries = extract_junie_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].message, "hello");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_antigravity_prefers_conversation_artifacts_for_brain_input() {
    let root = unique_test_dir("gemini-antigravity-brain");
    let brain = root.join("brain").join("conv-1");
    let conversation_artifact = brain.join("conversation.json");
    let step_output = brain
        .join(".system_generated")
        .join("steps")
        .join("001")
        .join("output.txt");

    write_file(
        &conversation_artifact,
        r#"{
  "projectRoot": "/Users/tester/workspace/RepoAlpha",
  "messages": [
    {"role":"user","content":"Map the architecture","timestamp":"2026-02-01T00:00:00Z"},
    {"role":"assistant","content":"We should split extraction and reporting.","timestamp":"2026-02-01T00:00:01Z"}
  ]
}"#,
    );
    write_file(
        &step_output,
        r#"{"project":"/Users/tester/workspace/RepoIgnored","decision":"fallback should stay unused"}"#,
    );
    set_mtime(&conversation_artifact, 1_706_745_600);
    set_mtime(&step_output, 1_706_745_660);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_antigravity_file(&brain, &config).unwrap();
    assert_eq!(entries[0].role, "system");
    assert!(entries[0].message.contains("mode: conversation-artifacts"));
    assert!(entries[0].message.contains("RepoAlpha"));
    assert!(
        entries[0]
            .message
            .contains(&conversation_artifact.display().to_string())
    );
    assert!(
        !entries
            .iter()
            .any(|entry| entry.message.contains("step output fallback"))
    );
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.role == "user" || entry.role == "assistant")
            .count(),
        2
    );
    assert!(
        entries
            .iter()
            .filter(|entry| entry.role != "system")
            .all(|entry| entry.cwd.as_deref() == Some("/Users/tester/workspace/RepoAlpha"))
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_antigravity_classifies_frame_kinds_from_fixture() {
    let root = unique_test_dir("gemini-antigravity-frame-kind");
    let brain = root.join("brain").join("conv-frame-kind");
    let conversation_artifact = brain.join("conversation.json");
    let _ = fs::remove_dir_all(&root);

    write_file(
        &conversation_artifact,
        GEMINI_ANTIGRAVITY_FRAME_KIND_FIXTURE,
    );
    set_mtime(&conversation_artifact, 1_712_829_600);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_antigravity_file(&brain, &config).unwrap();
    assert_eq!(entries[0].role, "system");
    assert_eq!(entries[0].frame_kind, None);

    let conversation_entries: Vec<_> = entries
        .iter()
        .filter(|entry| entry.role != "system")
        .cloned()
        .collect();
    assert_eq!(
        frame_kinds(&conversation_entries),
        vec![
            Some(FrameKind::UserMsg),
            Some(FrameKind::AgentReply),
            Some(FrameKind::InternalThought),
            Some(FrameKind::ToolCall),
        ]
    );
    assert_eq!(
        conversation_entries[0].message,
        "User asks for frame separation"
    );
    assert_eq!(conversation_entries[1].message, "Visible assistant reply");
    assert_eq!(conversation_entries[2].message, "Hidden chain of thought");
    assert!(conversation_entries[3].message.contains("searchDocs"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_antigravity_pb_input_resolves_brain_and_falls_back_to_steps() {
    let root = unique_test_dir("gemini-antigravity-pb");
    let pb = root.join("conversations").join("conv-2.pb");
    let step_output = root
        .join("brain")
        .join("conv-2")
        .join(".system_generated")
        .join("steps")
        .join("007")
        .join("output.txt");

    write_file(&pb, "opaque");
    write_file(
        &step_output,
        r#"{"project":"/Users/tester/workspace/RepoBeta","decision":"Ship the extraction in additive mode."}"#,
    );
    set_mtime(&step_output, 1_706_745_720);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_antigravity_file(&pb, &config).unwrap();
    assert_eq!(entries[0].role, "system");
    assert!(entries[0].message.contains("mode: step-output-fallback"));
    assert!(
        entries[0]
            .message
            .contains("not a full conversation transcript")
    );
    assert!(entries[0].message.contains(&pb.display().to_string()));
    assert_eq!(entries[1].role, "artifact");
    assert!(entries[1].message.contains("step output fallback"));
    assert!(
        entries[1]
            .cwd
            .as_deref()
            .is_some_and(|cwd| cwd.ends_with("RepoBeta"))
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_antigravity_missing_brain_errors_honestly() {
    let root = unique_test_dir("gemini-antigravity-missing-brain");
    let pb = root.join("conversations").join("conv-3.pb");
    write_file(&pb, "opaque");

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let err = extract_gemini_antigravity_file(&pb, &config).unwrap_err();
    let message = err.to_string();
    assert!(message.contains("opaque/encrypted"));
    assert!(message.contains("brain/conv-3/"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_antigravity_brain_input_falls_back_explicitly() {
    let root = unique_test_dir("gemini-antigravity-brain-fallback");
    let brain = root.join("brain").join("conv-4");
    let step_a = brain
        .join(".system_generated")
        .join("steps")
        .join("002")
        .join("output.txt");
    let step_b = brain
        .join(".system_generated")
        .join("steps")
        .join("009")
        .join("output.txt");

    write_file(
        &step_a,
        r#"{"project":"RepoGamma","decision":"Prefer readable artifacts first."}"#,
    );
    write_file(
        &step_b,
        r#"{"decision":"Degrade to step outputs when chat artifacts are absent."}"#,
    );
    set_mtime(&step_a, 1_706_745_780);
    set_mtime(&step_b, 1_706_745_840);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_antigravity_file(&brain, &config).unwrap();
    assert!(entries[0].message.contains("mode: step-output-fallback"));
    assert!(entries[1].message.contains(&step_a.display().to_string()));
    assert!(entries[2].message.contains(&step_b.display().to_string()));
    assert_eq!(entries[1].cwd.as_deref(), Some("RepoGamma"));
    assert_eq!(entries[2].cwd.as_deref(), Some("RepoGamma"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_codex_session_filtering_includes_all_messages() {
    // Simulate the session-based filtering logic:
    // If any message in a session mentions the filter, all messages are included.

    let session_a_msgs = [
        ("s1", "work on CodeScribe refactoring", 1000i64),
        ("s1", "fix the bug in controller", 1001),
        ("s1", "done with changes", 1002),
    ];
    let session_b_msgs = [
        ("s2", "unrelated project work", 2000),
        ("s2", "more unrelated stuff", 2001),
    ];

    // Build sessions map
    let mut sessions: HashMap<String, Vec<(String, i64)>> = HashMap::new();
    for (sid, text, ts) in session_a_msgs.iter().chain(session_b_msgs.iter()) {
        sessions
            .entry(sid.to_string())
            .or_default()
            .push((text.to_string(), *ts));
    }

    let filter = "CodeScribe";
    let filter_lower = filter.to_lowercase();

    // Determine matching sessions
    let matching: HashSet<String> = sessions
        .iter()
        .filter(|(_id, msgs)| {
            msgs.iter()
                .any(|(text, _)| text.to_lowercase().contains(&filter_lower))
        })
        .map(|(id, _)| id.clone())
        .collect();

    // Session s1 should match (has "CodeScribe" in first message)
    assert!(matching.contains("s1"));
    // Session s2 should NOT match
    assert!(!matching.contains("s2"));

    // All 3 messages from s1 should be included, not just the one mentioning CodeScribe
    let included_count: usize = sessions
        .iter()
        .filter(|(id, _)| matching.contains(id.as_str()))
        .map(|(_, msgs)| msgs.len())
        .sum();
    assert_eq!(included_count, 3);
}

#[test]
fn test_codex_session_filtering_no_filter_includes_all() {
    let sessions: HashMap<String, Vec<(String, i64)>> = HashMap::from([
        (
            "s1".to_string(),
            vec![("msg1".to_string(), 1000), ("msg2".to_string(), 1001)],
        ),
        ("s2".to_string(), vec![("msg3".to_string(), 2000)]),
    ]);

    // No filter -> all sessions match
    let matching: HashSet<String> = sessions.keys().cloned().collect();
    assert_eq!(matching.len(), 2);
}

#[test]
fn test_codex_session_filtering_cwd_match() {
    // Simulate cwd-based matching
    let session_msgs: Vec<(Option<String>, String)> = vec![
        (
            Some("/Users/maciejgad/hosted/VetCoders/CodeScribe".to_string()),
            "run tests".to_string(),
        ),
        (None, "looks good".to_string()),
    ];

    let filter = "CodeScribe";
    let filter_lower = filter.to_lowercase();

    let session_matches = session_msgs.iter().any(|(cwd, text)| {
        text.to_lowercase().contains(&filter_lower)
            || cwd
                .as_ref()
                .is_some_and(|c| c.to_lowercase().contains(&filter_lower))
    });

    assert!(session_matches);
}

#[test]
fn test_extract_message_text_plain_string() {
    let msg = Some(serde_json::Value::String("hello world".to_string()));
    assert_eq!(extract_message_text(&msg), "hello world");
}

#[test]
fn test_extract_message_text_content_blocks() {
    let msg = Some(serde_json::json!([
        {"type": "text", "text": "first"},
        {"type": "image", "url": "..."},
        {"type": "text", "text": "second"}
    ]));
    assert_eq!(extract_message_text(&msg), "first\nsecond");
}

#[test]
fn test_extract_message_text_object_with_content_string() {
    let msg = Some(serde_json::json!({
        "role": "user",
        "content": "direct content"
    }));
    assert_eq!(extract_message_text(&msg), "direct content");
}

#[test]
fn test_extract_message_text_object_with_content_array() {
    let msg = Some(serde_json::json!({
        "role": "assistant",
        "content": [
            {"type": "text", "text": "response part 1"},
            {"type": "tool_use", "id": "abc"},
            {"type": "text", "text": "response part 2"}
        ]
    }));
    assert_eq!(
        extract_message_text(&msg),
        "response part 1\nresponse part 2"
    );
}

#[test]
fn test_extract_message_text_none() {
    assert_eq!(extract_message_text(&None), "");
}

#[test]
fn test_dedup_logic() {
    let entries = vec![
        TimelineEntry {
            timestamp: Utc.timestamp_opt(1000, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "s1".to_string(),
            role: "user".to_string(),
            message: "same message here".to_string(),
            branch: None,
            cwd: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.timestamp_opt(1000, 0).unwrap(),
            agent: "codex".to_string(),
            session_id: "s2".to_string(),
            role: "user".to_string(),
            message: "same message here".to_string(),
            branch: None,
            cwd: None,
            frame_kind: None,
        },
        TimelineEntry {
            timestamp: Utc.timestamp_opt(1001, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "s1".to_string(),
            role: "user".to_string(),
            message: "different".to_string(),
            branch: None,
            cwd: None,
            frame_kind: None,
        },
    ];

    let mut result = entries;
    let mut seen: HashSet<(i64, String)> = HashSet::new();
    result.retain(|entry| {
        let key_msg: String = entry.message.chars().take(100).collect();
        let key = (entry.timestamp.timestamp(), key_msg);
        seen.insert(key)
    });

    // First two have same timestamp + same message -> deduped to 1
    assert_eq!(result.len(), 2);
    assert_eq!(result[0].agent, "claude"); // first one kept
    assert_eq!(result[1].message, "different");
}

#[test]
fn test_gemini_message_with_content() {
    let msg = GeminiMessage {
        msg_type: Some("user".to_string()),
        content: Some(serde_json::Value::String("hello from gemini".to_string())),
        display_content: None,
        timestamp: Some("2026-01-20T19:50:45.683Z".to_string()),
        thoughts: vec![],
        role: None,
    };
    assert_eq!(
        render_gemini_message_content(&msg).as_deref(),
        Some("hello from gemini")
    );
    assert_eq!(msg.msg_type.as_deref().unwrap(), "user");
}

#[test]
fn test_gemini_message_type_mapping() {
    // "gemini" type maps to "assistant" role
    let msg = GeminiMessage {
        msg_type: Some("gemini".to_string()),
        content: Some(serde_json::Value::String("response text".to_string())),
        display_content: None,
        timestamp: Some("2026-01-20T19:50:51.778Z".to_string()),
        thoughts: vec![],
        role: None,
    };
    let role = match msg.msg_type.as_deref().unwrap_or("user") {
        "gemini" => "assistant",
        "user" => "user",
        _ => "skip",
    };
    assert_eq!(role, "assistant");
}

#[test]
fn test_gemini_message_skip_error_info() {
    // "error" and "info" types should be skipped
    for msg_type in &["error", "info"] {
        let msg = GeminiMessage {
            msg_type: Some(msg_type.to_string()),
            content: Some(serde_json::Value::String("some system message".to_string())),
            display_content: None,
            timestamp: Some("2026-01-20T19:16:15.218Z".to_string()),
            thoughts: vec![],
            role: None,
        };
        let role = match msg.msg_type.as_deref().unwrap_or("user") {
            "user" => Some("user"),
            "gemini" => Some("assistant"),
            _ => None, // skip
        };
        assert_eq!(role, None);
    }
}

#[test]
fn test_gemini_session_deserialization() {
    // Full round-trip: JSON with unknown fields (id, model, thoughts, tokens)
    // must deserialize without errors — serde ignores unknown fields by default.
    let json = r#"{
            "sessionId": "a45ff16f-2a8c-4a45-b690-2c2aaf631b71",
            "projectHash": "fef6ad02174d592d21e7f8a6143564388027ec0c38bbb44dec26e99f9cd9140f",
            "startTime": "2026-01-20T19:50:45.683Z",
            "lastUpdated": "2026-01-20T19:54:06.680Z",
            "messages": [
                {
                    "id": "772f4448-0cda-4256-8d89-121dc68776b7",
                    "timestamp": "2026-01-20T19:50:45.683Z",
                    "type": "user",
                    "content": "siemka!"
                },
                {
                    "id": "64b73173-3b0f-4838-9121-5dfd1f1bb5e1",
                    "timestamp": "2026-01-20T19:50:51.778Z",
                    "type": "gemini",
                    "content": "Cześć Maciej.",
                    "model": "gemini-3-flash-preview",
                    "thoughts": [{"subject": "test", "description": "ignored"}],
                    "tokens": {"input": 100, "output": 25}
                }
            ]
        }"#;

    let session: GeminiSession = serde_json::from_str(json).unwrap();
    assert_eq!(
        session.session_id.as_deref(),
        Some("a45ff16f-2a8c-4a45-b690-2c2aaf631b71")
    );
    assert_eq!(session.messages.len(), 2);
    assert_eq!(session.messages[0].msg_type.as_deref(), Some("user"));
    assert_eq!(
        session.messages[0].content.as_ref(),
        Some(&serde_json::Value::String("siemka!".to_string()))
    );
    assert_eq!(session.messages[1].msg_type.as_deref(), Some("gemini"));
    assert_eq!(
        session.messages[1].content.as_ref(),
        Some(&serde_json::Value::String("Cześć Maciej.".to_string()))
    );
}

#[test]
fn test_render_gemini_content_value_preserves_structured_blocks() {
    let value = serde_json::json!([
        {"text": "co to jest reachy mini? @../../../.gemini/tmp/codescribe/images/clipboard-1773858428029.png"},
        {"text": "\n--- Content from referenced files ---"},
        {"inlineData": {"mimeType": "image/png", "data": "abc123"}},
        {"text": "\n--- End of content ---"}
    ]);

    let rendered = render_gemini_content_value(&value).unwrap();
    assert!(rendered.contains("co to jest reachy mini?"));
    assert!(rendered.contains("--- Content from referenced files ---"));
    assert!(rendered.contains("[inlineData omitted: mimeType=image/png, data_chars=6]"));
    assert!(rendered.contains("--- End of content ---"));
}

#[test]
fn test_render_gemini_content_value_supports_object_shapes() {
    let value = serde_json::json!({
        "content": [
            {"text": "first line"},
            {"fileData": {"mimeType": "text/plain", "fileUri": "file:///tmp/note.txt"}}
        ]
    });

    let rendered = render_gemini_content_value(&value).unwrap();
    assert!(rendered.contains("first line"));
    assert!(rendered.contains("file:///tmp/note.txt"));
    assert!(rendered.contains("mimeType=text/plain"));
}

#[test]
fn test_extract_gemini_file_preserves_user_array_content() {
    let root = unique_test_dir("gemini-array-user");
    let tmp = root.join("session.json");
    let _ = fs::remove_dir_all(&root);

    let content = r##"{
  "sessionId": "sess-array",
  "messages": [
    {
      "type":"user",
      "content":[
        {"text":"# Task: Gemini truth repair"},
        {"text":"- preserve user arrays honestly"}
      ],
      "timestamp":"2026-02-01T00:00:00Z"
    },
    {
      "type":"gemini",
      "content":"working on it",
      "timestamp":"2026-02-01T00:00:01Z"
    }
  ]
}"##;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].role, "user");
    assert_eq!(
        entries[0].message,
        "# Task: Gemini truth repair\n- preserve user arrays honestly"
    );
    assert_eq!(entries[1].role, "assistant");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_file_keeps_inline_data_as_explicit_placeholder() {
    let root = unique_test_dir("gemini-inline-data");
    let tmp = root.join("session.json");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{
  "sessionId": "sess-inline",
  "messages": [
    {
      "type":"user",
      "timestamp":"2026-02-01T00:00:00Z",
      "content":[
        {"text":"co to jest reachy mini? @../../../.gemini/tmp/codescribe/images/clipboard-1773858428029.png"},
        {"text":"\n--- Content from referenced files ---"},
        {"inlineData":{"mimeType":"image/png","data":"abc123"}},
        {"text":"\n--- End of content ---"}
      ],
      "displayContent":[
        {"text":"co to jest reachy mini? @../../../.gemini/tmp/codescribe/images/clipboard-1773858428029.png"}
      ]
    },
    {
      "type":"gemini",
      "timestamp":"2026-02-01T00:00:01Z",
      "content":"To jest humanoidalny robot."
    }
  ]
}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries[0].message.contains("co to jest reachy mini?"));
    assert!(
        entries[0]
            .message
            .contains("[inlineData omitted: mimeType=image/png, data_chars=6]")
    );
    assert!(entries[1].message.contains("humanoidalny robot"));

    let _ = fs::remove_dir_all(&root);
}
