// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Mixed-workstream fail-closed surface (cut A+B).
//!
//! Real-shaped Codex rollout: the session baseline (`session_meta` + every
//! `turn_context`) stays in repo `vista`, but one whole turn window runs its
//! executable tool calls with an explicit `workdir` into repo `fleet-bus`
//! (shape of session 01a040db-60b7), and another turn window carries
//! conflicting workdirs from two real repos. The fail-closed contract:
//!
//! 1. `-p vista` returns only positively Vista-scoped frames — the whole
//!    fleet window (including the message BEFORE the first tool call) and the
//!    conflicted window are absent;
//! 2. the session is reported as a mixed-scope candidate;
//! 3. a homogeneous control session keeps the legacy bucket inheritance.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use aicx::api::Aicx;
use aicx::intents::{IntentExtraction, IntentsConfig};

// HOME is process-global; serialize the two env-dependent tests in this file.
static HOME_LOCK: Mutex<()> = Mutex::new(());

fn unique_root(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "aicx-mixed-scope-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock after unix epoch")
            .as_nanos()
    ))
}

fn make_repo(root: &Path, name: &str) -> PathBuf {
    let repo = root.join("workspaces").join(name);
    fs::create_dir_all(repo.join(".git")).expect("repo .git dir");
    repo
}

fn rollout_path(root: &Path, filename: &str) -> PathBuf {
    let dir = root
        .join(".codex")
        .join("sessions")
        .join("2026")
        .join("01")
        .join("01");
    fs::create_dir_all(&dir).expect("codex sessions dir");
    dir.join(filename)
}

fn write_mixed_rollout(root: &Path, vista: &Path, fleet: &Path, other: &Path) -> String {
    let session_id = "11111111-2222-3333-4444-555555555555";
    let template = r#"{"timestamp":"2026-01-01T00:00:00Z","type":"session_meta","payload":{"id":"@SID@","cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T00:01:00Z","type":"turn_context","payload":{"cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T00:01:10Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Pracujemy nad vista.\nDecision: preserve vista opening decision"}]}}
{"timestamp":"2026-01-01T00:02:00Z","type":"turn_context","payload":{"cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T00:02:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Biore fleet task.\nDecision: preserve fleet bus decision"}]}}
{"timestamp":"2026-01-01T00:02:10Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c1","input":"const r = await tools.exec_command({cmd:\"npm test\",\"workdir\":\"@FLEET@\"});"}}
{"timestamp":"2026-01-01T00:03:00Z","type":"turn_context","payload":{"cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T00:03:05Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c2","input":"const a = await tools.exec_command({cmd:\"ls\",\"workdir\":\"@FLEET@\"});"}}
{"timestamp":"2026-01-01T00:03:10Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c3","input":"const b = await tools.exec_command({cmd:\"ls\",\"workdir\":\"@OTHER@\"});"}}
{"timestamp":"2026-01-01T00:03:20Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Sprzeczne dowody.\nDecision: preserve conflicted turn decision"}]}}
"#;
    let body = template
        .replace("@SID@", session_id)
        .replace("@VISTA@", &vista.display().to_string())
        .replace("@FLEET@", &fleet.display().to_string())
        .replace("@OTHER@", &other.display().to_string());
    let path = rollout_path(
        root,
        &format!("rollout-2026-01-01T00-00-00-{session_id}.jsonl"),
    );
    fs::write(path, body).expect("write mixed rollout");
    session_id.to_string()
}

fn write_homogeneous_rollout(root: &Path, vista: &Path) -> String {
    let session_id = "99999999-8888-7777-6666-555555555555";
    let template = r#"{"timestamp":"2026-01-01T01:00:00Z","type":"session_meta","payload":{"id":"@SID@","cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T01:01:00Z","type":"turn_context","payload":{"cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T01:01:10Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Czysta sesja vista.\nDecision: preserve homogeneous vista control decision"}]}}
"#;
    let body = template
        .replace("@SID@", session_id)
        .replace("@VISTA@", &vista.display().to_string());
    let path = rollout_path(
        root,
        &format!("rollout-2026-01-01T01-00-00-{session_id}.jsonl"),
    );
    fs::write(path, body).expect("write homogeneous rollout");
    session_id.to_string()
}

struct HomeGuard<'a> {
    _lock: MutexGuard<'a, ()>,
    previous_home: Option<String>,
    previous_aicx: Option<String>,
}

impl<'a> HomeGuard<'a> {
    fn set(root: &Path) -> Self {
        let lock = HOME_LOCK.lock().expect("home lock");
        let previous_home = std::env::var("HOME").ok();
        let previous_aicx = std::env::var("AICX_HOME").ok();
        unsafe {
            std::env::set_var("HOME", root);
            std::env::remove_var("AICX_HOME");
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

fn vista_config(frame_kind: aicx::timeline::FrameKind) -> IntentsConfig {
    IntentsConfig {
        project: "vista".to_string(),
        hours: 0,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: Some(frame_kind),
        live: false,
    }
}

fn extract(root: &Path, frame_kind: aicx::timeline::FrameKind) -> IntentExtraction {
    let aicx_home = root.join(".aicx");
    Aicx::with_aicx_home(&aicx_home)
        .extract_intents(&vista_config(frame_kind))
        .expect("extract intents through public API")
}

#[test]
fn mixed_session_fleet_turns_never_leak_into_vista_intents() {
    let root = unique_root("mixed");
    let _guard = HomeGuard::set(&root);
    let vista = make_repo(&root, "vista");
    let fleet = make_repo(&root, "fleet-bus");
    let other = make_repo(&root, "other-repo");
    let mixed_sid = write_mixed_rollout(&root, &vista, &fleet, &other);
    let control_sid = write_homogeneous_rollout(&root, &vista);

    let aicx_home = root.join(".aicx");
    aicx::catalog::rebuild(&aicx_home, &root).expect("rebuild catalog over fixture home");

    // The product surface runs one extraction per frame kind (user / agent).
    let user_extraction = extract(&root, aicx::timeline::FrameKind::UserMsg);
    let agent_extraction = extract(&root, aicx::timeline::FrameKind::AgentReply);
    let mixed_scope: Vec<_> = user_extraction
        .mixed_scope
        .iter()
        .chain(agent_extraction.mixed_scope.iter())
        .collect();
    let records: Vec<_> = user_extraction
        .records
        .iter()
        .chain(agent_extraction.records.iter())
        .collect();

    // The mixed session is reported as a mixed-scope candidate — even though
    // the fail-closed filter removed its whole agent-reply file from vista.
    assert!(
        mixed_scope
            .iter()
            .any(|session| session.agent == "codex" && session.session_id == mixed_sid),
        "mixed_scope must name the mixed session: {:?}",
        mixed_scope
    );
    // The homogeneous control session is not mixed.
    assert!(
        !mixed_scope
            .iter()
            .any(|session| session.session_id == control_sid),
        "homogeneous control session must not be mixed: {:?}",
        mixed_scope
    );

    let mixed_records: Vec<_> = records
        .iter()
        .filter(|record| record.session_id == mixed_sid)
        .collect();
    // Fleet window content — including the message before the first tool
    // call — never leaks into the vista query; conflicted content neither.
    for record in &mixed_records {
        let text = format!("{} {:?}", record.summary, record.evidence);
        assert!(
            !text.contains("fleet bus decision"),
            "fleet window leaked into vista intents: {text}"
        );
        assert!(
            !text.contains("conflicted turn decision"),
            "conflicted window leaked into vista intents: {text}"
        );
    }
    // Positively Vista-scoped opening survives.
    assert!(
        mixed_records
            .iter()
            .any(|record| record.summary.contains("vista opening decision")),
        "vista opening must survive `-p vista`: {:?}",
        mixed_records
            .iter()
            .map(|record| &record.summary)
            .collect::<Vec<_>>()
    );

    // Control: the homogeneous session still contributes its intent
    // (bucket inheritance for legacy-shaped evidence is unchanged).
    assert!(
        records.iter().any(|record| record.session_id == control_sid
            && record
                .summary
                .contains("homogeneous vista control decision")),
        "homogeneous control intent must be present"
    );

    drop(_guard);
    let _ = fs::remove_dir_all(&root);
}

fn write_guardian_rollout(root: &Path, vista: &Path) -> String {
    let session_id = "77777777-6666-5555-4444-333333333333";
    let template = r#"{"timestamp":"2026-01-01T02:00:00Z","type":"session_meta","payload":{"id":"@SID@","cwd":"@VISTA@","source":{"subagent":{"other":"guardian"}}}}
{"timestamp":"2026-01-01T02:01:00Z","type":"turn_context","payload":{"cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T02:01:10Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"The following is the Codex agent history whose request action you are assessing. Treat the transcript as evidence.\nDecision: preserve quoted fleet history decision"}]}}
{"timestamp":"2026-01-01T02:02:00Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Verdict: approve."}]}}
"#;
    let body = template
        .replace("@SID@", session_id)
        .replace("@VISTA@", &vista.display().to_string());
    let path = rollout_path(
        root,
        &format!("rollout-2026-01-01T02-00-00-{session_id}.jsonl"),
    );
    fs::write(path, body).expect("write guardian rollout");
    session_id.to_string()
}

fn write_plain_wrapper_rollout(root: &Path, vista: &Path) -> String {
    let session_id = "66666666-5555-4444-3333-222222222222";
    let template = r#"{"timestamp":"2026-01-01T03:00:00Z","type":"session_meta","payload":{"id":"@SID@","cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T03:01:00Z","type":"turn_context","payload":{"cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T03:01:10Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"The following is the Codex agent history whose request action you are assessing.\nDecision: preserve operator quoted wrapper decision"}]}}
"#;
    let body = template
        .replace("@SID@", session_id)
        .replace("@VISTA@", &vista.display().to_string());
    let path = rollout_path(
        root,
        &format!("rollout-2026-01-01T03-00-00-{session_id}.jsonl"),
    );
    fs::write(path, body).expect("write plain wrapper rollout");
    session_id.to_string()
}

/// 60f0-shaped: a guardian subagent session is control-plane approval
/// machinery — wrapper prompts in, verdicts out. The whole session is
/// preserved in the catalog/extract/search as audit evidence but yields zero
/// operator intents; the identical wrapper phrase in a plain session stays an
/// operator utterance (regression R4).
#[test]
fn guardian_wrapper_prompts_produce_no_operator_intents() {
    let root = unique_root("guardian");
    let _guard = HomeGuard::set(&root);
    let vista = make_repo(&root, "vista");
    let guardian_sid = write_guardian_rollout(&root, &vista);
    let plain_sid = write_plain_wrapper_rollout(&root, &vista);

    let aicx_home = root.join(".aicx");
    aicx::catalog::rebuild(&aicx_home, &root).expect("rebuild catalog over fixture home");

    let user_extraction = extract(&root, aicx::timeline::FrameKind::UserMsg);
    let agent_extraction = extract(&root, aicx::timeline::FrameKind::AgentReply);
    let records: Vec<_> = user_extraction
        .records
        .iter()
        .chain(agent_extraction.records.iter())
        .collect();

    assert!(
        !records
            .iter()
            .any(|record| record.session_id == guardian_sid),
        "guardian session is control-plane evidence and must produce no operator intents at all: {:?}",
        records
            .iter()
            .filter(|record| record.session_id == guardian_sid)
            .map(|record| &record.summary)
            .collect::<Vec<_>>()
    );
    // Regression R4 at the public surface: the same phrase in a plain
    // session is an operator utterance and its intent survives.
    assert!(
        records.iter().any(|record| record.session_id == plain_sid
            && record.summary.contains("operator quoted wrapper decision")),
        "plain-session phrase must stay an operator intent: {:?}",
        records
            .iter()
            .map(|record| &record.summary)
            .collect::<Vec<_>>()
    );

    drop(_guard);
    let _ = fs::remove_dir_all(&root);
}

/// The production read path: a committed CURRENT index plus a finite,
/// non-live window. Whole-session index chunks cannot express per-frame
/// scope, so a session flagged mixed at index build must fall back to the
/// census lane (fail-closed per frame), and guardian sessions must not
/// surface at all. `identity_source == "index-v1"` proves this run really
/// went through the index lane — the complaint class the first test (which
/// forces the census path with `hours: 0`) cannot see.
#[test]
fn index_lane_falls_back_to_census_for_mixed_and_drops_guardian() {
    let root = unique_root("indexlane");
    let _guard = HomeGuard::set(&root);
    let vista = make_repo(&root, "vista");
    let fleet = make_repo(&root, "fleet-bus");
    let other = make_repo(&root, "other-repo");
    let mixed_sid = write_mixed_rollout(&root, &vista, &fleet, &other);
    let control_sid = write_homogeneous_rollout(&root, &vista);
    let guardian_sid = write_guardian_rollout(&root, &vista);

    let aicx_home = root.join(".aicx");
    aicx::catalog::rebuild(&aicx_home, &root).expect("rebuild catalog over fixture home");
    aicx::source_index::build(&aicx_home, &[], false, true, false)
        .expect("publish CURRENT index over fixture home");

    let config = |frame_kind: aicx::timeline::FrameKind| IntentsConfig {
        project: "vista".to_string(),
        // Finite window covering the 2026-01 fixture dates; not 0 (would
        // disable the index lane) and not ≤48h (would trigger live scan).
        hours: 100_000,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: Some(frame_kind),
        live: false,
    };
    let user_extraction = Aicx::with_aicx_home(&aicx_home)
        .extract_intents(&config(aicx::timeline::FrameKind::UserMsg))
        .expect("extract user intents through public API");
    let agent_extraction = Aicx::with_aicx_home(&aicx_home)
        .extract_intents(&config(aicx::timeline::FrameKind::AgentReply))
        .expect("extract agent intents through public API");

    // The committed index served this run — the production lane under test.
    assert_eq!(
        user_extraction.stats.identity_source, "index-v1",
        "test must exercise the index lane, not the census fallback"
    );

    let records: Vec<_> = user_extraction
        .records
        .iter()
        .chain(agent_extraction.records.iter())
        .collect();

    // Guardian sessions are control-plane evidence: zero operator intents
    // through the index lane as well.
    assert!(
        !records
            .iter()
            .any(|record| record.session_id == guardian_sid),
        "guardian session leaked through the index lane: {:?}",
        records
            .iter()
            .filter(|record| record.session_id == guardian_sid)
            .map(|record| &record.summary)
            .collect::<Vec<_>>()
    );

    // The mixed session is re-sourced through the census lane: fleet and
    // conflicted windows stay out of `-p vista`, the vista opening survives.
    let mixed_records: Vec<_> = records
        .iter()
        .filter(|record| record.session_id == mixed_sid)
        .collect();
    for record in &mixed_records {
        let text = format!("{} {:?}", record.summary, record.evidence);
        assert!(
            !text.contains("fleet bus decision"),
            "fleet window leaked through the index lane: {text}"
        );
        assert!(
            !text.contains("conflicted turn decision"),
            "conflicted window leaked through the index lane: {text}"
        );
    }
    assert!(
        mixed_records
            .iter()
            .any(|record| record.summary.contains("vista opening decision")),
        "vista opening must survive the census fallback: {:?}",
        mixed_records
            .iter()
            .map(|record| &record.summary)
            .collect::<Vec<_>>()
    );

    // The census fallback reports the mixed session, the homogeneous
    // control keeps bucket inheritance.
    let mixed_scope: Vec<_> = user_extraction
        .mixed_scope
        .iter()
        .chain(agent_extraction.mixed_scope.iter())
        .collect();
    assert!(
        mixed_scope
            .iter()
            .any(|session| session.agent == "codex" && session.session_id == mixed_sid),
        "mixed_scope must name the mixed session through the index lane: {:?}",
        mixed_scope
    );
    assert!(
        records.iter().any(|record| record.session_id == control_sid
            && record
                .summary
                .contains("homogeneous vista control decision")),
        "homogeneous control intent must be present through the index lane"
    );

    drop(_guard);
    let _ = fs::remove_dir_all(&root);
}

fn write_unresolved_foreign_rollout(root: &Path, vista: &Path) -> String {
    let session_id = "55555555-4444-3333-2222-111111111111";
    let template = r#"{"timestamp":"2026-01-01T04:00:00Z","type":"session_meta","payload":{"id":"@SID@","cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T04:01:00Z","type":"turn_context","payload":{"cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T04:01:10Z","type":"response_item","payload":{"type":"message","role":"user","content":[{"type":"input_text","text":"Otwieramy viste.\nDecision: preserve vista baseline opening"}]}}
{"timestamp":"2026-01-01T04:02:00Z","type":"turn_context","payload":{"cwd":"@VISTA@"}}
{"timestamp":"2026-01-01T04:02:05Z","type":"response_item","payload":{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Pisze z unresolved workdir.\nDecision: preserve unresolved foreign turn"}]}}
{"timestamp":"2026-01-01T04:02:10Z","type":"response_item","payload":{"type":"custom_tool_call","name":"exec","call_id":"c9","input":"const r = await tools.exec_command({cmd:\"npm test\",\"workdir\":\"/definitely/missing/fleet-bus\"});"}}
"#;
    let body = template
        .replace("@SID@", session_id)
        .replace("@VISTA@", &vista.display().to_string());
    let path = rollout_path(
        root,
        &format!("rollout-2026-01-01T04-00-00-{session_id}.jsonl"),
    );
    fs::write(path, body).expect("write unresolved foreign rollout");
    session_id.to_string()
}

/// Follow-up regression: `turn_context` baseline is vista, but one turn
/// window's explicit workdir is an unresolvable foreign path
/// (`/definitely/missing/fleet-bus`). That window is durably unattributed —
/// it must NOT inherit the vista bucket, in either read lane, and the
/// session must not be over-convicted as mixed either.
#[test]
fn unresolved_foreign_window_inherits_nothing_in_any_lane() {
    let root = unique_root("unresolved");
    let _guard = HomeGuard::set(&root);
    let vista = make_repo(&root, "vista");
    let unresolved_sid = write_unresolved_foreign_rollout(&root, &vista);

    let aicx_home = root.join(".aicx");
    aicx::catalog::rebuild(&aicx_home, &root).expect("rebuild catalog over fixture home");
    aicx::source_index::build(&aicx_home, &[], false, true, false)
        .expect("publish CURRENT index over fixture home");

    let config = |hours: u64, frame_kind: aicx::timeline::FrameKind| IntentsConfig {
        project: "vista".to_string(),
        hours,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: Some(frame_kind),
        live: false,
    };
    let run = |hours: u64| {
        let user = Aicx::with_aicx_home(&aicx_home)
            .extract_intents(&config(hours, aicx::timeline::FrameKind::UserMsg))
            .expect("extract user intents");
        let agent = Aicx::with_aicx_home(&aicx_home)
            .extract_intents(&config(hours, aicx::timeline::FrameKind::AgentReply))
            .expect("extract agent intents");
        (user, agent)
    };

    for (lane, hours) in [("census", 0), ("index", 100_000)] {
        let (user_extraction, agent_extraction) = run(hours);
        if lane == "index" {
            assert_eq!(
                user_extraction.stats.identity_source, "index-v1",
                "index lane must serve the finite-window run"
            );
        }
        let records: Vec<_> = user_extraction
            .records
            .iter()
            .chain(agent_extraction.records.iter())
            .collect();
        let session_records: Vec<_> = records
            .iter()
            .filter(|record| record.session_id == unresolved_sid)
            .collect();
        assert!(
            !session_records
                .iter()
                .any(|record| record.summary.contains("unresolved foreign turn")),
            "{lane} lane: unattributed window must not inherit the vista bucket: {:?}",
            session_records
                .iter()
                .map(|record| &record.summary)
                .collect::<Vec<_>>()
        );
        assert!(
            session_records
                .iter()
                .any(|record| record.summary.contains("vista baseline opening")),
            "{lane} lane: the positively vista-scoped window must survive: {:?}",
            session_records
                .iter()
                .map(|record| &record.summary)
                .collect::<Vec<_>>()
        );
        let mixed_scope: Vec<_> = user_extraction
            .mixed_scope
            .iter()
            .chain(agent_extraction.mixed_scope.iter())
            .collect();
        assert!(
            !mixed_scope
                .iter()
                .any(|session| session.session_id == unresolved_sid),
            "{lane} lane: unattributed evidence must not convict the session as mixed: {:?}",
            mixed_scope
        );
    }

    drop(_guard);
    let _ = fs::remove_dir_all(&root);
}
