use super::*;
use crate::chunker::{ChunkMetadataSidecar, ChunkerConfig, chunk_entries};
use chrono::{Duration, TimeZone};
use filetime::{FileTime, set_file_mtime};
use std::collections::HashMap;
use std::fs;
use std::io::Cursor;
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

fn default_config(include_assistant: bool) -> ExtractionConfig {
    ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant,
        watermark: None,
    }
}

fn set_mtime(path: &Path, unix_seconds: i64) {
    set_file_mtime(path, FileTime::from_unix_time(unix_seconds, 0)).unwrap();
}

/// Render a path the way the extractors embed it in their messages. Production
/// runs every source path through `sanitize::validate_read_path`, which
/// canonicalizes and strips the Windows `\\?\` verbatim prefix. These tests
/// build raw `env::temp_dir()` paths, so on windows-msvc the raw form (8.3 short
/// names, non-canonical casing) never appears verbatim in the embedded long
/// form — assert against the same canonicalized rendering production emits. On
/// Unix the temp paths already round-trip, so this is effectively a no-op there.
fn canonical_display(path: &Path) -> String {
    sanitize::validate_read_path(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .display()
        .to_string()
}

fn frame_kinds(entries: &[TimelineEntry]) -> Vec<Option<FrameKind>> {
    entries.iter().map(|entry| entry.frame_kind).collect()
}

#[test]
fn test_repo_name_from_cwd() {
    // Fallback behavior
    assert_eq!(
        repo_name_from_cwd(Some("/Users/user/test-org/lbrx-services"), &[]),
        "lbrx-services"
    );
    assert_eq!(
        repo_name_from_cwd(Some("/Users/user/test-org/mlx-batch-runner"), &[]),
        "mlx-batch-runner"
    );
    assert_eq!(repo_name_from_cwd(None, &[]), "unknown");
    assert_eq!(repo_name_from_cwd(Some("/"), &[]), "unknown");
    assert_eq!(repo_name_from_cwd(Some(""), &[]), "unknown");

    // Single project filter
    assert_eq!(
        repo_name_from_cwd(
            Some("/Users/user/test-org/lbrx-services/subfolder"),
            &["lbrx".to_string()]
        ),
        "lbrx"
    );

    // Multiple project filters
    let filters = vec!["lbrx-services".to_string(), "foo".to_string()];
    assert_eq!(
        repo_name_from_cwd(
            Some("/Users/user/test-org/lbrx-services/subfolder"),
            &filters
        ),
        "lbrx-services"
    );

    // Multi-filter selection must use word-boundary path matching, NOT
    // raw substring. `--project test` against `/tmp/fastest-project`
    // would previously pick "test" as the label even though the project
    // filter step (correctly) rejects that path; the helper must agree.
    let filters = vec!["test".to_string(), "other".to_string()];
    assert_ne!(
        repo_name_from_cwd(Some("/tmp/fastest-project"), &filters),
        "test",
        "multi-filter label must not be picked via substring match"
    );
}

#[test]
fn test_decode_claude_project_path_with_leading_dash() {
    let encoded = "-Users-user-hosted-VetCoders-CodeScribe";
    let decoded = decode_claude_project_path(encoded);
    assert_eq!(decoded, "Users-user-hosted-VetCoders-CodeScribe");
}

#[test]
fn test_decode_claude_project_path_without_leading_dash() {
    let encoded = "Users-user-projects-foo";
    let decoded = decode_claude_project_path(encoded);
    assert_eq!(decoded, "Users-user-projects-foo");
}

#[test]
fn test_claude_dir_decode_preserves_hyphenated_repo() {
    let decoded = decode_claude_project_path("vista-portal-frontend");
    assert_eq!(decoded, "vista-portal-frontend");
    assert_ne!(decoded, "vista/portal/frontend");
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
    assert_eq!(decoded, "a-b-c-d-e-f");
}

// Replaces the previous `test_claude_filter_no_suffix_leak` which asserted
// the over-restrictive whole-string equality shape introduced by an
// earlier pass-4 cut. The reviewer bots (gemini HIGH + chatgpt-codex P1
// on PR #8) correctly flagged that the old shape broke every legit
// `-p reponame` invocation against an absolute-path Claude dir like
// `-Users-test-user-Git-aicx`. The new shape is a deliberate **soft
// prefilter** at the directory-listing stage paired with a strict
// per-entry `cwd` filter inside `extract_claude` — see the doc comment
// on `claude_project_dir_matches_filter` for the trade-off rationale.

#[test]
fn test_claude_dir_filter_accepts_repo_in_last_path_segment() {
    // Real-world common case: filter is a repo name; encoded dir is the
    // absolute cwd. The repo name appears as the last `-`-chunk of the
    // encoded form. All three should match.
    let aicx = vec!["aicx".to_string()];
    assert!(claude_project_dir_matches_filter(
        "-Users-test-user-Git-aicx",
        &aicx
    ));
    assert!(claude_project_dir_matches_filter("-aicx", &aicx));
    assert!(claude_project_dir_matches_filter("aicx", &aicx));
}

#[test]
fn test_claude_dir_filter_case_insensitive_on_repo_name() {
    let mixed = vec!["AiCx".to_string()];
    assert!(claude_project_dir_matches_filter(
        "-Users-test-user-Git-aicx",
        &mixed
    ));
}

#[test]
fn test_claude_dir_filter_rejects_non_last_segment_match() {
    // `vista` is NOT the last `-`-chunk of these encoded forms, so the
    // soft prefilter rejects them.
    let vista = vec!["vista".to_string()];
    assert!(!claude_project_dir_matches_filter("vista-portal", &vista));
    assert!(!claude_project_dir_matches_filter(
        "-Users-test-user-Git-vista-portal",
        &vista
    ));
    // The leading `vista-` substring is also not enough — the soft
    // prefilter wants `-vista` at the END (or exact match).
    assert!(!claude_project_dir_matches_filter("vista-app", &vista));
}

#[test]
fn test_claude_dir_filter_hyphenated_repo_matches_exactly() {
    // A hyphenated repo name `vista-portal` must match its own encoded
    // dir name. Three shapes again.
    let vista_portal = vec!["vista-portal".to_string()];
    assert!(claude_project_dir_matches_filter(
        "vista-portal",
        &vista_portal
    ));
    assert!(claude_project_dir_matches_filter(
        "-vista-portal",
        &vista_portal
    ));
    assert!(claude_project_dir_matches_filter(
        "-Users-test-user-Git-vista-portal",
        &vista_portal
    ));
}

#[test]
fn test_claude_dir_filter_owner_repo_form_cannot_decide_from_encoded_dir_alone() {
    // `owner/repo` filter is INHERENTLY undecidable from a Claude
    // encoded dir name alone — the encoding turned `/` into `-` losslessly
    // with no way to recover the owner/repo boundary. After the #15 fix
    // `decode_claude_project_path` no longer mangles hyphens into
    // slashes, so the soft prefilter cannot honestly say "yes this
    // encoded dir matches Loctree/aicx". It returns false here and lets
    // the strict per-entry `cwd` filter (which sees the unambiguous
    // original path) make the final call.
    let lo = vec!["Loctree/aicx".to_string()];
    assert!(
        !claude_project_dir_matches_filter("-Users-test-user-Git-Loctree-aicx", &lo),
        "owner/repo dir-name prefilter is intentionally pessimistic — \
         per-entry cwd resolves the strict case downstream"
    );

    // The corollary: an unrelated repo also doesn't match. (Symmetric.)
    assert!(!claude_project_dir_matches_filter(
        "-Users-test-user-Git-VetCoders-loct-io",
        &lo
    ));

    // A path-shaped CWD (not a Claude-encoded dir) DOES match via the
    // strict path matcher — this is the entry-level shape that runs
    // INSIDE extract_claude per-entry. Documented here so the contract
    // between the soft dir prefilter and the strict entry filter is
    // visible to readers of just the test module. (Function is private
    // to `super::` aka `crate::sources`; reachable via `use super::*`
    // at the top of this tests module.)
    assert!(
        project_filter_matches_path("/Users/user/Git/Loctree/aicx", &lo),
        "the strict matcher (used per-entry on real cwd values) DOES \
         match Loctree/aicx against a proper /-separated cwd"
    );
}

#[test]
fn test_body_mentions_repo_token_no_substring_leak() {
    // Regression guard for the codescribe project-hint inference site
    // (sources.rs ~4943). Token-equality semantics: `-p vista` must NOT
    // pick up a body mentioning `vista-portal`; `-p vista-portal` MUST.
    let body = "dyskusja o vista-portal i jakies inne tematy oraz osobny vista projekt"
        .to_ascii_lowercase();
    assert!(body_mentions_repo_token(&body, "vista"));
    assert!(body_mentions_repo_token(&body, "vista-portal"));

    let body_only_compound = "ta sesja dotyczy vista-portal".to_ascii_lowercase();
    assert!(!body_mentions_repo_token(&body_only_compound, "vista"));
    assert!(body_mentions_repo_token(
        &body_only_compound,
        "vista-portal"
    ));

    // Punctuation + URLs count as token separators.
    let body_url = "see https://example.com/vista for details".to_ascii_lowercase();
    assert!(body_mentions_repo_token(&body_url, "vista"));

    // Empty repo never matches.
    assert!(!body_mentions_repo_token(&body, ""));
}

#[test]
fn test_claude_dir_filter_inherent_ambiguity_is_a_soft_prefilter_concern() {
    // The Claude encoding is inherently lossy. `-Users-test-user-Git-nextra-docs-vista`
    // is ambiguously either a 6-segment path ending in `vista` (`-p vista`
    // match) or a 4-segment path ending in `nextra-docs-vista` (`-p vista`
    // should NOT match). The dir-name pre-filter cannot decide — the
    // strict per-entry `cwd` filter in `extract_claude` resolves it when
    // `cwd` is present. We document the trade-off by asserting that the
    // pre-filter LETS THIS THROUGH (the alternative is dropping legit
    // sessions whose downstream `cwd` would have matched).
    let vista = vec!["vista".to_string()];
    assert!(claude_project_dir_matches_filter(
        "-Users-test-user-Git-nextra-docs-vista",
        &vista
    ));
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
fn test_watermark_filter_drops_only_strictly_lt() {
    let root = unique_test_dir("watermark-strict-lt");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"type":"user","message":{"role":"user","content":"before watermark"},"timestamp":"2026-02-01T00:00:00Z","sessionId":"sess-watermark","cwd":"/tmp/aicx"}
{"type":"user","message":{"role":"user","content":"at watermark"},"timestamp":"2026-02-01T00:00:01Z","sessionId":"sess-watermark","cwd":"/tmp/aicx"}
{"type":"user","message":{"role":"user","content":"after watermark"},"timestamp":"2026-02-01T00:00:02Z","sessionId":"sess-watermark","cwd":"/tmp/aicx"}"#;
    write_file(&tmp, content);

    let watermark = Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 1).single().unwrap();
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: Some(watermark),
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    let messages: Vec<_> = entries.iter().map(|entry| entry.message.as_str()).collect();

    assert_eq!(messages, vec!["at watermark", "after watermark"]);
    assert!(entries.iter().all(|entry| entry.timestamp >= watermark));

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
fn test_extract_claude_file_ismeta_user_entry_becomes_system_note() {
    // A harness-injected user row (`isMeta: true` — e.g. a hook/system-reminder
    // injection) must be classified as SystemNote, not UserMsg, so it is dropped
    // by the conversation projection and excluded from the user-only/intent lane,
    // while still surviving the full report (include_assistant = true).
    let root = unique_test_dir("claude-ismeta-system-note");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"type":"user","message":{"role":"user","content":"real operator question"},"timestamp":"2026-04-14T10:00:00Z","sessionId":"sess-meta","gitBranch":"main","cwd":"/tmp/aicx"}
{"type":"user","isMeta":true,"message":{"role":"user","content":"<system-reminder>injected hook context</system-reminder>"},"timestamp":"2026-04-14T10:00:01Z","sessionId":"sess-meta","gitBranch":"main","cwd":"/tmp/aicx"}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    assert_eq!(
        frame_kinds(&entries),
        vec![Some(FrameKind::UserMsg), Some(FrameKind::SystemNote)]
    );
    assert_eq!(entries[0].message, "real operator question");
    assert!(entries[1].message.contains("injected hook context"));

    // user-only extraction (include_assistant = false) must drop the meta row.
    let user_only = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: false,
        watermark: None,
    };
    let user_entries = extract_claude_file(&tmp, &user_only).unwrap();
    assert_eq!(frame_kinds(&user_entries), vec![Some(FrameKind::UserMsg)]);
    assert_eq!(user_entries[0].message, "real operator question");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_claude_file_drops_signature_only_thinking_killer_case() {
    let root = unique_test_dir("claude-empty-thinking-signature");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"type":"user","message":{"role":"user","content":"Hello"},"timestamp":"2026-04-14T10:00:00Z","sessionId":"sess-signature","gitBranch":"main","cwd":"/tmp/aicx"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"abc123"},{"type":"text","text":"Visible answer"}]},"timestamp":"2026-04-14T10:00:01Z","sessionId":"sess-signature","gitBranch":"main","cwd":"/tmp/aicx"}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        frame_kinds(&entries),
        vec![Some(FrameKind::UserMsg), Some(FrameKind::AgentReply)]
    );
    assert!(
        entries
            .iter()
            .all(|entry| !entry.message.contains("signature"))
    );
    assert!(
        entries
            .iter()
            .all(|entry| !entry.message.contains("abc123"))
    );
    assert_eq!(entries[1].message, "Visible answer");

    let conversation = to_conversation(&entries, &[]);
    assert_eq!(conversation.len(), 2);
    assert!(
        conversation
            .iter()
            .all(|entry| !entry.message.contains("signature"))
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_claude_file_keeps_visible_thinking_text_without_signature() {
    let root = unique_test_dir("claude-thinking-text-signature");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"Useful hidden note","signature":"abc123"}]},"timestamp":"2026-04-14T10:00:01Z","sessionId":"sess-signature","gitBranch":"main","cwd":"/tmp/aicx"}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].frame_kind, Some(FrameKind::InternalThought));
    assert_eq!(entries[0].message, "Useful hidden note");
    assert!(!entries[0].message.contains("signature"));
    assert!(!entries[0].message.contains("abc123"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_claude_file_drops_signature_only_thinking_block() {
    let root = unique_test_dir("claude-signature-thinking");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Visible assistant reply"},{"type":"thinking","thinking":"","signature":"abc123"}]},"timestamp":"2026-04-14T10:00:01Z","sessionId":"claude-signature","gitBranch":"main","cwd":"/tmp/aicx"}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].frame_kind, Some(FrameKind::AgentReply));
    assert_eq!(entries[0].message, "Visible assistant reply");
    assert!(
        !entries
            .iter()
            .any(|entry| entry.message.contains("signature"))
    );
    assert!(!entries.iter().any(|entry| entry.message.contains("abc123")));

    let conversation = to_conversation(&entries, &[]);
    assert_eq!(conversation.len(), 1);
    assert_eq!(conversation[0].message, "Visible assistant reply");
    assert!(!conversation[0].message.contains("signature"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_claude_file_drops_empty_thinking_signature_block() {
    let root = unique_test_dir("claude-empty-signature-thinking");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Visible assistant reply"},{"type":"thinking","thinking":"","signature":"abc123"}]},"timestamp":"2026-04-14T10:00:01Z","sessionId":"claude-signature-regression","gitBranch":"main","cwd":"/Users/tester/workspaces/VetCoders/ai-contexters"}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].frame_kind, Some(FrameKind::AgentReply));
    assert_eq!(entries[0].message, "Visible assistant reply");
    assert!(
        entries
            .iter()
            .all(|entry| !entry.message.contains("signature")
                && !entry.message.contains("abc123")
                && !entry.message.contains("\"type\":\"thinking\"")),
        "Claude empty thinking signature block leaked into entries: {entries:#?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_claude_conversation_mode_stays_signature_clean() {
    let root = unique_test_dir("claude-conversation-signature-clean");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"type":"user","message":{"role":"user","content":"Hello"},"timestamp":"2026-04-14T10:00:00Z","sessionId":"claude-conversation-clean","cwd":"/Users/tester/workspaces/VetCoders/ai-contexters"}
{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"Visible assistant reply"},{"type":"thinking","thinking":"","signature":"abc123"}]},"timestamp":"2026-04-14T10:00:01Z","sessionId":"claude-conversation-clean","cwd":"/Users/tester/workspaces/VetCoders/ai-contexters"}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_claude_file(&tmp, &config).unwrap();
    let conversation = to_conversation(&entries, &[]);
    assert_eq!(conversation.len(), 2);
    assert!(
        conversation
            .iter()
            .all(|entry| !entry.message.contains("signature") && !entry.message.contains("abc123")),
        "conversation projection leaked signature: {conversation:#?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_codex_file_preserves_signature_word() {
    let root = unique_test_dir("codex-signature-preserved");
    let tmp = root.join("history.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"session_id":"s1","text":"The API signature changed intentionally.","ts":1000,"role":"assistant","cwd":"/tmp/a"}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_codex_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].message,
        "The API signature changed intentionally."
    );

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
fn test_extract_codex_file_classifies_developer_response_item_as_system_note() {
    let root = unique_test_dir("codex-developer-response-item");
    let tmp = root.join("rollout.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"timestamp":"2026-06-05T10:00:00Z","type":"session_meta","payload":{"id":"codex-developer-role","cwd":"/tmp/aicx"}}
{"timestamp":"2026-06-05T10:00:01Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"real user request"}]}}
{"timestamp":"2026-06-05T10:00:02Z","type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"injected developer instruction"}]}}"#;
    write_file(&tmp, content);

    let entries = extract_codex_file(&tmp, &default_config(true)).unwrap();
    assert_eq!(
        frame_kinds(&entries),
        vec![Some(FrameKind::UserMsg), Some(FrameKind::SystemNote)]
    );
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].message, "real user request");
    assert_eq!(entries[1].role, "system");
    assert_eq!(entries[1].message, "injected developer instruction");

    let user_only_entries = extract_codex_file(&tmp, &default_config(false)).unwrap();
    assert_eq!(
        frame_kinds(&user_only_entries),
        vec![Some(FrameKind::UserMsg)]
    );
    assert_eq!(user_only_entries[0].message, "real user request");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_missing_meta_warns_and_falls_back_to_stem() {
    let root = unique_test_dir("codex-missing-session-meta");
    let tmp = root.join("rollout-without-meta.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"hello without meta"}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, "rollout-without-meta");
    assert_eq!(
        warnings,
        vec![CodexSessionWarning::MissingSessionMeta {
            fallback: "rollout-without-meta".to_string()
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_duplicate_meta_warns_first_wins() {
    let root = unique_test_dir("codex-duplicate-session-meta");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"session-a","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"session-b","cwd":"/tmp/b"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"hello duplicate meta"}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, "session-a");
    assert_eq!(
        warnings,
        vec![CodexSessionWarning::DuplicateSessionMeta {
            first: "session-a".to_string(),
            ignored: vec!["session-b".to_string()],
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_filename_mismatch_warns() {
    let root = unique_test_dir("codex-filename-mismatch");
    let filename_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let meta_id = "019e2574-8a7f-7d33-a318-b365aa0ab970";
    let stem = format!("rollout-2026-05-14T00-47-35-{filename_id}");
    let tmp = root.join(format!("{stem}.jsonl"));
    let _ = fs::remove_dir_all(&root);

    let content = format!(
        r#"{{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{{"id":"{meta_id}","cwd":"/tmp/a"}}}}
{{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{{"type":"user_message","message":"hello mismatch"}}}}"#
    );
    write_file(&tmp, &content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, meta_id);
    assert_eq!(
        warnings,
        vec![CodexSessionWarning::FilenameMismatch {
            meta_id: meta_id.to_string(),
            filename_stem: stem,
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_non_rollout_filename_does_not_mismatch_warn() {
    let root = unique_test_dir("codex-non-rollout-no-mismatch");
    let meta_id = "019e2574-8a7f-7d33-a318-b365aa0ab970";
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = format!(
        r#"{{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{{"id":"{meta_id}","cwd":"/tmp/a"}}}}
{{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{{"type":"user_message","message":"hello direct file"}}}}"#
    );
    write_file(&tmp, &content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, meta_id);
    assert!(warnings.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_codex_session_diagnostics_aggregates_counts() {
    let mut diagnostics = CodexSessionDiagnostics::default();
    assert!(diagnostics.is_empty());

    diagnostics.observe(&[CodexSessionWarning::MissingSessionMeta {
        fallback: "rollout-without-meta".to_string(),
    }]);
    diagnostics.observe(&[
        CodexSessionWarning::DuplicateSessionMeta {
            first: "session-a".to_string(),
            ignored: vec!["session-b".to_string()],
        },
        CodexSessionWarning::FilenameMismatch {
            meta_id: "019e2574-8a7f-7d33-a318-b365aa0ab970".to_string(),
            filename_stem: "rollout-2026-05-14T00-47-35-aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"
                .to_string(),
        },
    ]);
    diagnostics.observe(&[CodexSessionWarning::UnparsableTimestamp {
        count: 3,
        samples: vec![
            "2026-02-01T00:00:01".to_string(),
            "garbage".to_string(),
            "x".to_string(),
        ],
    }]);
    diagnostics.observe(&[CodexSessionWarning::UnknownMsgType {
        count: 4,
        samples: vec![
            "task_started".to_string(),
            "task_complete".to_string(),
            "error".to_string(),
        ],
    }]);

    assert!(!diagnostics.is_empty());
    assert_eq!(diagnostics.missing, 1);
    assert_eq!(diagnostics.duplicate, 1);
    assert_eq!(diagnostics.mismatch, 1);
    assert_eq!(diagnostics.unparsable_ts, 3);
    assert_eq!(diagnostics.unknown_msg_type, 4);

    diagnostics.observe(&[CodexSessionWarning::LineParseError {
        line_number: 7,
        snippet: "{\"broken\":".to_string(),
    }]);
    assert_eq!(diagnostics.line_parse_error, 1);
}

#[test]
fn test_codex_history_silent_skip_emits_warning() {
    let root = unique_test_dir("codex-history-line-parse-warning");
    let tmp = root.join("history.jsonl");
    let _ = fs::remove_dir_all(&root);

    let valid = r#"{"session_id":"history-a","text":"hello history","ts":1770000000,"role":"user","cwd":"/tmp/aicx"}"#;
    let malformed = format!(r#"{{"session_id":"broken","text":"{}""#, "x".repeat(240));
    let content = format!("{valid}\n{malformed}\n");
    write_file(&tmp, &content);

    let (entries, warnings) =
        parse_codex_file_with_diagnostics(&tmp, "codex", &default_config(true)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, "hello history");

    let warning = warnings
        .iter()
        .find_map(|warning| match warning {
            CodexSessionWarning::LineParseError {
                line_number,
                snippet,
            } => Some((*line_number, snippet)),
            _ => None,
        })
        .expect("malformed Codex history JSONL line should warn");
    assert_eq!(warning.0, 2);
    assert_eq!(warning.1.chars().count(), 200);
    assert!(malformed.starts_with(warning.1));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_codex_session_event_silent_skip_emits_warning() {
    let root = unique_test_dir("codex-session-line-parse-warning");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"session-a","cwd":"/tmp/a"}}
{"timestamp":
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"hello session"}}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &default_config(true)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message, "hello session");
    assert!(warnings.iter().any(|warning| {
        matches!(
            warning,
            CodexSessionWarning::LineParseError {
                line_number: 2,
                snippet,
            } if snippet.trim_end() == "{\"timestamp\":"
        )
    }));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_duplicate_and_mismatch_warn_together() {
    let root = unique_test_dir("codex-duplicate-and-mismatch");
    let filename_id = "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee";
    let meta_id = "019e2574-8a7f-7d33-a318-b365aa0ab970";
    let ignored_id = "119e2574-8a7f-7d33-a318-b365aa0ab970";
    let stem = format!("rollout-2026-05-14T00-47-35-{filename_id}");
    let tmp = root.join(format!("{stem}.jsonl"));
    let _ = fs::remove_dir_all(&root);

    let content = format!(
        r#"{{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{{"id":"{meta_id}","cwd":"/tmp/a"}}}}
{{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{{"id":"{ignored_id}","cwd":"/tmp/b"}}}}
{{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{{"type":"user_message","message":"hello compound"}}}}"#
    );
    write_file(&tmp, &content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &config).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].session_id, meta_id);
    assert_eq!(
        warnings,
        vec![
            CodexSessionWarning::DuplicateSessionMeta {
                first: meta_id.to_string(),
                ignored: vec![ignored_id.to_string()],
            },
            CodexSessionWarning::FilenameMismatch {
                meta_id: meta_id.to_string(),
                filename_stem: stem,
            },
        ]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_unparsable_timestamps_warn_and_drop() {
    let root = unique_test_dir("codex-unparsable-ts");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"sess","cwd":"/tmp"}}
{"timestamp":"2026-02-01T00:00:01","type":"event_msg","payload":{"type":"user_message","message":"naive ts dropped"}}
{"timestamp":"not-a-timestamp","type":"event_msg","payload":{"type":"user_message","message":"garbage ts dropped"}}
{"timestamp":"2026/02/01T00:00:03Z","type":"event_msg","payload":{"type":"user_message","message":"slash separator dropped"}}
{"timestamp":"not-a-timestamp","type":"event_msg","payload":{"type":"user_message","message":"duplicate sample dropped"}}
{"timestamp":"2026-02-01T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"good ts kept"}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &config).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].message, "naive ts dropped");
    assert_eq!(
        entries[0].timestamp,
        Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 1).unwrap()
    );
    assert_eq!(entries[1].message, "good ts kept");

    assert_eq!(warnings.len(), 1);
    match &warnings[0] {
        CodexSessionWarning::UnparsableTimestamp { count, samples } => {
            assert_eq!(*count, 3);
            assert_eq!(
                samples,
                &vec![
                    "not-a-timestamp".to_string(),
                    "2026/02/01T00:00:03Z".to_string(),
                ]
            );
        }
        other => panic!("expected UnparsableTimestamp, got {other:?}"),
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_mcp_tool_call_is_kept_in_timeline() {
    let root = unique_test_dir("codex-mcp-tool-call");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"sess","cwd":"/tmp"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"mcp_tool_call","server":"rust-memex","tool":"memory_search"}}
{"timestamp":"2026-02-01T00:00:02Z","type":"event_msg","payload":{"type":"mcp_tool_call_response","server":"rust-memex","result":"ok"}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &config).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.role == "tool"));
    assert!(
        entries
            .iter()
            .all(|e| e.frame_kind == Some(FrameKind::ToolCall))
    );
    assert!(
        warnings.is_empty(),
        "expected no warnings, got {warnings:?}"
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_unknown_msg_type_warns_and_counts() {
    let root = unique_test_dir("codex-unknown-msg-type");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"sess","cwd":"/tmp"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"good user"}}
{"timestamp":"2026-02-01T00:00:02Z","type":"event_msg","payload":{"type":"task_started"}}
{"timestamp":"2026-02-01T00:00:03Z","type":"event_msg","payload":{"type":"web_search","query":"x"}}
{"timestamp":"2026-02-01T00:00:04Z","type":"event_msg","payload":{"type":"task_started"}}
{"timestamp":"2026-02-01T00:00:05Z","type":"event_msg","payload":{}}
{"timestamp":"2026-02-01T00:00:06Z","type":"event_msg","payload":{"type":""}}
{"timestamp":"2026-02-01T00:00:07Z","type":"event_msg","payload":{"type":"agent_message","message":"good assistant"}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &config).unwrap();
    assert_eq!(entries.len(), 7);
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[1].role, "system");
    assert_eq!(entries[1].frame_kind, Some(FrameKind::SystemNote));
    assert_eq!(entries[2].role, "system");
    assert_eq!(entries[2].message, "x");
    assert_eq!(entries[6].role, "assistant");

    assert_eq!(warnings.len(), 1);
    match &warnings[0] {
        CodexSessionWarning::UnknownMsgType { count, samples } => {
            assert_eq!(*count, 2);
            assert_eq!(samples, &vec!["<missing>".to_string()]);
        }
        other => panic!("expected UnknownMsgType, got {other:?}"),
    }

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
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].agent, "gemini");
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[1].role, "assistant");
    assert_eq!(entries[2].role, "system");
    assert_eq!(entries[2].frame_kind, Some(FrameKind::SystemNote));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_file_session_jsonl_uses_metadata_session_id() {
    let root = unique_test_dir("gemini-jsonl-direct");
    let tmp = root.join("session-2026-05-13T02-16-c6c4ada0.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r##"{"sessionId":"c6c4ada0-5d27-4e74-8194-be01b46bf865","projectHash":"hash-1","startTime":"2026-05-13T02:16:02.852Z","lastUpdated":"2026-05-13T02:16:02.852Z","kind":"main"}
{"id":"u1","timestamp":"2026-05-13T02:16:04.460Z","type":"user","content":[{"text":"# Research Task"},{"text":"\nMap the lanes"}]}
{"$set":{"lastUpdated":"2026-05-13T02:16:04.460Z"}}
{"id":"a1","timestamp":"2026-05-13T02:16:13.173Z","type":"gemini","content":"Report written","thoughts":[{"subject":"Planning","description":"Reading files","timestamp":"2026-05-13T02:16:14.000Z"}]}"##;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 3);
    assert!(
        entries
            .iter()
            .all(|entry| entry.session_id == "c6c4ada0-5d27-4e74-8194-be01b46bf865")
    );
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].message, "# Research Task\nMap the lanes");
    assert_eq!(entries[1].role, "assistant");
    assert_eq!(entries[1].message, "Report written");
    assert_eq!(entries[2].role, "reasoning");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_gemini_file_prefers_session_path_project_over_content_hints() {
    let root = unique_test_dir("gemini-session-path-project");
    let tmp = root
        .join(".gemini")
        .join("tmp")
        .join("vista-portal")
        .join("chats")
        .join("session-2026-05-17T11-29-6d5b2959.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r##"{"sessionId":"6d5b2959-c56b-4c90-b198-41eb2ce399da","projectHash":"atomic-orbitals-b716c2b71310439897d3f81602f6c799","startTime":"2026-05-17T11:29:00.000Z","kind":"main"}
{"id":"u1","timestamp":"2026-05-17T11:29:01.000Z","type":"user","content":[{"cwd":"/Users/user/Desktop/screenshot/Screenshot","text":"Review this screenshot for Vista Portal."}]}
{"id":"a1","timestamp":"2026-05-17T11:29:02.000Z","type":"gemini","content":"The screenshot review belongs to the Vista Portal session."}"##;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_gemini_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(
        entries
            .iter()
            .all(|entry| entry.cwd.as_deref() == Some("vista-portal"))
    );
    assert_eq!(
        repo_labels_from_entries(&entries, &[]),
        vec!["vista-portal"]
    );

    let screenshot_filter = ExtractionConfig {
        project_filter: vec!["screenshot".to_string()],
        ..config
    };
    let filtered_entries = extract_gemini_file(&tmp, &screenshot_filter).unwrap();
    assert!(
        filtered_entries.is_empty(),
        "session path ownership must not match screenshot-only content hints"
    );

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
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"step-term-1","status":"COMPLETED","command":"rg foo","output":"matched line"}}}"#;
    write_file(&tmp, content);

    let cutoff = Utc.timestamp_opt(0, 0).single().unwrap();
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff,
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_junie_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 5);
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
    assert_eq!(entries[4].role, "tool");
    assert_eq!(entries[4].frame_kind, Some(FrameKind::ToolCall));
    assert_eq!(entries[4].message, "$ rg foo\nmatched line");
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
fn test_extract_junie_file_captures_thoughts_and_tool_chain() {
    let root = unique_test_dir("junie-rich");
    let session_dir = root.join("session-260519-164145-rich");
    let tmp = session_dir.join("events.jsonl");
    let _ = fs::remove_dir_all(&root);

    // Stream snapshots in IN_PROGRESS -> COMPLETED order; dedup must collapse
    // the two identical COMPLETED frames for the same stepId.
    let content = r#"{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"CurrentDirectoryUpdatedEvent","currentDirectory":"/work/repo"}}}
{"kind":"UserPromptEvent","requestId":"prompt-260519-164200-aaaa","prompt":"map the repo","presentablePrompt":"map the repo"}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"AgentThoughtBlockUpdatedEvent","stepId":"th-1","text":"I should run loctree context first."}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"McpBlockUpdatedEvent","stepId":"mcp-1","toolName":"loctree-mcp/context","input":"{\"project\":\"/work/repo\"}","status":"IN_PROGRESS","details":""}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"McpBlockUpdatedEvent","stepId":"mcp-1","toolName":"loctree-mcp/context","input":"{\"project\":\"/work/repo\"}","status":"COMPLETED","details":"atlas v1"}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ToolBlockUpdatedEvent","stepId":"tool-1","status":"COMPLETED","text":"Found \"docs/plans/**\"","details":"docs/plans/PLAN_22.md\n"}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ViewFilesBlockUpdatedEvent","stepId":"view-1","status":"COMPLETED","files":[{"relativePath":"docs/plans/PLAN_22.md","lineFrom":1,"lineTo":50}]}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"FileChangesBlockUpdatedEvent","stepId":"chg-1","status":"COMPLETED","changes":[{"relativePath":"src/lib.rs"}]}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"term-1","status":"IN_PROGRESS","command":"cargo build","output":""}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"term-1","status":"COMPLETED","command":"cargo build","output":"Compiling aicx"}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"TerminalBlockUpdatedEvent","stepId":"term-1","status":"COMPLETED","command":"cargo build","output":"Compiling aicx"}}}
{"kind":"SessionA2uxEvent","event":{"state":"IN_PROGRESS","agentEvent":{"kind":"ResultBlockUpdatedEvent","stepId":"res-1","cancelled":false,"result":"Mapped.","errorCode":"Submit"}}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_junie_file(&tmp, &config).unwrap();
    let kinds: Vec<_> = entries
        .iter()
        .map(|e| (e.role.as_str(), e.frame_kind))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("user", Some(FrameKind::UserMsg)),
            ("reasoning", Some(FrameKind::InternalThought)),
            ("tool", Some(FrameKind::ToolCall)),
            ("tool", Some(FrameKind::ToolCall)),
            ("tool", Some(FrameKind::ToolCall)),
            ("tool", Some(FrameKind::ToolCall)),
            ("tool", Some(FrameKind::ToolCall)),
            ("assistant", Some(FrameKind::AgentReply)),
        ]
    );
    assert_eq!(entries[0].message, "map the repo");
    assert_eq!(entries[1].message, "I should run loctree context first.");
    assert!(entries[2].message.starts_with("loctree-mcp/context: "));
    assert!(entries[2].message.contains("atlas v1"));
    assert!(entries[3].message.starts_with("Found"));
    assert!(entries[3].message.contains("PLAN_22.md"));
    assert_eq!(entries[4].message, "viewed: docs/plans/PLAN_22.md:1-50");
    assert_eq!(entries[5].message, "edited: src/lib.rs");
    assert_eq!(entries[6].message, "$ cargo build\nCompiling aicx");
    assert_eq!(entries[7].message, "Mapped.");
    assert!(
        entries
            .iter()
            .all(|entry| entry.cwd.as_deref() == Some("/work/repo"))
    );
    assert!(
        entries
            .windows(2)
            .all(|pair| pair[0].timestamp < pair[1].timestamp)
    );

    // user-only mode strips everything but UserMsg
    let user_only = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: false,
        watermark: None,
    };
    let user_entries = extract_junie_file(&tmp, &user_only).unwrap();
    assert_eq!(user_entries.len(), 1);
    assert_eq!(user_entries[0].role, "user");

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
fn test_extract_junie_file_plan_attachment_prompt_becomes_system_note() {
    let root = unique_test_dir("junie-plan-attachment-system-note");
    let session_dir = root.join("session-260605-183024-junie");
    let tmp = session_dir.join("events.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"kind":"UserPromptEvent","requestId":"prompt-260605-183100-real1","prompt":"real operator question","presentablePrompt":"real operator question"}
{"kind":"UserPromptEvent","requestId":"prompt-260605-183101-meta1","prompt":"Implement the suggested plan","presentablePrompt":"Implement the suggested plan","customAttachments":[{"kind":"PlanAttachment","plan":{"sections":[{"name":"Requirements","content":"Injected harness plan"}]}}]}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_junie_file(&tmp, &config).unwrap();
    assert_eq!(
        frame_kinds(&entries),
        vec![Some(FrameKind::UserMsg), Some(FrameKind::SystemNote)]
    );
    assert_eq!(entries[0].role, "user");
    assert_eq!(entries[0].message, "real operator question");
    assert_eq!(entries[1].role, "system");
    assert_eq!(entries[1].message, "Implement the suggested plan");

    let user_only = ExtractionConfig {
        project_filter: vec![],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: false,
        watermark: None,
    };
    let user_entries = extract_junie_file(&tmp, &user_only).unwrap();
    assert_eq!(frame_kinds(&user_entries), vec![Some(FrameKind::UserMsg)]);
    assert_eq!(user_entries[0].message, "real operator question");

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
    assert!(entries[0].message.contains("repoalpha"));
    assert!(
        entries[0]
            .message
            .contains(&canonical_display(&conversation_artifact))
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
    assert!(entries[0].message.contains(&canonical_display(&pb)));
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
    assert!(entries[1].message.contains(&canonical_display(&step_a)));
    assert!(entries[2].message.contains(&canonical_display(&step_b)));
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
            Some("/Users/user/hosted/VetCoders/CodeScribe".to_string()),
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
            timestamp_source: None,
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
            timestamp_source: None,
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
            timestamp_source: None,
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
fn test_gemini_message_error_info_map_to_system_note() {
    // "error" and "info" types are preserved as system notes.
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
            "error" | "info" => Some("system"),
            _ => None,
        };
        assert_eq!(role, Some("system"));
        assert_eq!(
            role.and_then(frame_kind_from_role),
            Some(FrameKind::SystemNote)
        );
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

#[test]
fn test_parse_rfc3339_or_naive_utc_accepts_z() {
    let ts = parse_rfc3339_or_naive_utc("2026-01-01T00:00:00Z").unwrap();
    assert_eq!(ts, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
}

#[test]
fn test_parse_rfc3339_or_naive_utc_accepts_offset() {
    let ts = parse_rfc3339_or_naive_utc("2026-01-01T01:00:00+01:00").unwrap();
    assert_eq!(ts, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
}

#[test]
fn test_parse_rfc3339_or_naive_utc_accepts_fractional_z() {
    let ts = parse_rfc3339_or_naive_utc("2026-01-01T00:00:00.123Z").unwrap();
    assert_eq!(ts.timestamp_subsec_millis(), 123);
}

#[test]
fn test_parse_rfc3339_or_naive_utc_accepts_naive_as_utc() {
    let ts = parse_rfc3339_or_naive_utc("2026-01-01T00:00:00").unwrap();
    assert_eq!(ts, Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap());
}

#[test]
fn test_parse_rfc3339_or_naive_utc_rejects_garbage() {
    assert!(parse_rfc3339_or_naive_utc("not-a-timestamp").is_err());
}

// `read_line_limited` was removed in favor of
// `aicx_parser::sanitize::read_line_capped`, which adds UTF-8 boundary
// walk-back so an oversized line whose cut lands inside a multi-byte
// codepoint doesn't surface as `InvalidData`. The two smoke tests
// below were carried over verbatim against the new helper so the
// migration is locked in. Full UTF-8 boundary coverage lives in
// `crates/aicx-parser/tests/sanitize_caps.rs`.
#[test]
fn test_read_line_capped_returns_normal_line() {
    let mut reader = Cursor::new(b"{\"ok\":true}\nnext\n".to_vec());
    let line = sanitize::read_line_capped(&mut reader, 32)
        .unwrap()
        .unwrap();
    assert!(!line.exceeded);
    assert_eq!(line.line, "{\"ok\":true}\n");
}

#[test]
fn test_read_line_capped_skips_to_next_line_after_oversized() {
    let mut reader = Cursor::new(b"aaaaaaaaa\nok\n".to_vec());
    let first = sanitize::read_line_capped(&mut reader, 4).unwrap().unwrap();
    assert!(first.exceeded);
    assert_eq!(first.line, "aaaa");
    let second = sanitize::read_line_capped(&mut reader, 4).unwrap().unwrap();
    assert_eq!(second.line, "ok\n");
}

#[test]
fn test_parse_claude_jsonl_first_non_empty_session_id_wins() {
    let root = unique_test_dir("claude-session-first-wins");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"type":"user","message":{"role":"user","content":"first"},"timestamp":"2026-02-01T00:00:00Z","sessionId":""}
{"type":"user","message":{"role":"user","content":"second"},"timestamp":"2026-02-01T00:00:01Z","sessionId":"session-a"}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_claude_jsonl_with_diagnostics(&tmp, "fallback", &default_config(true)).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.session_id == "session-a"));
    assert!(warnings.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_claude_jsonl_session_id_drift_warns() {
    let root = unique_test_dir("claude-session-drift");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"type":"user","message":{"role":"user","content":"first"},"timestamp":"2026-02-01T00:00:00Z","sessionId":"session-a"}
{"type":"assistant","message":{"role":"assistant","content":"second"},"timestamp":"2026-02-01T00:00:01Z","sessionId":"session-b"}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_claude_jsonl_with_diagnostics(&tmp, "fallback", &default_config(true)).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|entry| entry.session_id == "session-a"));
    assert_eq!(
        warnings,
        vec![ClaudeSessionWarning::SessionIdDrift {
            first: "session-a".to_string(),
            ignored: vec!["session-b".to_string()],
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_claude_jsonl_missing_session_id_warns_fallback() {
    let root = unique_test_dir("claude-session-missing");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"type":"user","message":{"role":"user","content":"first"},"timestamp":"2026-02-01T00:00:00Z"}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_claude_jsonl_with_diagnostics(&tmp, "fallback-id", &default_config(true)).unwrap();
    assert_eq!(entries[0].session_id, "fallback-id");
    assert_eq!(
        warnings,
        vec![ClaudeSessionWarning::MissingSessionId {
            fallback: "fallback-id".to_string(),
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_claude_jsonl_naive_timestamp_is_kept() {
    let root = unique_test_dir("claude-naive-ts");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"type":"user","message":{"role":"user","content":"naive"},"timestamp":"2026-02-01T00:00:00","sessionId":"session-a"}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_claude_jsonl_with_diagnostics(&tmp, "fallback", &default_config(true)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].timestamp,
        Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()
    );
    assert!(warnings.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_claude_jsonl_invalid_timestamp_warns() {
    let root = unique_test_dir("claude-invalid-ts");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"type":"user","message":{"role":"user","content":"bad"},"timestamp":"bad-ts","sessionId":"session-a"}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_claude_jsonl_with_diagnostics(&tmp, "fallback", &default_config(true)).unwrap();
    assert!(entries.is_empty());
    assert_eq!(
        warnings,
        vec![ClaudeSessionWarning::UnparsableTimestamp {
            count: 1,
            samples: vec!["bad-ts".to_string()],
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_claude_jsonl_preserves_missing_timestamp_with_fallback_metadata() {
    let root = unique_test_dir("claude-missing-ts-fallback");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"type":"user","message":{"role":"user","content":"first with timestamp"},"timestamp":"2026-02-01T00:00:00Z","sessionId":"session-a","cwd":"/tmp/aicx"}
{"type":"user","message":{"role":"user","content":"missing timestamp preserved"},"sessionId":"session-a","cwd":"/tmp/aicx"}
{"type":"assistant","message":{"role":"assistant","content":"assistant with timestamp"},"timestamp":"2026-02-01T00:00:01Z","sessionId":"session-a","cwd":"/tmp/aicx"}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_claude_jsonl_with_diagnostics(&tmp, "fallback", &default_config(true)).unwrap();

    assert_eq!(entries.len(), 3);
    assert_eq!(entries[0].message, "first with timestamp");
    assert_eq!(entries[1].message, "missing timestamp preserved");
    assert_eq!(entries[2].message, "assistant with timestamp");
    assert_eq!(entries[1].timestamp, entries[0].timestamp);
    assert_eq!(
        entries[1].timestamp_source.as_deref(),
        Some("fallback_previous")
    );
    assert!(entries[0].timestamp_source.is_none());
    assert!(entries[2].timestamp_source.is_none());

    assert_eq!(warnings.len(), 1);
    match &warnings[0] {
        ClaudeSessionWarning::FallbackTimestamp { count, samples } => {
            assert_eq!(count, &1);
            assert_eq!(
                samples,
                &vec!["line 2: <missing> -> fallback_previous".to_string()]
            );
            let rendered = warnings[0].describe(&tmp);
            assert!(rendered.contains("1 frames preserved with fallback timestamp"));
            assert!(rendered.contains("sample lines: line 2: <missing> -> fallback_previous"));
            assert!(!rendered.contains("frames dropped"));
        }
        other => panic!("expected FallbackTimestamp, got {other:?}"),
    }

    let chunks = chunk_entries(&entries, "aicx", "claude", &ChunkerConfig::default());
    let fallback_chunk = chunks
        .iter()
        .find(|chunk| chunk.text.contains("missing timestamp preserved"))
        .expect("fallback body should be chunked");
    let sidecar = ChunkMetadataSidecar::from(fallback_chunk);
    assert_eq!(
        sidecar.timestamp_source.as_deref(),
        Some("fallback_previous")
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_codex_session_meta_empty_then_valid_wins() {
    let root = unique_test_dir("codex-empty-valid-meta");
    let tmp = root.join("rollout.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"session_meta","payload":{"id":"session-a","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:02Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &default_config(true)).unwrap();
    assert_eq!(entries[0].session_id, "session-a");
    assert!(warnings.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_codex_session_meta_empty_then_valid_duplicate_warns() {
    let root = unique_test_dir("codex-empty-valid-duplicate");
    let tmp = root.join("rollout.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"session_meta","payload":{"id":"session-a","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:02Z","type":"session_meta","payload":{"id":"session-b","cwd":"/tmp/b"}}
{"timestamp":"2026-02-01T00:00:03Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &default_config(true)).unwrap();
    assert_eq!(entries[0].session_id, "session-a");
    assert_eq!(
        warnings,
        vec![CodexSessionWarning::DuplicateSessionMeta {
            first: "session-a".to_string(),
            ignored: vec!["session-b".to_string()],
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_codex_session_meta_empty_only_falls_back() {
    let root = unique_test_dir("codex-empty-only-meta");
    let tmp = root.join("rollout-empty-only.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &default_config(true)).unwrap();
    assert_eq!(entries[0].session_id, "rollout-empty-only");
    assert_eq!(
        warnings,
        vec![CodexSessionWarning::MissingSessionMeta {
            fallback: "rollout-empty-only".to_string(),
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_role_only_tool_frame_is_preserved() {
    let root = unique_test_dir("codex-role-tool");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"session-a","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"message","role":"tool","message":"tool output"}}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &default_config(true)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "tool");
    assert_eq!(entries[0].frame_kind, Some(FrameKind::ToolCall));
    assert_eq!(entries[0].message, "tool output");
    assert_eq!(warnings.len(), 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_parse_codex_session_unknown_payload_preserved_as_system_note() {
    let root = unique_test_dir("codex-unknown-preserved");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"session-a","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"surprise","content":{"nested":true}}}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_codex_session_file_with_diagnostics(&tmp, "codex", &default_config(true)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "system");
    assert_eq!(entries[0].frame_kind, Some(FrameKind::SystemNote));
    assert!(entries[0].message.contains("\"nested\":true"));
    assert_eq!(warnings.len(), 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_codex_file_mixed_history_first_warns_and_recovers_both() {
    let root = unique_test_dir("codex-mixed-history-first");
    let tmp = root.join("mixed.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"session_id":"history-a","text":"history message","ts":1000,"role":"user","cwd":"/tmp/a"}
{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"session-a","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"session message"}}"#;
    write_file(&tmp, content);

    let entries = extract_codex_file(&tmp, &default_config(true)).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry.session_id == "history-a"));
    assert!(entries.iter().any(|entry| entry.session_id == "session-a"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_codex_file_mixed_session_first_warns_and_recovers_both() {
    let root = unique_test_dir("codex-mixed-session-first");
    let tmp = root.join("mixed.jsonl");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"session-a","cwd":"/tmp/a"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"session message"}}
{"session_id":"history-a","text":"history message","ts":1000,"role":"user","cwd":"/tmp/a"}"#;
    write_file(&tmp, content);

    let entries = extract_codex_file(&tmp, &default_config(true)).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().any(|entry| entry.session_id == "history-a"));
    assert!(entries.iter().any(|entry| entry.session_id == "session-a"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_gemini_jsonl_session_id_drift_warns() {
    let content = r#"{"sessionId":"session-a","kind":"main"}
{"sessionId":"session-b","kind":"main"}
{"timestamp":"2026-02-01T00:00:00Z","type":"user","content":"hello"}"#;

    let (session, warnings) = parse_gemini_jsonl_session(content).unwrap();
    assert_eq!(session.session_id.as_deref(), Some("session-a"));
    assert_eq!(
        warnings,
        vec![GeminiSessionWarning::SessionIdDrift {
            first: "session-a".to_string(),
            ignored: vec!["session-b".to_string()],
        }]
    );
}

#[test]
fn test_gemini_jsonl_same_session_id_has_no_drift_warning() {
    let content = r#"{"sessionId":"session-a","kind":"main"}
{"sessionId":"session-a","kind":"main"}
{"timestamp":"2026-02-01T00:00:00Z","type":"user","content":"hello"}"#;

    let (_session, warnings) = parse_gemini_jsonl_session(content).unwrap();
    assert!(warnings.is_empty());
}

#[test]
fn test_gemini_jsonl_missing_session_id_falls_back_to_filename() {
    let root = unique_test_dir("gemini-missing-session");
    let tmp = root.join("session-fallback.jsonl");
    let _ = fs::remove_dir_all(&root);
    write_file(
        &tmp,
        r#"{"timestamp":"2026-02-01T00:00:00Z","type":"user","content":"hello"}"#,
    );

    let (entries, warnings) =
        parse_gemini_session_with_diagnostics(&tmp, &default_config(true)).unwrap();
    assert_eq!(entries[0].session_id, "session-fallback");
    assert_eq!(
        warnings,
        vec![GeminiSessionWarning::MissingSessionId {
            fallback: "session-fallback".to_string(),
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_gemini_naive_timestamp_is_kept_as_utc() {
    let root = unique_test_dir("gemini-naive-ts");
    let tmp = root.join("session.json");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"sessionId":"session-a","messages":[{"type":"user","content":"hello","timestamp":"2026-02-01T00:00:00"}]}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_gemini_session_with_diagnostics(&tmp, &default_config(true)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(
        entries[0].timestamp,
        Utc.with_ymd_and_hms(2026, 2, 1, 0, 0, 0).unwrap()
    );
    assert!(warnings.is_empty());

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_gemini_invalid_timestamp_warns() {
    let root = unique_test_dir("gemini-invalid-ts");
    let tmp = root.join("session.json");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"sessionId":"session-a","messages":[{"type":"user","content":"hello","timestamp":"bad"}]}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_gemini_session_with_diagnostics(&tmp, &default_config(true)).unwrap();
    assert!(entries.is_empty());
    assert_eq!(
        warnings,
        vec![GeminiSessionWarning::UnparsableTimestamp {
            count: 1,
            samples: vec!["message 1: bad".to_string()],
        }]
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_gemini_unknown_role_preserves_content_as_system_note() {
    let root = unique_test_dir("gemini-unknown-role");
    let tmp = root.join("session.json");
    let _ = fs::remove_dir_all(&root);
    let content = r#"{"sessionId":"session-a","messages":[{"type":"mystery","content":"keep me","timestamp":"2026-02-01T00:00:00Z"}]}"#;
    write_file(&tmp, content);

    let (entries, warnings) =
        parse_gemini_session_with_diagnostics(&tmp, &default_config(true)).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].role, "system");
    assert_eq!(entries[0].frame_kind, Some(FrameKind::SystemNote));
    assert_eq!(entries[0].message, "keep me");
    assert_eq!(warnings.len(), 1);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_junie_session_id_canonical_path() {
    let path = PathBuf::from("/tmp/session-260408-214715-abcd/events.jsonl");
    let (session_id, warning) = junie_session_id_from_path_with_warning(&path);
    assert_eq!(session_id, "260408-214715-abcd");
    assert!(warning.is_none());
}

#[test]
fn test_junie_session_id_parentless_events_hashes() {
    let path = PathBuf::from("events.jsonl");
    let (session_id, warning) = junie_session_id_from_path_with_warning(&path);
    assert!(session_id.starts_with("unknown-"));
    assert_ne!(session_id, "events");
    assert_eq!(
        warning,
        Some(JunieSessionWarning::JunieFallbackId {
            fallback: session_id
        })
    );
}

#[test]
fn test_junie_session_id_nested_path_walks_ancestors() {
    let path = PathBuf::from("/tmp/session-260408-214715-abcd/subdir/events.jsonl");
    let (session_id, warning) = junie_session_id_from_path_with_warning(&path);
    assert_eq!(session_id, "260408-214715-abcd");
    assert!(warning.is_none());
}

#[test]
fn test_junie_session_id_wrapper_uses_ancestor_logic() {
    let path = PathBuf::from("/tmp/session-260408-214715-abcd/subdir/events.jsonl");
    assert_eq!(
        junie_session_id_from_path_with_warning(&path).0,
        "260408-214715-abcd"
    );
}

#[test]
fn test_project_filter_matches_owner_repo_segments() {
    assert!(project_filter_matches_path(
        "/Users/user/Git/Loctree/aicx/src",
        &["Loctree/aicx".to_string()]
    ));
}

#[test]
fn test_project_filter_matches_path_local_checkout_without_git_does_not_match_owner() {
    // Pass-4 Wave F-2 (PR #8 follow-up to chatgpt-codex-connector P1):
    // the old "last-segment relax" that let `-p Loctree/aicx` match
    // `/Users/user/Git/aicx` regardless of owner is gone. It leaked
    // cross-org: filter `Loctree/aicx` ALSO matched `/.../VetCoders/aicx`.
    //
    // Bug #14's original intent ("local checkout matches canonical
    // identity") now travels through Tier 1 — `aicx_parser::segmentation::
    // infer_tiered_identity_from_cwd` consults the local `.git/config`
    // remote URL and answers honestly. This unit test only exercises
    // paths with NO `.git` (cwd is a random tmp-ish path), so Tier 1
    // returns None and the strict adjacency path correctly refuses.
    //
    // The "with git remote" case is covered by
    // `aicx_parser::segmentation` tests; here we lock in the strict
    // behavior so the cross-org leak cannot silently come back.
    assert!(
        !project_filter_matches_path("/some/non-git/scratch/aicx", &["Loctree/aicx".to_string()]),
        "without a .git remote pointing at Loctree/aicx, the path-only \
         strict matcher must NOT accept `-p Loctree/aicx` for a directory \
         that merely ends in `aicx` — that's the cross-org leak we removed"
    );
    // The symmetric anti-leak: a checkout literally under `/VetCoders/aicx`
    // must also be rejected by `-p Loctree/aicx` at the strict tier.
    assert!(
        !project_filter_matches_path(
            "/some/non-git/scratch/VetCoders/aicx",
            &["Loctree/aicx".to_string()]
        ),
        "cross-org leak guard: `-p Loctree/aicx` must NOT accept a path \
         under /VetCoders/aicx"
    );
    // And the positive control: strict adjacency `Loctree/aicx` in the
    // path DOES match (this is Tier 3 strict behavior).
    assert!(project_filter_matches_path(
        "/some/non-git/scratch/Loctree/aicx",
        &["Loctree/aicx".to_string()]
    ));
}

#[test]
fn test_project_filter_matches_path_tier1_resolves_canonical_from_remote_url() {
    // Tier 1 also matches when `cwd` is itself a remote URL string —
    // `infer_repo_identity_from_remote_like` parses common shapes.
    // This locks in the canonical-resolver wiring without needing a
    // real `.git/config` on disk.
    assert!(
        project_filter_matches_path(
            "https://github.com/Loctree/aicx",
            &["Loctree/aicx".to_string()]
        ),
        "Tier 1 must resolve a github HTTPS URL to Loctree/aicx"
    );
    assert!(
        !project_filter_matches_path(
            "https://github.com/VetCoders/aicx",
            &["Loctree/aicx".to_string()]
        ),
        "Tier 1 must reject a github URL whose owner differs from the filter"
    );
}

#[test]
fn test_is_windows_absolute_path_recognizes_drive_letter_and_unc() {
    // Drive-letter form, both separators
    assert!(is_windows_absolute_path("C:\\Users\\user\\Git\\aicx"));
    assert!(is_windows_absolute_path("C:/Users/user/Git/aicx"));
    assert!(is_windows_absolute_path("d:\\code"));
    assert!(is_windows_absolute_path("Z:/work"));
    // UNC form
    assert!(is_windows_absolute_path("\\\\fileserver\\share\\repo"));
    // Negative cases
    assert!(!is_windows_absolute_path("/Users/user/Git/aicx")); // Unix
    assert!(!is_windows_absolute_path("Loctree/aicx")); // canonical slug
    assert!(!is_windows_absolute_path("C")); // too short
    assert!(!is_windows_absolute_path("C:")); // missing separator
    assert!(!is_windows_absolute_path("C:doc.txt")); // drive-letter relative (Windows-legal but not absolute)
    assert!(!is_windows_absolute_path("12:\\foo")); // not a letter
    assert!(!is_windows_absolute_path("\\single-backslash")); // not UNC
    assert!(!is_windows_absolute_path("")); // empty
}

#[test]
fn test_project_filter_matches_path_windows_segments_match_through_tier3() {
    // Regression for chatgpt-codex-connector P1 at src/sources.rs:1069:
    // Tier 3 must still recognize backslash-separated path segments.
    // Even when Tier 1 (canonical resolver) returns None for a Windows
    // path on a non-Windows CI runner (no `.git` on disk), the strict
    // adjacency matcher on `\` / `/` segments must accept the filter.
    //
    // The Tier 1 path itself (Windows .git/config resolution on a
    // Windows runner) is not asserted in unit tests because the parser
    // may relative-resolve a fake Windows shape against the test
    // process's own cwd. That cross-platform behavior is covered by
    // the resolver's own crate tests, not here.
    assert!(
        project_filter_matches_path("C:\\repos\\Loctree\\aicx", &["Loctree/aicx".to_string()]),
        "Tier 3 must accept adjacent `Loctree\\aicx` segments in a Windows path"
    );
    assert!(
        project_filter_matches_path(
            "\\\\fileserver\\share\\Loctree\\aicx",
            &["Loctree/aicx".to_string()]
        ),
        "Tier 3 must accept adjacent `Loctree\\aicx` segments in a UNC path"
    );
}

#[test]
fn test_project_filter_matches_path_strict_owner_repo() {
    assert!(project_filter_matches_path(
        "/x/Loctree/aicx",
        &["Loctree/aicx".to_string()]
    ));
}

#[test]
fn test_project_filter_matches_path_substring_does_not_leak() {
    assert!(!project_filter_matches_path(
        "/x/vista-portal",
        &["vista".to_string()]
    ));
}

#[test]
fn test_project_filter_matches_owner_wildcard_segment() {
    assert!(project_filter_matches_path(
        "/Users/user/Git/Loctree/aicx",
        &["Loctree/".to_string()]
    ));
}

#[test]
fn test_project_filter_matches_repo_wildcard_segment() {
    assert!(project_filter_matches_path(
        "/Users/user/Git/Other/aicx",
        &["/aicx".to_string()]
    ));
}

#[test]
fn test_project_filter_rejects_vista_for_vista_portal() {
    assert!(!project_filter_matches_path(
        "/Users/user/Git/vista-portal",
        &["vista".to_string()]
    ));
}

#[test]
fn test_project_filter_matches_path_strict_segments() {
    // Empty filter => match all.
    assert!(project_filter_matches_path("/anything", &[]));

    // Exact segment match — the key fix vs old substring behavior.
    assert!(project_filter_matches_path(
        "/tmp/test/foo",
        &["test".to_string()]
    ));
    // Substring used to false-positive here; now correctly rejected.
    assert!(!project_filter_matches_path(
        "/tmp/fastest-project",
        &["test".to_string()]
    ));

    // No word-boundary matching inside a segment.
    assert!(!project_filter_matches_path(
        "/Users/user/Git/vista-portal-pr15-hotfix",
        &["portal".to_string()]
    ));
    assert!(!project_filter_matches_path(
        "/Users/user/Git/vista-portal-pr15-hotfix",
        &["vista-portal".to_string()]
    ));

    // Case-insensitive both directions.
    assert!(project_filter_matches_path(
        "/TMP/Test/foo",
        &["test".to_string()]
    ));
    assert!(project_filter_matches_path(
        "/tmp/test",
        &["TEST".to_string()]
    ));

    // ANY filter mode (multiple filters).
    assert!(project_filter_matches_path(
        "/tmp/abc",
        &["xyz".to_string(), "abc".to_string()]
    ));

    // Windows-style backslash separator.
    assert!(project_filter_matches_path(
        "C:\\Users\\test\\foo",
        &["test".to_string()]
    ));

    // Empty cwd never matches a non-empty filter.
    assert!(!project_filter_matches_path("", &["x".to_string()]));
}

#[test]
fn test_extract_codex_file_project_filter_rejects_substring_false_positive() {
    // `--project test` must NOT match a session whose cwd is `/tmp/fastest-project`.
    let root = unique_test_dir("codex-pf-substring-fp");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"sess","cwd":"/tmp/fastest-project"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"hello"}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec!["test".to_string()],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_codex_file(&tmp, &config).unwrap();
    assert!(
        entries.is_empty(),
        "session in /tmp/fastest-project must not match --project test, got {} entries",
        entries.len()
    );

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_extract_codex_file_project_filter_accepts_path_segment() {
    let root = unique_test_dir("codex-pf-accept");
    let tmp = root.join("session.jsonl");
    let _ = fs::remove_dir_all(&root);

    let content = r#"{"timestamp":"2026-02-01T00:00:00Z","type":"session_meta","payload":{"id":"sess","cwd":"/Users/x/Git/vista-portal"}}
{"timestamp":"2026-02-01T00:00:01Z","type":"event_msg","payload":{"type":"user_message","message":"hi"}}"#;
    write_file(&tmp, content);

    let config = ExtractionConfig {
        project_filter: vec!["vista-portal".to_string()],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_codex_file(&tmp, &config).unwrap();
    assert_eq!(entries.len(), 1, "vista-portal filter must match path");

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_codescribe_filter_per_transcript_not_splattered() {
    let root = unique_test_dir("codescribe-splatter");
    let home = root.join("home");
    let day = home
        .join(".codescribe")
        .join("transcriptions")
        .join("2026-05-22");
    fs::create_dir_all(&day).unwrap();

    // Create the expected repo directories so that resolve_codescribe_cwd_hint works
    let aicx_dir = home.join("Loctree").join("aicx");
    fs::create_dir_all(&aicx_dir).unwrap();
    let widgets_dir = home.join("acme").join("widgets");
    fs::create_dir_all(&widgets_dir).unwrap();

    // Transcript 1: explicitly mentions "aicx" in content (JSON)
    let content_matching = r#"{"segments":[{"start":0,"end":1,"speaker":"Maciej","text":"let's work on Loctree/aicx today."}]}"#;
    write_file(&day.join("100000_match.json"), content_matching);

    // Transcript 2: explicit project frontmatter to contradict the fallback
    let content_unmatching =
        "---\nproject: acme/widgets\n---\n### Maciej:\nlet's work on widgets.\n";
    write_file(&day.join("110000_unmatch.md"), content_unmatching);

    let config = ExtractionConfig {
        project_filter: vec!["Loctree/aicx".to_string()],
        cutoff: Utc.timestamp_opt(0, 0).single().unwrap(),
        include_assistant: true,
        watermark: None,
    };

    let entries = extract_codescribe_from_home(&home, &config).unwrap();
    // Only the matching one should survive
    assert_eq!(entries.len(), 1);
    assert!(entries[0].message.contains("Loctree/aicx"));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_operator_md_owner_repo_decode_preserves_owner() {
    let root = unique_test_dir("operator-owner-repo");
    let home = root.join("home");

    // Create directories
    let acme_dir = home.join("acme").join("widgets");
    fs::create_dir_all(&acme_dir).unwrap();
    let globex_dir = home.join("globex").join("widgets");
    fs::create_dir_all(&globex_dir).unwrap();

    // Acme
    let acme_cwd = resolve_operator_cwd_hint(&home, Path::new("dummy"), Some("acme/widgets"));
    assert_eq!(acme_cwd.as_deref(), Some(acme_dir.to_str().unwrap()));

    // Globex
    let globex_cwd = resolve_operator_cwd_hint(&home, Path::new("dummy"), Some("globex/widgets"));
    assert_eq!(globex_cwd.as_deref(), Some(globex_dir.to_str().unwrap()));

    assert_ne!(acme_cwd, globex_cwd);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_discover_operator_markdown_honors_caller_cutoff_for_all_time() {
    let root = unique_test_dir("operator-md-cutoff-all-time");
    let home = root.join("home");
    let downloads = home.join("Downloads");
    fs::create_dir_all(&downloads).unwrap();

    // A markdown file with mtime 90 days ago — well outside the legacy 30d default.
    let ancient = downloads.join("ancient-decision.md");
    write_file(
        &ancient,
        "Decision: ancient choice from before the 30d window.",
    );
    let ninety_days_ago = (Utc::now() - Duration::days(90)).timestamp();
    set_mtime(&ancient, ninety_days_ago);

    // Caller cutoff = epoch == "all time"; mirrors `aicx store -H 0`.
    let epoch = Utc.timestamp_opt(0, 0).single().unwrap();
    let discovered = discover_operator_markdown_from(&home, None, Some(epoch));

    assert_eq!(
        discovered.len(),
        1,
        "epoch caller cutoff must discover markdown regardless of mtime age",
    );
    assert_eq!(discovered[0].path, ancient);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_discover_operator_markdown_default_30d_when_no_caller_cutoff() {
    let root = unique_test_dir("operator-md-cutoff-default");
    let home = root.join("home");
    let downloads = home.join("Downloads");
    fs::create_dir_all(&downloads).unwrap();

    // Recent file: mtime defaults to "now" via fs::write — inside 30d default.
    let recent = downloads.join("recent.md");
    write_file(
        &recent,
        "Decision: recent choice within the 30d default window.",
    );

    // Ancient file: mtime 90 days ago — outside 30d default.
    let ancient = downloads.join("ancient.md");
    write_file(&ancient, "Decision: pre-default-window history.");
    let ninety_days_ago = (Utc::now() - Duration::days(90)).timestamp();
    set_mtime(&ancient, ninety_days_ago);

    // None caller cutoff → legacy 30d default preserved.
    let discovered = discover_operator_markdown_from(&home, None, None);

    assert_eq!(
        discovered.len(),
        1,
        "None caller cutoff must preserve the 30d default; ancient file should be filtered",
    );
    assert_eq!(discovered[0].path, recent);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn test_discover_operator_markdown_honors_caller_cutoff_when_narrower() {
    let root = unique_test_dir("operator-md-cutoff-narrower");
    let home = root.join("home");
    let downloads = home.join("Downloads");
    fs::create_dir_all(&downloads).unwrap();

    // Fixture is 15 days old: inside the 30d default, outside a 7d caller window.
    let fifteen_days_old = downloads.join("fifteen-days-old.md");
    write_file(
        &fifteen_days_old,
        "Decision: 15-day-old choice should not survive a 7-day caller cutoff.",
    );
    let fifteen_days = (Utc::now() - Duration::days(15)).timestamp();
    set_mtime(&fifteen_days_old, fifteen_days);

    // Caller cutoff = 7 days ago — narrower than the 30d default.
    let seven_days_ago = Utc::now() - Duration::days(7);
    let discovered = discover_operator_markdown_from(&home, None, Some(seven_days_ago));

    assert!(
        discovered.is_empty(),
        "15-day-old markdown must be excluded by a 7-day caller cutoff (narrower than 30d default)",
    );

    let _ = fs::remove_dir_all(&root);
}
