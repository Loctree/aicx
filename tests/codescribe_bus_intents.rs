// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]
//! Codescribe speech through the whole retrieval path: catalog hot refresh →
//! census intents → published CURRENT → indexed intents, for both the bus and
//! dictated takes. Synthetic fixtures only.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aicx::intents::{
    INDEX_IDENTITY_SOURCE, IntentExtraction, IntentKind, IntentRecord, IntentSourceFilter,
    IntentsConfig, extract_intents_with_stats_for_projects_filtered,
};
use chrono::{DateTime, Duration, Utc};
use serde_json::json;

const SESSION: &str = "aaaaaaaa-1111-4222-8333-444444444444";
const TAKE: &str = "agent-channel-0-bbbbbbbb-1111-4222-8333-444444444444";
const LEDGER_STEM: &str = "cccccccc-1111-4222-8333-444444444444";
const SPOKEN: &str = "Let's ship the synthetic voice lane before the review";
const PARTIAL: &str = "Let's ship the synthetic";
const TYPED: &str = "Let's wire the synthetic typed lane";
const SUMMARY_DECISION: &str = "from now on the synthetic summary decides";
const REPLY: &str = "Decision: the synthetic reply stays off the human lane";
const INSTRUCTIONS: &str = "Two things for today:\nlet's fix the synthetic terminal colors\ntask: preserve the synthetic session state";
const QUOTED_REVIEW: &str =
    "from the reviewer:\n> Decision: synthetic auto reviews stay disabled by default";
const DICTATED: &str = "Let's record the synthetic dictation lane";
const DICTATED_REWRITE: &str = "Let's record the synthetic dictation lane, neatly formatted";

struct HomeGuard {
    previous_home: Option<String>,
    previous_aicx: Option<String>,
}

impl HomeGuard {
    // This integration target has one test; no other thread reads HOME.
    fn set(root: &Path) -> Self {
        let previous_home = std::env::var("HOME").ok();
        let previous_aicx = std::env::var("AICX_HOME").ok();
        unsafe {
            std::env::set_var("HOME", root);
            std::env::set_var("AICX_HOME", root.join(".aicx"));
        }
        Self {
            previous_home,
            previous_aicx,
        }
    }
}

impl Drop for HomeGuard {
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

fn unique_root() -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-codescribe-bus-intents-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos()
    ))
}

fn jsonl(rows: &[serde_json::Value]) -> String {
    rows.iter().map(|row| format!("{row}\n")).collect()
}

fn at(base: DateTime<Utc>, seconds: i64) -> String {
    (base + Duration::seconds(seconds)).to_rfc3339()
}

/// A Claude session in the repo: one typed operator line and the compaction
/// summary the harness writes after the context ran out.
fn write_claude_session(root: &Path, repo: &Path, base: DateTime<Utc>) {
    let cwd = repo.display().to_string();
    let dir = root.join(".claude/projects").join(cwd.replace('/', "-"));
    fs::create_dir_all(&dir).expect("create claude project dir");
    let body = jsonl(&[
        json!({"type": "user", "sessionId": SESSION, "cwd": cwd, "timestamp": at(base, 0),
               "message": {"role": "user", "content": TYPED}}),
        json!({"type": "assistant", "sessionId": SESSION, "cwd": cwd, "timestamp": at(base, 5),
               "message": {"role": "assistant", "model": "claude-test",
                           "content": [{"type": "text", "text": "Wiring it."}]}}),
        json!({"type": "user", "sessionId": SESSION, "cwd": cwd, "timestamp": at(base, 60),
               "isCompactSummary": true, "compactMetadata": {"trigger": "auto"},
               "message": {"role": "user", "content": format!(
                   "This session is being continued from a previous conversation.\n\
                    Summary:\n- Decision: {SUMMARY_DECISION}")}}),
    ]);
    fs::write(dir.join(format!("{SESSION}.jsonl")), body).expect("write claude session");
}

/// The bus generation that carried one spoken take and two typed messages to
/// that session, with the receiver's spoken reply.
fn write_bus_ledger(root: &Path, base: DateTime<Utc>) -> PathBuf {
    let dir = root.join(".codescribe/agent-bridge/buses/events/2026_0102");
    fs::create_dir_all(&dir).expect("create bus events dir");
    let recipients = json!([
        {"name": "alpha", "provider": "claude-code", "provider_session_id": SESSION, "channel": "1"}
    ]);
    let evidence = |revision: i64, action: &str, text: &str, offset: i64| {
        json!({"schema": "codescribe.transcript-evidence.v1", "session_id": TAKE,
               "audience": "alpha", "reducer_revision": revision, "reducer_action": action,
               "rendered_text": text, "emitted_at": at(base, offset), "recipients": recipients})
    };
    let channel = |state: &str, offset: i64| {
        json!({"schema": "codescribe.channel-session.v1", "kind": "channel_session",
               "session_id": TAKE, "channel": "0", "state": state,
               "opened_at": at(base, 100), "emitted_at": at(base, offset)})
    };
    let typed = |text: &str, offset: i64| {
        json!({"schema": "codescribe.agent-user-message.v1", "kind": "agent_user_message",
               "source": "typed", "name": "alpha", "provider": "claude-code",
               "provider_session_id": SESSION, "text": text, "emitted_at": at(base, offset),
               "recipients": recipients})
    };
    let body = jsonl(&[
        channel("open", 100),
        evidence(1, "apply_ledger_decision", PARTIAL, 105),
        evidence(7, "seal_coverage", SPOKEN, 112),
        evidence(8, "seal_coverage", SPOKEN, 113),
        channel("sealed", 115),
        json!({"schema": "codescribe.agent-reply.v1", "kind": "agent_reply", "name": "alpha",
               "provider": "claude-code", "provider_session_id": SESSION, "text": REPLY,
               "emitted_at": at(base, 130), "recipients": recipients}),
        typed(INSTRUCTIONS, 150),
        typed(QUOTED_REVIEW, 170),
    ]);
    let ledger = dir.join(format!("{LEDGER_STEM}.compressed.jsonl"));
    fs::write(&ledger, body).expect("write bus ledger");
    ledger
}

/// One dictated take as Codescribe archives it: the raw recognizer text and a
/// formatter rewrite of the same speech.
fn write_dictated_take(root: &Path, base: DateTime<Utc>) -> PathBuf {
    let day = root
        .join(".codescribe/transcriptions")
        .join(base.format("%Y-%m-%d").to_string());
    fs::create_dir_all(&day).expect("create transcription day");
    let stem = format!("{}_synthetic-take", base.format("%H%M%S"));
    let raw = day.join(format!("{stem}_raw.txt"));
    fs::write(&raw, DICTATED).expect("write raw take");
    fs::write(day.join(format!("{stem}_ai.txt")), DICTATED_REWRITE).expect("write rewrite");
    raw
}

fn summaries(records: &[IntentRecord]) -> Vec<&str> {
    records
        .iter()
        .map(|record| record.summary.as_str())
        .collect()
}

fn containing<'a>(records: &'a [IntentRecord], needle: &str) -> Vec<&'a IntentRecord> {
    records
        .iter()
        .filter(|record| record.summary.contains(needle))
        .collect()
}

/// What every lane must agree on for the receiving project.
fn assert_project_lane(records: &[IntentRecord], lane: &str) {
    let voice = containing(records, "synthetic voice lane");
    assert_eq!(
        voice.len(),
        1,
        "{lane}: one take is one human turn, latest revision only: {:?}",
        summaries(records)
    );
    assert_eq!(voice[0].agent, "codescribe", "{lane}");
    assert_eq!(voice[0].kind, IntentKind::Intent, "{lane}");
    assert_eq!(
        voice[0].source.as_deref(),
        Some("voice_transcript"),
        "{lane}: bus speech carries voice provenance"
    );
    assert!(
        records.iter().all(|record| record.summary != PARTIAL),
        "{lane}: a superseded snapshot leaked: {:?}",
        summaries(records)
    );
    assert!(
        !containing(records, "synthetic typed lane").is_empty(),
        "{lane}: the typed operator line must still be served: {:?}",
        summaries(records)
    );
    // A multi-line typed delivery is the operator's own instructions.
    for line in [
        "synthetic terminal colors",
        "preserve the synthetic session state",
    ] {
        let found = containing(records, line);
        assert!(
            found.iter().any(|record| record.agent == "codescribe"),
            "{lane}: typed instruction `{line}` was lost: {:?}",
            summaries(records)
        );
    }
    for absent in [
        "synthetic summary decides",
        "synthetic reply stays off",
        "synthetic auto reviews",
    ] {
        assert!(
            containing(records, absent).is_empty(),
            "{lane}: `{absent}` is not fresh operator speech: {:?}",
            summaries(records)
        );
    }
}

/// Dictation lands in its own bucket and is served once per take.
fn assert_dictation_served_once(extraction: &IntentExtraction, lane: &str) {
    let dictated = containing(&extraction.records, "synthetic dictation lane");
    assert_eq!(
        dictated.len(),
        1,
        "{lane}: a take is served once: {:?}",
        summaries(&extraction.records)
    );
    assert_eq!(dictated[0].agent, "codescribe", "{lane}");
    assert_eq!(
        dictated[0].source.as_deref(),
        Some("voice_transcript"),
        "{lane}: dictation carries voice provenance"
    );
    assert!(
        containing(&extraction.records, "neatly formatted").is_empty(),
        "{lane}: a formatter rewrite is not the operator's words"
    );
}

#[test]
fn codescribe_speech_reaches_intents_through_hot_refresh_census_and_index() {
    let root = unique_root();
    let _guard = HomeGuard::set(&root);
    let repo = root.join("workspaces/demo");
    fs::create_dir_all(repo.join(".git")).expect("create fixture repository");
    let aicx_home = root.join(".aicx");
    let base = Utc::now() - Duration::hours(6);

    // The durable census exists before the operator speaks.
    write_claude_session(&root, &repo, base);
    aicx::catalog::rebuild(&aicx_home, &root).expect("rebuild fixture catalog");
    let receiver = aicx::catalog::read_entries_at(&aicx_home)
        .expect("read catalog")
        .into_iter()
        .find(|entry| entry.session_id == SESSION)
        .expect("receiving session is cataloged");

    // A hot refresh admits the new ledger, scoped like its receiver, and the
    // new dictated take, once and unscoped.
    let ledger = write_bus_ledger(&root, base);
    let raw_take = write_dictated_take(&root, base);
    let cutoff = (Utc::now() - Duration::hours(48))
        .timestamp_nanos_opt()
        .expect("cutoff in range") as u128;
    let refresh = aicx::catalog::refresh_hot(&aicx_home, &root, cutoff).expect("hot refresh");
    assert!(refresh.admitted_sessions >= 2, "{refresh:?}");
    let catalog = aicx::catalog::read_entries_at(&aicx_home).expect("read refreshed catalog");
    let bus_row = catalog
        .iter()
        .find(|entry| entry.agent == "codescribe" && entry.session_id.starts_with("bus-"))
        .expect("bus session admitted by hot refresh");
    assert_eq!(
        bus_row.session_id,
        format!("bus-{LEDGER_STEM}-claude-{SESSION}")
    );
    assert_eq!(bus_row.source_path, ledger.display().to_string());
    assert_eq!(
        bus_row.cwd, receiver.cwd,
        "bus speech inherits the receiver's cwd"
    );
    assert_eq!(
        bus_row.project, receiver.project,
        "bus speech inherits the receiver's project"
    );
    let takes: Vec<_> = catalog
        .iter()
        .filter(|entry| entry.agent == "codescribe" && !entry.session_id.starts_with("bus-"))
        .collect();
    assert_eq!(takes.len(), 1, "one catalog row per dictated take");
    assert_eq!(takes[0].source_path, raw_take.display().to_string());
    assert_eq!(
        takes[0].project.as_deref(),
        Some(aicx::importers::codescribe::CODESCRIBE_DICTATION_PROJECT),
        "dictation lands in its explicit bucket, never a guessed repo"
    );
    let project = receiver.project.clone().expect("receiver has a project");

    let scoped = IntentsConfig {
        project: project.clone(),
        hours: 72,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: Some(aicx::timeline::FrameKind::UserMsg),
        live: false,
    };
    let unscoped = IntentsConfig {
        project: String::new(),
        ..scoped.clone()
    };
    let filter = IntentSourceFilter {
        agent: None,
        date_lo: None,
        date_hi: None,
        full_source_scan: false,
    };

    let census = extract_intents_with_stats_for_projects_filtered(
        &scoped,
        std::slice::from_ref(&project),
        &filter,
    )
    .expect("census intents");
    assert_project_lane(&census.records, "census");
    let census_all = extract_intents_with_stats_for_projects_filtered(&unscoped, &[], &filter)
        .expect("unscoped census intents");
    assert_dictation_served_once(&census_all, "census");
    let dictation = aicx::importers::codescribe::CODESCRIBE_DICTATION_PROJECT.to_string();
    let dictation_only = IntentsConfig {
        project: dictation.clone(),
        ..scoped.clone()
    };
    let census_dictation =
        extract_intents_with_stats_for_projects_filtered(&dictation_only, &[dictation], &filter)
            .expect("dictation bucket intents");
    assert_dictation_served_once(&census_dictation, "census -p dictation");
    assert!(
        containing(&census_dictation.records, "synthetic voice lane").is_empty(),
        "the dictation bucket holds no bus speech"
    );

    aicx::source_index::build(&aicx_home, &[], false, true, false).expect("publish CURRENT");
    let indexed = extract_intents_with_stats_for_projects_filtered(&scoped, &[project], &filter)
        .expect("indexed intents");
    assert_eq!(
        indexed.stats.identity_source, INDEX_IDENTITY_SOURCE,
        "second read must come from the published CURRENT"
    );
    assert_project_lane(&indexed.records, "index");
    let indexed_all = extract_intents_with_stats_for_projects_filtered(&unscoped, &[], &filter)
        .expect("unscoped indexed intents");
    assert_dictation_served_once(&indexed_all, "index");

    let _ = fs::remove_dir_all(&root);
}
