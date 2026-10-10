// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use aicx::intents::{
    INDEX_IDENTITY_SOURCE, IntentSourceFilter, IntentsConfig,
    extract_intents_with_stats_for_projects_filtered,
};

static HOME_LOCK: Mutex<()> = Mutex::new(());

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-intents-source-selection-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos()
    ))
}

fn json_path(path: &Path) -> String {
    let quoted = serde_json::Value::String(path.display().to_string()).to_string();
    quoted[1..quoted.len() - 1].to_string()
}

struct HomeGuard<'a> {
    _lock: MutexGuard<'a, ()>,
    previous_home: Option<String>,
    previous_aicx: Option<String>,
}

impl<'a> HomeGuard<'a> {
    fn set(root: &Path) -> Self {
        let lock = HOME_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous_home = std::env::var("HOME").ok();
        let previous_aicx = std::env::var("AICX_HOME").ok();
        unsafe {
            std::env::set_var("HOME", root);
            std::env::set_var("AICX_HOME", root.join(".aicx"));
        }
        Self {
            _lock: lock,
            previous_home,
            previous_aicx,
        }
    }
}

impl Drop for HomeGuard<'_> {
    fn drop(&mut self) {
        unsafe {
            match &self.previous_home {
                Some(value) => std::env::set_var("HOME", value),
                None => std::env::remove_var("HOME"),
            }
            match &self.previous_aicx {
                Some(value) => std::env::set_var("AICX_HOME", value),
                None => std::env::remove_var("AICX_HOME"),
            }
        }
    }
}

#[test]
fn indexed_old_session_keeps_fresh_utterance_and_drops_old_utterance() {
    let root = unique_root("old-session-new-utterance");
    let _guard = HomeGuard::set(&root);
    let repo = root.join("workspaces/aicx");
    fs::create_dir_all(repo.join(".git")).expect("create fixture repository");

    let session_id = "11111111-2222-4333-8444-555555555555";
    let session_dir = root.join(".codex/sessions/2025/01/01");
    fs::create_dir_all(&session_dir).expect("create Codex session directory");
    let source = session_dir.join(format!("rollout-2025-01-01T00-00-00-{session_id}.jsonl"));
    let cwd = json_path(&repo);
    let fresh = chrono::Utc::now() - chrono::Duration::days(1);
    let fresh_turn = (fresh - chrono::Duration::seconds(10)).to_rfc3339();
    let fresh_message = fresh.to_rfc3339();
    let fresh_date = fresh.format("%Y-%m-%d").to_string();
    let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
    let body = format!(
        r#"{{"timestamp":"2025-01-01T00:00:00Z","type":"session_meta","payload":{{"id":"{session_id}","cwd":"{cwd}"}}}}
{{"timestamp":"2025-01-01T00:01:00Z","type":"turn_context","payload":{{"cwd":"{cwd}"}}}}
{{"timestamp":"2025-01-01T00:01:10Z","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"Decision: discard the stale indexed utterance"}}]}}}}
{{"timestamp":"{fresh_turn}","type":"turn_context","payload":{{"cwd":"{cwd}"}}}}
{{"timestamp":"{fresh_message}","type":"response_item","payload":{{"type":"message","role":"user","content":[{{"type":"input_text","text":"Decision: preserve the fresh indexed utterance"}}]}}}}
"#
    );
    for (index, line) in body.lines().enumerate() {
        serde_json::from_str::<serde_json::Value>(line)
            .unwrap_or_else(|error| panic!("fixture line {}: {error}", index + 1));
    }
    fs::write(&source, body).expect("write old session with fresh utterance");

    let aicx_home = root.join(".aicx");
    aicx::catalog::rebuild(&aicx_home, &root).expect("rebuild fixture catalog");
    let catalog_entry = aicx::catalog::read_entries_at(&aicx_home)
        .expect("read fixture catalog")
        .into_iter()
        .find(|entry| entry.session_id == session_id)
        .expect("old session is cataloged");
    assert_eq!(
        catalog_entry.date.as_deref(),
        Some("2025-01-01"),
        "fixture must carry an old canonical metadata date"
    );
    aicx::source_index::build(&aicx_home, &[], false, true, false)
        .expect("publish fixture CURRENT");

    let config = IntentsConfig {
        project: "aicx".to_string(),
        hours: 720,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: Some(aicx::timeline::FrameKind::UserMsg),
        live: true,
    };
    let source_filter = IntentSourceFilter {
        agent: Some("codex".to_string()),
        date_lo: Some(fresh_date),
        date_hi: Some(today),
        full_source_scan: false,
    };
    let extraction = extract_intents_with_stats_for_projects_filtered(
        &config,
        &["aicx".to_string()],
        &source_filter,
    )
    .expect("extract through indexed source-selection lane");

    let receipt = extraction
        .selection
        .iter()
        .find(|receipt| receipt.session_id == session_id)
        .expect("indexed selection receipt");
    assert!(
        receipt.path.contains("extracts/codex")
            && receipt.parser_coverage.as_deref() == Some("complete_visible"),
        "fixture did not use the validated indexed extract lane: {receipt:?}"
    );
    assert_eq!(
        extraction.stats.identity_source, INDEX_IDENTITY_SOURCE,
        "regression must exercise published CURRENT, not census fallback"
    );
    let summaries = extraction
        .records
        .iter()
        .map(|record| record.summary.as_str())
        .collect::<Vec<_>>();
    assert!(
        summaries
            .iter()
            .any(|summary| summary.contains("fresh indexed utterance")),
        "fresh utterance in old session was rejected by metadata date: {summaries:?}"
    );
    assert!(
        summaries
            .iter()
            .all(|summary| !summary.contains("stale indexed utterance")),
        "old utterance survived per-utterance date filtering: {summaries:?}"
    );
    assert_eq!(receipt.parsed_frames, 2);
    assert_eq!(receipt.qualified_frames, 1);
    assert_eq!(receipt.outside_window_frames, 1);

    let appended_at = fresh + chrono::Duration::minutes(1);
    let mut changed = fs::read_to_string(&source).expect("read source before append");
    changed.push_str(&format!(
        "{}\n",
        serde_json::json!({
            "timestamp": appended_at.to_rfc3339(),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "Decision: expose the appended indexed utterance"
                }]
            }
        })
    ));
    fs::write(&source, changed).expect("append source after CURRENT publication");
    let refreshed = extract_intents_with_stats_for_projects_filtered(
        &config,
        &["aicx".to_string()],
        &source_filter,
    )
    .expect("re-source fingerprint-changed CURRENT row");
    assert!(
        refreshed
            .records
            .iter()
            .any(|record| record.summary.contains("appended indexed utterance")),
        "source append was hidden by stale CURRENT: {:?}",
        refreshed.records
    );

    fs::remove_file(&source).expect("remove indexed source");
    let missing = extract_intents_with_stats_for_projects_filtered(
        &config,
        &["aicx".to_string()],
        &source_filter,
    )
    .expect("missing source remains an honest extraction result");
    assert!(
        missing.stats.source_errors > 0,
        "missing CURRENT source must be counted as a coverage hole"
    );
    assert!(
        missing
            .records
            .iter()
            .all(|record| record.session_id != session_id),
        "missing source reused stale indexed claims: {:?}",
        missing.records
    );

    drop(_guard);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn indexed_bounded_projection_serves_visible_claim_but_marks_incomplete() {
    let root = unique_root("bounded-projection");
    let _guard = HomeGuard::set(&root);
    let repo = root.join("workspaces/aicx");
    fs::create_dir_all(repo.join(".git")).expect("create fixture repository");
    let session_id = "22222222-3333-4444-8555-666666666666";
    let session_dir = root.join(".codex/sessions/2026/10/06");
    fs::create_dir_all(&session_dir).expect("create Codex session directory");
    let source = session_dir.join(format!("rollout-2026-10-06T00-00-00-{session_id}.jsonl"));
    let at = chrono::Utc::now() - chrono::Duration::minutes(5);
    let mut file = fs::File::create(&source).expect("create oversized rollout");
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": (at - chrono::Duration::minutes(1)).to_rfc3339(),
            "type": "session_meta",
            "payload": {"id": session_id, "cwd": repo.display().to_string()}
        })
    )
    .unwrap();
    writeln!(
        file,
        "{}",
        serde_json::json!({
            "timestamp": at.to_rfc3339(),
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{
                    "type": "input_text",
                    "text": "Decision: retain visible bounded projection claim"
                }]
            }
        })
    )
    .unwrap();
    write!(
        file,
        "{{\"timestamp\":\"{}\",\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"call_id\":\"c1\",\"output\":\"",
        at.to_rfc3339()
    )
    .unwrap();
    let padding = vec![b'x'; 1024 * 1024];
    for _ in 0..65 {
        file.write_all(&padding).unwrap();
    }
    file.write_all(b"\"}}\n").unwrap();
    file.sync_all().unwrap();
    drop(file);

    let aicx_home = root.join(".aicx");
    aicx::catalog::rebuild(&aicx_home, &root).expect("rebuild oversized catalog");
    aicx::source_index::build(&aicx_home, &[], false, true, false)
        .expect("publish bounded CURRENT");
    let extraction = extract_intents_with_stats_for_projects_filtered(
        &IntentsConfig {
            project: "aicx".to_string(),
            hours: 720,
            strict: false,
            min_confidence: None,
            kind_filter: None,
            frame_kind: Some(aicx::timeline::FrameKind::UserMsg),
            live: false,
        },
        &["aicx".to_string()],
        &IntentSourceFilter {
            agent: Some("codex".to_string()),
            ..Default::default()
        },
    )
    .expect("extract bounded CURRENT");

    assert_eq!(extraction.stats.identity_source, INDEX_IDENTITY_SOURCE);
    assert!(
        extraction
            .records
            .iter()
            .any(|record| { record.summary.contains("visible bounded projection claim") }),
        "visible bounded claim disappeared: {:?}",
        extraction.records
    );
    assert!(
        extraction.stats.source_errors > 0,
        "bounded projection must keep completeness false"
    );
    let receipt = extraction
        .selection
        .iter()
        .find(|receipt| receipt.session_id == session_id)
        .expect("bounded source receipt");
    assert!(
        receipt
            .parser_coverage
            .as_deref()
            .is_some_and(|coverage| coverage.starts_with("bounded_projection")),
        "bounded coverage receipt missing: {receipt:?}"
    );

    drop(_guard);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn maintenance_republishes_legacy_unknown_then_reparses_modern_physical_drift() {
    let root = unique_root("maintenance-proof");
    let _guard = HomeGuard::set(&root);
    let repo = root.join("workspaces/aicx");
    fs::create_dir_all(repo.join(".git")).expect("create fixture repository");
    let session_id = "33333333-4444-4555-8666-777777777777";
    let session_dir = root.join(".codex/sessions/2026/10/06");
    fs::create_dir_all(&session_dir).expect("create Codex session directory");
    let source = session_dir.join(format!("rollout-2026-10-06T00-00-00-{session_id}.jsonl"));
    let write_source = |marker: char| {
        let body = [
            serde_json::json!({
                "timestamp": "2026-10-06T00:00:00Z",
                "type": "session_meta",
                "payload": {"id": session_id, "cwd": repo.display().to_string()}
            }),
            serde_json::json!({
                "timestamp": "2026-10-06T00:01:00Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "user",
                    "content": [{
                        "type": "input_text",
                        "text": format!("Decision: retain maintenance body {marker}")
                    }]
                }
            }),
        ]
        .into_iter()
        .map(|row| format!("{row}\n"))
        .collect::<String>();
        fs::write(&source, body).expect("write maintenance source");
    };
    let aicx_home = root.join(".aicx");
    let state_path = aicx_home.join("indexed/_all/source_parse_state.v1.json");
    let current_chunk = || {
        let hybrid = aicx_home.join("indexed/_all/hybrid");
        let generation = aicx::vector_index::resolve_hybrid_generation_dir(&hybrid);
        let adapter = aicx_retrieve::TantivyAdapter::new(generation).expect("open fixture CURRENT");
        adapter
            .scan_chunks(adapter.doc_count, |metadata| {
                metadata
                    .get("session_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(session_id)
            })
            .expect("scan fixture CURRENT")
            .into_iter()
            .next()
            .expect("fixture CURRENT chunk")
    };
    let read_record = || {
        let state: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&state_path).expect("read parse ledger"))
                .expect("parse ledger json");
        state["sessions"]
            .as_object()
            .and_then(|sessions| sessions.values().next())
            .cloned()
            .expect("single parse-ledger record")
    };

    write_source('A');
    aicx::catalog::rebuild(&aicx_home, &root).expect("rebuild maintenance catalog");
    aicx::source_index::build(&aicx_home, &[], false, true, false)
        .expect("publish initial modern CURRENT");

    let query = IntentsConfig {
        project: "aicx".to_string(),
        hours: 100_000,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: Some(aicx::timeline::FrameKind::UserMsg),
        live: false,
    };
    let fast_filter = IntentSourceFilter {
        agent: Some("codex".to_string()),
        full_source_scan: false,
        ..Default::default()
    };
    let initial_ledger = fs::read(&state_path).expect("read initial ledger bytes");
    let mut missing_ledger: serde_json::Value =
        serde_json::from_slice(&initial_ledger).expect("parse initial ledger for missing-row test");
    missing_ledger["sessions"]
        .as_object_mut()
        .expect("ledger sessions object")
        .clear();
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&missing_ledger).expect("serialize missing-row ledger"),
    )
    .expect("write ledger without CURRENT row");
    let missing = extract_intents_with_stats_for_projects_filtered(
        &query,
        &["aicx".to_string()],
        &fast_filter,
    )
    .expect("bounded query with CURRENT row missing from ledger");
    assert!(
        missing.records.is_empty(),
        "a cold CURRENT row without ledger proof must not trigger a raw history parse"
    );
    assert_eq!(missing.stats.legacy_scope_unproven, 1);
    assert!(
        missing
            .selection
            .iter()
            .any(|row| { row.session_id == session_id && row.status == "legacy_scope_unproven" })
    );
    fs::write(&state_path, initial_ledger).expect("restore initial ledger");

    let mut transitional: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).expect("read initial ledger"))
            .expect("parse initial ledger");
    let transitional_record = transitional["sessions"]
        .as_object_mut()
        .and_then(|sessions| sessions.values_mut().next())
        .expect("single mutable ledger record");
    transitional_record["source_physical_identity"] = serde_json::json!([]);
    transitional_record["coverage"] = serde_json::json!("CompleteVisible");
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&transitional).expect("serialize transitional ledger"),
    )
    .expect("write transitional ledger");

    let maintenance = aicx::source_index::build(&aicx_home, &[], false, false, false)
        .expect("normal maintenance republish");
    assert_eq!(maintenance.sources_reused, 1);
    assert_eq!(maintenance.sources_parsed, 0);
    let unknown = read_record();
    assert!(
        unknown
            .get("coverage")
            .is_none_or(serde_json::Value::is_null),
        "normal maintenance must persist legacy coverage as unknown: {unknown}"
    );
    assert_eq!(
        unknown["source_physical_identity"],
        serde_json::json!([]),
        "normal maintenance must not stamp today's identity onto yesterday's body"
    );
    assert!(current_chunk().metadata["conversation_coverage"].is_null());

    let mut unproven: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&state_path).expect("read unknown ledger"))
            .expect("parse unknown ledger");
    unproven["sessions"]
        .as_object_mut()
        .and_then(|sessions| sessions.values_mut().next())
        .expect("single unknown ledger record")["scope_unattributed"] =
        serde_json::Value::Bool(true);
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&unproven).expect("serialize unproven ledger"),
    )
    .expect("write unproven legacy scope");
    let legacy_mtime = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&source).expect("legacy source metadata"),
    );
    let legacy_len = fs::metadata(&source).unwrap().len();
    write_source('B');
    assert_eq!(fs::metadata(&source).unwrap().len(), legacy_len);
    filetime::set_file_mtime(&source, legacy_mtime).expect("restore legacy source mtime");

    let fast = extract_intents_with_stats_for_projects_filtered(
        &query,
        &["aicx".to_string()],
        &fast_filter,
    )
    .expect("fast query with explicit legacy scope hole");
    assert!(
        fast.records.is_empty(),
        "legacy extract must not bypass scope"
    );
    assert!(fast.stats.source_errors > 0);
    assert_eq!(fast.stats.legacy_scope_unproven, 1);
    assert!(
        fast.selection
            .iter()
            .any(|row| { row.session_id == session_id && row.status == "legacy_scope_unproven" })
    );
    assert!(
        fast.stats
            .completeness(None, fast.records.len())
            .warnings
            .iter()
            .any(|warning| warning.contains("legacy_scope_unproven"))
    );

    // With CURRENT temporarily unavailable, the ordinary catalog lane performs
    // one checked source read and publishes the strong reader-cache proof. Put
    // CURRENT back before the next query so the legacy index branch must use
    // the non-filling cache peek rather than silently parsing the source.
    let current_pointer = aicx_home.join("indexed/_all/hybrid/CURRENT");
    let parked_pointer = aicx_home.join("indexed/_all/hybrid/CURRENT.test-parked");
    fs::rename(&current_pointer, &parked_pointer).expect("park CURRENT pointer");
    let checked_read = extract_intents_with_stats_for_projects_filtered(
        &query,
        &["aicx".to_string()],
        &fast_filter,
    );
    fs::rename(&parked_pointer, &current_pointer).expect("restore CURRENT pointer");
    let checked_read = checked_read.expect("checked catalog read warms reader cache");
    assert!(checked_read.records.iter().any(|record| {
        record.session_id == session_id && record.summary.contains("maintenance body B")
    }));

    let warm = extract_intents_with_stats_for_projects_filtered(
        &query,
        &["aicx".to_string()],
        &fast_filter,
    )
    .expect("legacy index query with strong cached scope proof");
    assert!(warm.records.iter().any(|record| {
        record.session_id == session_id && record.summary.contains("maintenance body B")
    }));
    assert_eq!(warm.stats.source_errors, 0, "{:#?}", warm.selection);
    assert_eq!(warm.stats.legacy_scope_unproven, 0);
    assert!(warm.selection.iter().any(|row| {
        row.session_id == session_id
            && row.status == "qualified"
            && row.parser_coverage.as_deref() == Some("complete_visible")
    }));
    assert!(
        !warm
            .selection
            .iter()
            .any(|row| row.status == "legacy_scope_unproven")
    );

    let exhaustive = extract_intents_with_stats_for_projects_filtered(
        &query,
        &["aicx".to_string()],
        &IntentSourceFilter {
            agent: Some("codex".to_string()),
            full_source_scan: true,
            ..Default::default()
        },
    )
    .expect("explicit full source scan");
    assert!(exhaustive.records.iter().any(|record| {
        record.session_id == session_id && record.summary.contains("maintenance body B")
    }));
    assert_eq!(exhaustive.stats.source_errors, 0);

    let upgraded = aicx::source_index::build(&aicx_home, &[], false, true, false)
        .expect("explicit full-rescan upgrade");
    assert_eq!(upgraded.sources_parsed, 1);
    let modern = read_record();
    assert_eq!(modern["coverage"], "CompleteVisible");
    assert!(
        modern["source_physical_identity"]
            .as_array()
            .is_some_and(|identity| !identity.is_empty()),
        "full-rescan must persist physical identity: {modern}"
    );

    let pinned_mtime = filetime::FileTime::from_last_modification_time(
        &fs::metadata(&source).expect("source metadata"),
    );
    let old_len = fs::metadata(&source).unwrap().len();
    write_source('C');
    assert_eq!(fs::metadata(&source).unwrap().len(), old_len);
    filetime::set_file_mtime(&source, pinned_mtime).expect("restore source mtime");
    let reparsed = aicx::source_index::build(&aicx_home, &[], false, false, false)
        .expect("maintenance after physical drift");
    assert_eq!(reparsed.sources_parsed, 1);
    assert_eq!(reparsed.sources_reused, 0);
    let current = current_chunk();
    assert!(current.text.contains("maintenance body C"));
    assert!(!current.text.contains("maintenance body B"));

    drop(_guard);
    let _ = fs::remove_dir_all(root);
}
