// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

use aicx::output::{
    ConversationExtractStats, ConversationMessage, ReportMetadata, write_conversation_json,
    write_conversation_markdown,
};
use aicx::redact::redact_secrets;
use chrono::{TimeZone, Utc};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

fn chars(ch: char, len: usize) -> String {
    std::iter::repeat_n(ch, len).collect()
}

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-secret-redaction-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ))
}

fn gcp_service_account_json() -> (String, Vec<String>) {
    gcp_service_account_json_with_padding(0)
}

fn gcp_service_account_json_with_padding(padding_len: usize) -> (String, Vec<String>) {
    let private_key_id = chars('1', 40);
    let client_email = "aicx-redaction-test@aicx-test.iam.gserviceaccount.com".to_string();
    let private_key = format!(
        "{}{}{}{}{}",
        "-----BEGIN ",
        "PRIVATE KEY-----\\n",
        chars('P', 32),
        "\\n-----END PRIVATE KEY-----",
        "\\n"
    );
    let padding = chars('Z', padding_len);
    let json = format!(
        r#"{{
  "type": "service_account",
  "project_id": "aicx-test",
  "padding": "{padding}",
  "private_key_id": "{private_key_id}",
  "private_key": "{private_key}",
  "client_email": "{client_email}",
  "token_uri": "https://oauth2.googleapis.com/token"
}}"#
    );

    (json, vec![private_key_id, private_key, client_email])
}

#[test]
fn redact_secrets_handles_large_gcp_service_account_json() {
    let (gcp_json, raw_values) = gcp_service_account_json_with_padding(4096);
    assert!(
        gcp_json.len() > 4096,
        "fixture must exceed 4 KiB, got {} bytes",
        gcp_json.len()
    );

    let redacted = redact_secrets(&gcp_json);

    for raw in raw_values {
        assert!(!redacted.contains(&raw), "redacted output leaked: {raw}");
    }
    assert!(redacted.contains(r#""private_key_id": "[REDACTED_GCP_PRIVATE_KEY_ID]""#));
    assert!(redacted.contains(r#""private_key": "[REDACTED_GCP_PRIVATE_KEY]""#));
    assert!(redacted.contains(r#""client_email": "[REDACTED_GCP_CLIENT_EMAIL]""#));
}

#[test]
fn extract_outputs_do_not_leak_modern_secret_families() {
    let root = unique_test_dir("modern-families");
    fs::create_dir_all(&root).expect("create temp output dir");
    let md_path = root.join("secrets.md");
    let json_path = root.join("secrets.json");

    let (gcp_json, mut raw_values) = gcp_service_account_json();
    let tokens = vec![
        format!("sk-ant-api03-{}", chars('A', 24)),
        format!("sk-proj-{}", chars('B', 24)),
        format!("ghs_{}", chars('C', 36)),
        format!("gho_{}", chars('D', 36)),
        format!("ghu_{}", chars('E', 36)),
        format!("ghr_{}", chars('F', 36)),
        format!("glpat-{}", chars('G', 24)),
        format!("ASIA{}", chars('H', 16)),
        format!("xapp-1-{}", chars('I', 16)),
        format!(
            "eyJ{}.eyJ{}.{}",
            chars('J', 12),
            chars('K', 12),
            chars('L', 43)
        ),
        format!("sk_live_{}", chars('M', 24)),
        format!("sk_test_{}", chars('N', 24)),
        format!("rk_live_{}", chars('O', 24)),
    ];
    raw_values.extend(tokens.clone());

    let mut payloads = tokens
        .into_iter()
        .enumerate()
        .map(|(idx, token)| format!("secret family {idx}: {token}"))
        .collect::<Vec<_>>();
    payloads.push(format!("gcp service-account fixture: {gcp_json}"));

    let messages = payloads
        .iter()
        .enumerate()
        .map(|(idx, payload)| ConversationMessage {
            timestamp: Utc
                .with_ymd_and_hms(2026, 5, 20, 12, idx as u32, 0)
                .unwrap(),
            agent: "codex".to_string(),
            session_id: "secret-redaction-e2e".to_string(),
            role: if idx % 2 == 0 { "user" } else { "assistant" }.to_string(),
            message: payload.clone(),
            repo_project: "Loctree/aicx".to_string(),
            source_path: Some("/Users/user/Git/aicx".to_string()),
            branch: Some("feat/test-branch".to_string()),
            message_kind: Default::default(),
            collapse_stub_kind: None,
        })
        .collect::<Vec<_>>();

    let metadata = ReportMetadata {
        generated_at: Utc.with_ymd_and_hms(2026, 5, 20, 12, 0, 0).unwrap(),
        project_filter: Some("Loctree/aicx".to_string()),
        hours_back: 1,
        total_entries: messages.len(),
        sessions: vec!["secret-redaction-e2e".to_string()],
    };
    let extract_stats = ConversationExtractStats {
        aicx_version: env!("CARGO_PKG_VERSION"),
        redaction_enabled: true,
        raw_entries: messages.len(),
        conversation_messages: messages.len(),
        conversation_projection: "user_assistant_only",
        exact_short_duplicates_dropped: 0,
        harness_noise_dropped: 0,
    };

    write_conversation_markdown(&md_path, &messages, &metadata).expect("write markdown");
    write_conversation_json(&json_path, &messages, &metadata, &extract_stats).expect("write json");

    let md = fs::read_to_string(&md_path).expect("read markdown");
    let json = fs::read_to_string(&json_path).expect("read json");

    for raw in raw_values {
        assert!(!md.contains(&raw), "markdown leaked raw value: {raw}");
        assert!(!json.contains(&raw), "json leaked raw value: {raw}");
    }
    assert!(md.contains("[REDACTED_ANTHROPIC_KEY]"));
    assert!(json.contains("[REDACTED_GCP_PRIVATE_KEY]"));

    let _ = fs::remove_dir_all(&root);
}
