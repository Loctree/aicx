use aicx::sources::{ExtractionConfig, extract_codex_file};
use chrono::Utc;
use std::fs;

#[test]
fn test_rejects_legacy_json_format() {
    let tmp = std::env::temp_dir().join("ai-ctx-legacy-codex.json");
    let content = r#"{
      "session": {
        "timestamp": "2025-09-20T21:51:35.696Z",
        "id": "17dd1ddd-a5cb-4137-a837-51d06bc109a6",
        "instructions": ""
      },
      "items": []
    }"#;
    fs::write(&tmp, content).unwrap();

    let cutoff = Utc::now();
    let config = ExtractionConfig {
        project_filter: vec![],
        cutoff,
        include_assistant: true,
        watermark: None,
    };

    let result = extract_codex_file(&tmp, &config);
    assert!(result.is_err());
    let err_str = result.unwrap_err().to_string();
    assert!(err_str.contains("Legacy Codex JSON rollout format is unsupported"));

    let _ = fs::remove_file(&tmp);
}
