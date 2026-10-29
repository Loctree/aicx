// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

use aicx::extraction::ExtractionConfig;
use aicx::importers::{discover_codescribe_transcripts, extract_codescribe_from_home};
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
        r#"{"speaker":"engineer","keywords":["Vetcoders","vibecrafted"]}"#,
    );
    write_file(
        &day.join("175300_operator-decision_raw.txt"),
        "Decision: Vibecrafted owns the operator workflow for Vetcoders.",
    );
    write_file(
        &day.join("191400_chat.md"),
        "### Operator:\nDecision: portal copy must stay concrete.\n\n### Engineer:\nIntent: ship the aicx adapter today.\n",
    );
    write_file(
        &day.join("193600_whisper.json"),
        r#"{"segments":[{"start":1.5,"end":3.0,"speaker":"Engineer","text":"Decision: index Codescribe transcripts."}]}"#,
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
            .any(|entry| entry.message.contains("speaker_hint: engineer"))
    );
    assert!(entries.iter().any(|entry| entry.timestamp
        == Utc.with_ymd_and_hms(2026, 4, 30, 19, 36, 1).unwrap()
            + chrono::Duration::milliseconds(500)));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn catalog_takes_keep_one_text_per_recording_and_never_admit_failed_or_rewrites() {
    let root = unique_test_dir("catalog-takes");
    let home = root.join("home");
    let day = home
        .join(".codescribe")
        .join("transcriptions")
        .join("2026-04-30");
    for (name, content) in [
        // One recording, three text exports: the unnumbered raw wins.
        ("100000_alpha_raw.m4a", ""),
        ("100000_alpha_raw.txt", "alpha raw words"),
        ("100000_alpha_raw_1.txt", "alpha colliding raw export"),
        ("100000_alpha_cloud_1.txt", "alpha cloud export"),
        // Failed takes, numbered or not, are never speech.
        ("110000_beta_failed.txt", "No reliable speech detected"),
        ("110000_beta_failed_1.txt", "beta failed retry"),
        // Formatter rewrites alone are never attributed verbatim.
        ("120000_gamma_ai_1.txt", "gamma rewrite"),
        ("120000_gamma_formatted_1.txt", "gamma formatted rewrite"),
        // A numbered file with audio of its own is a distinct recording.
        ("130000_delta_raw.m4a", ""),
        ("130000_delta_raw.txt", "delta first recording"),
        ("130000_delta_raw_2.m4a", ""),
        ("130000_delta_raw_2.txt", "delta second recording"),
        // The only text of a take is kept whatever its export number.
        ("140000_epsilon_cloud_1.txt", "epsilon cloud only"),
        ("150000_zeta_raw_1.txt", "zeta numbered raw only"),
        // A slug ending in digits is not a collision suffix.
        ("160000_build_2115.md", "build notes"),
    ] {
        write_file(&day.join(name), content);
    }

    let mut kept: Vec<String> = aicx::importers::catalog_takes(&home)
        .into_iter()
        .map(|take| {
            take.path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    kept.sort();
    assert_eq!(
        kept,
        vec![
            "100000_alpha_raw.txt",
            "130000_delta_raw.txt",
            "130000_delta_raw_2.txt",
            "140000_epsilon_cloud_1.txt",
            "150000_zeta_raw_1.txt",
            "160000_build_2115.md",
        ]
    );

    let _ = fs::remove_dir_all(&root);
}
