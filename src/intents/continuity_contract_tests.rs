//! Frozen-clock contracts through the production catalog -> provider parser -> intents path.
use super::*;
use std::fs;

fn fixture(date: &str, turns: &[(&str, &str, &str)]) -> PathBuf {
    static NEXT_FIXTURE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let sequence = NEXT_FIXTURE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "aicx-continuity-contract-{}-{}-{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap(),
        sequence
    ));
    fs::create_dir_all(&root).unwrap();
    let path = root.join("session.jsonl");
    let cwd = "/fixtures/Loctree/aicx";
    let session_id = format!("11111111-2222-3333-4444-{sequence:012x}");
    let mut body = String::new();
    for (time, role, text) in turns {
        body.push_str(&serde_json::json!({"type":role,"timestamp":time,"sessionId":session_id,"cwd":cwd,"message":{"role":role,"content":text}}).to_string());
        body.push('\n');
    }
    fs::write(&path, body).unwrap();
    let entry = crate::catalog::CatalogEntry {
        schema: crate::catalog::CATALOG_SCHEMA.into(),
        session_id: session_id.clone(),
        agent: "claude".into(),
        project: Some("Loctree/aicx".into()),
        date: Some(date.into()),
        cwd: Some(cwd.into()),
        source_path: path.to_string_lossy().into(),
        source_len: None,
        source_mtime_ns: None,
        source_bundle_fingerprint: None,
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

#[test]
fn continuity_contract_checklist_resolution_is_specific() {
    let root = fixture(
        "2026-09-24",
        &[
            (
                "2026-10-02T12:00:00Z",
                "user",
                "- [ ] add delivery bus\n- [ ] add revision bus\nDecision: preserve all audio unless explicitly opted out.",
            ),
            ("2026-10-02T12:01:00Z", "user", "- [x] add delivery bus"),
        ],
    );
    let e = extract(&root);
    assert!(
        !e.records
            .iter()
            .any(|r| r.kind == IntentKind::Task && r.summary.contains("delivery bus"))
    );
    assert!(
        e.records
            .iter()
            .any(|r| r.kind == IntentKind::Task && r.summary.contains("revision bus"))
    );
    assert!(
        e.records
            .iter()
            .any(|r| r.kind == IntentKind::Decision && r.summary.contains("preserve all audio"))
    );
    fs::remove_dir_all(root).unwrap();
}
#[test]
fn continuity_contract_quoted_agent_and_tool_text_is_not_human_decision() {
    let root = fixture(
        "2026-10-02",
        &[
            (
                "2026-10-02T12:00:00Z",
                "user",
                "Agent report follows:\n> Decision: completed command is the new policy.\n```\nDecision: output must become founder policy.\n```\nDecision: keep original audio independent of revisions.",
            ),
            (
                "2026-10-02T12:01:00Z",
                "assistant",
                "Decision: discard original audio after successful command.",
            ),
        ],
    );
    let e = extract(&root);
    assert_eq!(
        e.records.len(),
        1,
        "quoted/agent material must not become fresh human decisions: {:?}",
        e.records
    );
    assert!(e.records[0].summary.contains("keep original audio"));
    assert_eq!(
        e.records[0].provenance.as_ref().unwrap().attribution,
        "human_candidate"
    );
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn continuity_contract_unbounded_unknown_frame_does_not_borrow_known_time() {
    let root = fixture(
        "2026-10-02",
        &[
            (
                "2026-10-02T12:00:00Z",
                "user",
                "Decision: preserve known audio policy.",
            ),
            ("", "user", "Decision: preserve undated revision policy."),
        ],
    );
    let bounded = extract(&root);
    assert_eq!(bounded.records.len(), 1);
    assert_eq!(bounded.selection[0].unknown_time_frames, 1);
    let config = IntentsConfig {
        project: "Loctree/aicx".into(),
        hours: 0,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
        live: false,
    };
    let unbounded = extract_intents_from_root_at_with_stats(
        &config,
        &root,
        "2026-10-03T04:47:00Z".parse().unwrap(),
    )
    .unwrap();
    assert_eq!(unbounded.records.len(), 2);
    let unknown = unbounded
        .records
        .iter()
        .find(|r| r.summary.contains("undated"))
        .unwrap();
    assert_eq!(unknown.timestamp, None);
    assert_eq!(unknown.date, "unknown");
    fs::remove_dir_all(root).unwrap();
}

#[test]
fn continuity_contract_candidate_budget_counts_every_dropped_candidate() {
    let message = (0..MAX_CANDIDATES + 3)
        .map(|n| format!("Decision: keep rail number {n}."))
        .collect::<Vec<_>>()
        .join("\n");
    let root = fixture("2026-10-02", &[("2026-10-02T12:00:00Z", "user", &message)]);
    let extraction = extract(&root);
    assert_eq!(extraction.stats.candidate_cap, MAX_CANDIDATES);
    assert_eq!(extraction.stats.dropped_candidates, 3);
    assert_eq!(extraction.records.len(), MAX_CANDIDATES);
    assert_eq!(extraction.stats.source_errors, 0);
    fs::remove_dir_all(root).unwrap();
}
