use aicx::sources::{
    ExtractionConfig, discover_codescribe_transcripts, extract_codescribe_from_home,
};
use aicx::timeline::FrameKind;
use chrono::{TimeZone, Utc};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

fn unique_test_dir(name: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-codescribe-{name}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before unix epoch")
            .as_nanos()
    ))
}

fn write_file(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, content).expect("write fixture");
}

fn extraction_config() -> ExtractionConfig {
    ExtractionConfig {
        project_filter: vec!["vibecrafted".to_string()],
        cutoff: Utc.with_ymd_and_hms(2026, 4, 30, 0, 0, 0).unwrap(),
        include_assistant: true,
        watermark: None,
    }
}

#[test]
fn codescribe_ingest_discovers_and_parses_txt_md_json_transcripts() {
    let root = unique_test_dir("formats");
    let home = root.join("home");
    let day = home
        .join(".codescribe")
        .join("transcriptions")
        .join("2026-04-30");
    let repo_root = home.join("Libraxis").join("vibecrafted");
    fs::create_dir_all(repo_root.join(".git")).expect("create project hint repo");

    write_file(
        &home.join(".codescribe").join("lexicon.custom.jsonl"),
        r#"{"speaker":"maciej","keywords":["VetCoders","vibecrafted"]}"#,
    );
    write_file(
        &day.join("175300_operator-decision_raw.txt"),
        "Decision: Vibecrafted owns the operator workflow for VetCoders.",
    );
    write_file(
        &day.join("191400_chat.md"),
        "### Monika:\nDecision: portal copy must stay concrete.\n\n### Maciej:\nIntent: ship the aicx adapter today.\n",
    );
    write_file(
        &day.join("193600_whisper.json"),
        r#"{"segments":[{"start":1.5,"end":3.0,"speaker":"Maciej","text":"Decision: index CodeScribe transcripts."}]}"#,
    );
    write_file(
        &day.join("193600_whisper.wav.truth.json"),
        r#"{"display_status":"sidecar, not a transcript"}"#,
    );
    write_file(
        &day.join("200000_no-speech_failed.txt"),
        "No reliable speech detected",
    );

    let discovered = discover_codescribe_transcripts(&home);
    assert_eq!(discovered.len(), 4, "truth sidecars must be ignored");

    let entries = extract_codescribe_from_home(&home, &extraction_config()).expect("extract");
    assert_eq!(entries.len(), 4, "no-speech txt should not emit an entry");
    assert!(entries.iter().all(|entry| entry.agent == "codescribe"));
    assert!(
        entries
            .iter()
            .all(|entry| entry.frame_kind == Some(FrameKind::UserMsg))
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.message.contains("kind: transcript"))
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry.cwd.as_deref() == Some(repo_root.to_str().unwrap()))
    );
    assert!(
        entries
            .iter()
            .any(|entry| entry.message.contains("speaker_hint: maciej"))
    );
    assert!(entries.iter().any(|entry| entry.timestamp
        == Utc.with_ymd_and_hms(2026, 4, 30, 19, 36, 1).unwrap()
            + chrono::Duration::milliseconds(500)));

    let _ = fs::remove_dir_all(&root);
}
