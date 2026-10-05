//! Frozen-clock contracts through the production catalog -> provider parser -> intents path.
use super::*;
use std::fs;

fn fixture(date: &str, turns: &[(&str, &str, &str)]) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "aicx-continuity-contract-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap()
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("session.jsonl");
    let cwd = "/fixtures/Loctree/aicx";
    let mut body = String::new();
    for (time, role, text) in turns {
        body.push_str(&serde_json::json!({"type":role,"timestamp":time,"sessionId":"full-session-identity","cwd":cwd,"message":{"role":role,"content":text}}).to_string());
        body.push('\n');
    }
    fs::write(&path, body).unwrap();
    let entry = crate::catalog::CatalogEntry {
        schema: crate::catalog::CATALOG_SCHEMA.into(),
        session_id: "full-session-identity".into(),
        agent: "claude".into(),
        project: Some("Loctree/aicx".into()),
        date: Some(date.into()),
        cwd: Some(cwd.into()),
        source_path: path.to_string_lossy().into(),
        source_len: None,
        source_mtime_ns: None,
        title: None,
        machine: None,
        logical_session_id: None,
        session_kind: None,
    };
    let catalog = crate::catalog::sessions_path_for(&root);
    fs::create_dir_all(catalog.parent().unwrap()).unwrap();
    fs::write(
        catalog,
        format!("{}\n", serde_json::to_string(&entry).unwrap()),
    )
    .unwrap();
    // Prove the fixture actually reaches the real provider parser before testing selection.
    let (_, frames, _) =
        crate::source_index::read_catalog_signal_with_scope_at(&root, &entry, FrameKind::UserMsg)
            .unwrap();
    assert_eq!(
        frames.len(),
        turns.iter().filter(|(_, r, _)| *r == "user").count()
    );
    root
}
fn extract(root: &Path) -> IntentExtraction {
    let config = IntentsConfig {
        project: "Loctree/aicx".into(),
        hours: 96,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
        live: false,
    };
    extract_intents_from_root_at_with_stats(&config, root, "2026-10-03T04:47:00Z".parse().unwrap())
        .unwrap()
}
#[test]
fn continuity_contract_old_session_recent_utterance() {
    let root = fixture(
        "2026-09-24",
        &[(
            "2026-10-02T12:00:00Z",
            "user",
            "Decision: preserve all audio unless the user opts out.",
        )],
    );
    let e = extract(&root);
    assert_eq!(e.stats.source_errors, 0);
    assert_eq!(
        e.records.len(),
        1,
        "recent utterance in an old session must qualify: {:?}",
        e.records
    );
    assert_eq!(e.records[0].date, "2026-10-02");
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn continuity_contract_exact_utc_window_and_per_frame_time() {
    let root = fixture(
        "2026-10-02",
        &[
            (
                "2026-09-29T04:46:59Z",
                "user",
                "Decision: exclude the pre-window old request.",
            ),
            (
                "2026-09-29T06:47:00+02:00",
                "user",
                "Decision: preserve the exact UTC cutoff request.",
            ),
            (
                "2026-10-02T12:00:00Z",
                "user",
                "Decision: preserve the recent audio constraint.",
            ),
            (
                "2026-10-03T04:47:01Z",
                "user",
                "Decision: exclude the future request.",
            ),
            (
                "2026-10-02T12:01:00Z",
                "assistant",
                "Decision: an agent proposal must stay outside human decisions.",
            ),
        ],
    );
    let e = extract(&root);
    assert_eq!(
        e.records.len(),
        2,
        "only [cutoff, now] utterances qualify: {:?}",
        e.records
    );
    let cutoff = e
        .records
        .iter()
        .find(|r| r.summary.contains("cutoff"))
        .unwrap();
    assert_eq!(
        cutoff.timestamp.as_deref(),
        Some("2026-09-29T04:47:00+00:00")
    );
    fs::remove_dir_all(root).unwrap();
}
