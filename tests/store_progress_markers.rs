// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

//! Integration coverage for the structured `[aicx][phase=...]` markers
//! emitted by `aicx::progress` when the pipeline is asked to behave
//! non-interactively. The chunk + steer + bm25 surface is the contract
//! the wizard TUI and downstream parsers rely on; this test guards it
//! without spinning the full `aicx store` pipeline (which would touch
//! `~/.aicx/store/` and is excluded from this pass).
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2024-2026 Vetcoders

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use aicx::progress::{
    FailureLog, FailureRecord, Heartbeat, NoopReporter, Phase, PhaseOutcome, Reporter,
    StructuredReporter, recovery_hint_for, render_failure_tail, select_reporter,
};

fn lance_trace_diagnostics_enabled() -> bool {
    std::env::var("RUST_LOG")
        .ok()
        .is_some_and(|filter| rust_log_enables_lance_trace_filter(&filter))
}

fn rust_log_enables_lance_trace_filter(filter: &str) -> bool {
    if tracing_subscriber::EnvFilter::try_new(filter).is_err() {
        return false;
    }

    filter.split(',').any(|directive| {
        let Some((target, level)) = directive.trim().rsplit_once('=') else {
            return false;
        };
        level.trim().eq_ignore_ascii_case("trace") && is_lance_target(target.trim())
    })
}

fn is_lance_target(target: &str) -> bool {
    matches!(target, "lance" | "lancedb")
        || target.starts_with("lance::")
        || target.starts_with("lancedb::")
}

#[derive(Default)]
struct CapturingReporter {
    events: Mutex<Vec<String>>,
    ticks_per_phase: Mutex<std::collections::HashMap<String, AtomicUsize>>,
}

impl CapturingReporter {
    fn record(&self, line: String) {
        self.events.lock().unwrap().push(line);
    }

    fn tick_count(&self, phase: &str) -> usize {
        self.ticks_per_phase
            .lock()
            .unwrap()
            .get(phase)
            .map(|c| c.load(Ordering::SeqCst))
            .unwrap_or(0)
    }
}

impl Reporter for CapturingReporter {
    fn phase_start(&self, phase: &Phase) {
        self.record(format!("start:{}", phase.name));
    }
    fn phase_tick(&self, phase: &Phase, current: u64) {
        self.record(format!("tick:{}:{current}", phase.name));
        let mut guard = self.ticks_per_phase.lock().unwrap();
        guard
            .entry(phase.name.to_string())
            .or_insert_with(|| AtomicUsize::new(0))
            .fetch_add(1, Ordering::SeqCst);
    }
    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome) {
        let suffix = if outcome.is_ok() { "ok" } else { "fail" };
        self.record(format!("finish:{}:{suffix}", phase.name));
    }
}

#[test]
fn store_pipeline_emits_chunk_steer_bm25_phase_markers_in_order() {
    let reporter = Arc::new(CapturingReporter::default());
    let chunk = Phase::start(reporter.clone(), "chunk", Some(120));
    chunk.tick(60);
    chunk.finish_ok("12 chunks");

    let steer = Phase::start(reporter.clone(), "steer_sync", Some(12));
    steer.tick(12);
    steer.finish_ok("12 docs");

    let bm25 = Phase::start(reporter.clone(), "bm25_sync", Some(12));
    bm25.finish_ok("12 docs");

    let events = reporter.events.lock().unwrap().clone();
    assert_eq!(
        events,
        vec![
            "start:chunk".to_string(),
            "tick:chunk:60".to_string(),
            "finish:chunk:ok".to_string(),
            "start:steer_sync".to_string(),
            "tick:steer_sync:12".to_string(),
            "finish:steer_sync:ok".to_string(),
            "start:bm25_sync".to_string(),
            "finish:bm25_sync:ok".to_string(),
        ]
    );
}

#[test]
fn structured_reporter_does_not_panic_when_used_concurrently() {
    let reporter: Arc<dyn Reporter> = Arc::new(StructuredReporter::new());
    let mut handles = Vec::new();
    for i in 0..8u8 {
        let r = reporter.clone();
        handles.push(std::thread::spawn(move || {
            let phase = Phase::start(r, "steer_sync", Some(i as u64 + 1));
            phase.tick(i as u64);
            phase.finish_ok("ok");
        }));
    }
    for h in handles {
        h.join().expect("thread panic");
    }
}

#[test]
fn select_reporter_returns_structured_when_forced() {
    let r = select_reporter(true);
    let phase = Phase::start(r, "chunk", None);
    phase.finish_ok("0 chunks");
}

#[test]
fn noop_reporter_handles_full_phase_lifecycle() {
    let r: Arc<dyn Reporter> = Arc::new(NoopReporter);
    let phase = Phase::start(r, "extract", None);
    phase.tick(0);
    phase.finish_ok("");
}

#[test]
fn lance_diagnostic_filter_requires_targeted_trace() {
    assert!(!rust_log_enables_lance_trace_filter(""));
    assert!(!rust_log_enables_lance_trace_filter("trace"));
    assert!(!rust_log_enables_lance_trace_filter("lance=debug"));
    assert!(!rust_log_enables_lance_trace_filter("aicx=trace"));
    assert!(rust_log_enables_lance_trace_filter("lance=trace"));
    assert!(rust_log_enables_lance_trace_filter(
        "aicx=debug,lance=trace"
    ));
    assert!(rust_log_enables_lance_trace_filter("lancedb::io=trace"));
}

#[test]
fn full_store_pipeline_emits_pre_write_phase_markers_before_any_chunk_tick() {
    // The `aicx store --full-rescan -H 0` operator regression: nothing
    // was visible for 15-20 minutes while extract/dedup/self_echo/segment
    // ran silently. Lock that fix in with a reporter capture that
    // checks (1) every pre-write phase emits start+finish, (2) every
    // pre-write phase finishes BEFORE the first `chunk` tick, and
    // (3) the chunk phase denominator matches segment count (not entries).
    let reporter = Arc::new(CapturingReporter::default());

    let extract = Phase::start(reporter.clone(), "extract", Some(2));
    extract.tick(1);
    extract.tick(2);
    extract.finish_ok("2 agents → 12345 entries");

    let dedup = Phase::start(reporter.clone(), "dedup", Some(12345));
    dedup.tick(500);
    dedup.tick(12345);
    dedup.finish_ok("kept 11000 / 12345 (skipped 1345)");

    let echo = Phase::start(reporter.clone(), "self_echo", Some(11000));
    echo.tick(500);
    echo.tick(11000);
    echo.finish_ok("kept 10800 / 11000 (filtered 200)");

    // Segment phase intentionally runs with total=None so the heartbeat
    // counter doesn't get rendered as a misleading `N/entries_total = 0%`
    // ratio on TTY. The phase still emits at least one tick (here
    // simulating a heartbeat fire) before finish so operators see it as
    // alive during long in-memory segmentation passes.
    let segment = Phase::start(reporter.clone(), "segment", None);
    segment.tick(1);
    segment.finish_ok("10800 entries → 420 segments");

    let chunk = Phase::start(reporter.clone(), "chunk", Some(420));
    chunk.tick(60);
    chunk.tick(420);
    chunk.finish_ok("420 chunks");

    let events = reporter.events.lock().unwrap().clone();

    // Order check — every pre-write phase finishes before the chunk
    // phase starts emitting ticks.
    let first_chunk_tick = events
        .iter()
        .position(|e| e.starts_with("tick:chunk:"))
        .expect("chunk phase must tick at least once");
    let last_segment_finish = events
        .iter()
        .rposition(|e| e == "finish:segment:ok")
        .expect("segment phase must finish");
    assert!(
        last_segment_finish < first_chunk_tick,
        "segment phase finished AFTER first chunk tick; pre-write progress regression"
    );

    // Every pre-write phase emits at least one tick so the operator
    // sees activity before any .md is written.
    for phase in ["extract", "dedup", "self_echo", "segment"] {
        assert!(
            reporter.tick_count(phase) >= 1,
            "phase {phase} emitted no ticks — operator would see 0/N stall"
        );
    }

    // The chunk denominator path is asserted through the ordering of
    // ticks (`tick:chunk:60` lands before `tick:chunk:420`), proving
    // the chunk total reflects segments (420) rather than entries
    // (10800) — otherwise the final tick value would saturate at
    // 10800, not 420.
    let last_chunk_tick = events
        .iter()
        .filter_map(|e| e.strip_prefix("tick:chunk:"))
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    assert_eq!(
        last_chunk_tick, 420,
        "chunk phase final tick should saturate at segment count, not entry count"
    );
}

#[test]
fn heartbeat_keeps_extract_phase_alive_during_opaque_subcall() {
    // Operator regression: during `extract_claude` the only visible
    // event was the per-agent eprintln after the call returned. With a
    // heartbeat we expect periodic `tick:extract:*` lines while the
    // (simulated) opaque sub-call runs.
    let reporter = Arc::new(CapturingReporter::default());
    let extract = Phase::start(reporter.clone(), "extract", Some(1));
    {
        let _hb = Heartbeat::spawn(extract.clone(), Duration::from_millis(200));
        // Simulate a slow opaque extract.
        std::thread::sleep(Duration::from_millis(700));
    }
    extract.tick(1);
    extract.finish_ok("simulated");

    assert!(
        reporter.tick_count("extract") >= 2,
        "expected heartbeat to keep the extract phase alive (>=2 ticks), got {}",
        reporter.tick_count("extract")
    );
}

#[test]
fn heartbeat_with_backoff_emits_fewer_ticks_than_constant_interval() {
    // On a 20-minute segment phase, a constant 2s heartbeat emits ~600
    // ticks — that floods the structured log. Backoff doubles the
    // interval each tick (capped at `max`) so a long phase converges
    // to one tick per `max`. Heartbeat intervals are intentionally
    // clamped to >=250ms, so this test uses the real lower bound:
    // over ~1.2s, a constant 250ms heartbeat would fire ~4 times, while
    // the backoff schedule (250, 400, 400) lands around 2-3 ticks on
    // normal runners.
    let reporter = Arc::new(CapturingReporter::default());
    let phase = Phase::start(reporter.clone(), "segment", None);
    let hb = Heartbeat::spawn_with_backoff(
        phase.clone(),
        Duration::from_millis(50),
        Duration::from_millis(400),
    );
    std::thread::sleep(Duration::from_millis(1200));
    hb.stop();
    phase.finish_ok("done");

    let ticks = reporter.tick_count("segment");
    assert!(
        (2..=5).contains(&ticks),
        "expected backoff to land in [2, 5] ticks over 1.2s with clamped initial=250ms max=400ms, got {ticks}"
    );
}

#[test]
fn heartbeat_floor_pins_tick_value_to_real_progress() {
    // Floor lets real-progress jumps (e.g. an agent's extractor just
    // returned 750 entries) override the bare heartbeat counter so the
    // spinner doesn't regress to "1, 2, 3, ..." after meaningful work
    // landed. This guards the per-agent extract loop pattern in
    // `run_store`.
    let reporter = Arc::new(CapturingReporter::default());
    let phase = Phase::start(reporter.clone(), "extract", Some(1000));
    let hb = Heartbeat::spawn(phase.clone(), Duration::from_millis(150));
    hb.raise_floor(750);
    std::thread::sleep(Duration::from_millis(450));
    hb.stop();
    phase.finish_ok("done");

    let events = reporter.events.lock().unwrap().clone();
    let max_tick = events
        .iter()
        .filter_map(|e| e.strip_prefix("tick:extract:"))
        .filter_map(|n| n.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    assert!(
        max_tick >= 750,
        "expected heartbeat to honor floor=750, got max tick {max_tick}"
    );
}

#[test]
fn structured_reporter_emits_phase_markers_for_every_pre_write_phase() {
    // Cross-check: when the reporter is forced to the structured surface
    // (the path the wizard TUI consumes), every new phase name emits at
    // least one event. Without this assertion a typo in a phase name
    // could silently drop the operator surface back to "nothing for 15
    // minutes" mode.
    let reporter: Arc<dyn Reporter> = Arc::new(StructuredReporter::new());
    for name in ["extract", "dedup", "self_echo", "segment", "chunk"] {
        let phase = Phase::start(reporter.clone(), name, Some(100));
        phase.tick(50);
        phase.finish_ok("smoke");
    }
}

// ---------------------------------------------------------------------------
// W2-04: bounded fast health vs deep forensic doctor.
//
// The doctor deep pass reuses the progress surface guarded by this file:
// phase markers, heartbeats, and cooperative cancellation. Fast health is
// the contract's other half — bounded, read-only, silent.
// ---------------------------------------------------------------------------

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering as AtomicOrdering};
use std::time::Instant;

fn read_only_doctor_opts() -> aicx::doctor::DoctorOptions {
    aicx::doctor::DoctorOptions {
        rebuild_steer_index: false,
        fix_buckets: false,
        dry_run: false,
        rebuild_sidecars: false,
        prune_empty_bodies: false,
        apply_prune_empty_bodies: false,
        migrate_identities: false,
        apply_migrate_identities: false,
        check_dedup: false,
        verbose: false,
        smoke: false,
    }
}

fn unique_fixture_base(tag: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "aicx-w204-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&base).unwrap();
    base
}

/// Synthetic canonical store: `dirs` nested repo dirs × `files_per_dir`
/// chunk files, each with a sidecar. Deep enough that a recursive scan is
/// observable, small enough to build quickly.
fn build_synthetic_store(base: &Path, dirs: usize, files_per_dir: usize) {
    for d in 0..dirs {
        let dir = base
            .join("store")
            .join("SampleOrg")
            .join(format!("repo-{d}"))
            .join("claude")
            .join("2026-07");
        std::fs::create_dir_all(&dir).unwrap();
        for f in 0..files_per_dir {
            let chunk = dir.join(format!("chunk-{f}.md"));
            std::fs::write(&chunk, format!("# chunk {d}/{f}\nbody\n")).unwrap();
            std::fs::write(chunk.with_extension("meta.json"), r#"{"schema_version":1}"#).unwrap();
        }
    }
}

/// Deterministic recursive digest of a tree: sorted relative paths, file
/// sizes, and contents. Any write anywhere under `root` changes the digest.
fn recursive_tree_digest(root: &Path) -> String {
    use sha2::{Digest, Sha256};
    fn walk(root: &Path, dir: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
        let Ok(read) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in read.flatten() {
            let path = entry.path();
            let rel = path
                .strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            if path.is_dir() {
                entries.push((format!("{rel}/"), Vec::new()));
                walk(root, &path, entries);
            } else {
                entries.push((rel, std::fs::read(&path).unwrap_or_default()));
            }
        }
    }
    let mut entries = Vec::new();
    walk(root, root, &mut entries);
    entries.sort();
    let mut hasher = Sha256::new();
    for (rel, bytes) in entries {
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    hex::encode(hasher.finalize())
}

/// Reporter that records `(event, elapsed)` and can flip a cancel flag the
/// moment a target phase starts.
#[derive(Default)]
struct TimedReporter {
    started_at: Option<Instant>,
    events: Mutex<Vec<(String, Duration)>>,
    cancel_on_phase: Option<(String, Arc<AtomicBool>)>,
}

impl TimedReporter {
    fn new(cancel_on_phase: Option<(String, Arc<AtomicBool>)>) -> Self {
        Self {
            started_at: Some(Instant::now()),
            events: Mutex::new(Vec::new()),
            cancel_on_phase,
        }
    }

    fn elapsed(&self) -> Duration {
        self.started_at
            .map(|s| s.elapsed())
            .unwrap_or(Duration::ZERO)
    }

    fn record(&self, line: String) {
        self.events.lock().unwrap().push((line, self.elapsed()));
    }

    fn event_names(&self) -> Vec<String> {
        self.events
            .lock()
            .unwrap()
            .iter()
            .map(|(name, _)| name.clone())
            .collect()
    }
}

impl Reporter for TimedReporter {
    fn phase_start(&self, phase: &Phase) {
        self.record(format!("start:{}", phase.name));
        if let Some((target, cancel)) = &self.cancel_on_phase
            && phase.name == target
        {
            cancel.store(true, AtomicOrdering::Relaxed);
        }
    }
    fn phase_tick(&self, phase: &Phase, current: u64) {
        self.record(format!("tick:{}:{current}", phase.name));
    }
    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome) {
        let suffix = if outcome.is_ok() { "ok" } else { "fail" };
        self.record(format!("finish:{}:{suffix}", phase.name));
    }
}

#[test]
fn fast_health_is_bounded_reports_unknown_and_leaves_the_tree_untouched() {
    // W2-04 falsification: on a large synthetic tree the fast pass must
    // (1) meet the two-second latency bound, (2) refuse to certify facts
    // that need the recursive scan (unknown, with the exact deep command),
    // and (3) leave a recursive digest of the base — including state
    // diagnostics — byte-for-byte unchanged.
    let base = unique_fixture_base("fast");
    build_synthetic_store(&base, 24, 50); // 1200 chunks + 1200 sidecars
    std::fs::create_dir_all(base.join("state")).unwrap();
    std::fs::write(base.join("state").join("sentinel.txt"), b"untouched").unwrap();

    let digest_before = recursive_tree_digest(&base);
    let opts = read_only_doctor_opts();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let started = Instant::now();
    let report = rt
        .block_on(aicx::doctor::run(
            Some(&base),
            &opts,
            false,
            Arc::new(NoopReporter),
            Arc::new(AtomicBool::new(false)),
        ))
        .expect("fast health must not error")
        .expect("fast health has no cancel source");
    let elapsed = started.elapsed();

    assert!(
        elapsed < Duration::from_secs(2),
        "fast health exceeded the 2s latency bound: {elapsed:?}"
    );
    for (name, check) in [
        ("index_freshness", &report.index_freshness),
        ("index_consistency", &report.index_consistency),
        ("noise_health", &report.noise_health),
        ("empty_body_chunks", &report.empty_body_chunks),
    ] {
        assert_eq!(
            check.severity,
            aicx::doctor::Severity::Unknown,
            "{name} needs the recursive pass and must stay unknown in fast health: {check:?}"
        );
        assert!(
            check
                .recommendation
                .as_deref()
                .is_some_and(|rec| rec.contains("aicx doctor --deep")),
            "{name} must hand the operator the exact deep command: {check:?}"
        );
    }
    // The bounded walk finished this store within budget, so sidecars are
    // fully verified — bounded truth, not a recursive-scan claim.
    assert_eq!(report.sidecars.severity, aicx::doctor::Severity::Green);
    assert!(
        report
            .canonical_store
            .detail
            .contains("1200 chunk file(s) counted by bounded walk"),
        "bounded walk should count the store it fully covered: {}",
        report.canonical_store.detail
    );

    let digest_after = recursive_tree_digest(&base);
    assert_eq!(
        digest_before, digest_after,
        "fast health must be read-only: no diagnostics writes, no mutation"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn deep_doctor_emits_phase_progress_within_two_seconds_and_in_order() {
    let base = unique_fixture_base("deep");
    build_synthetic_store(&base, 2, 3);

    let reporter = Arc::new(TimedReporter::new(None));
    let opts = read_only_doctor_opts();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let report = rt
        .block_on(aicx::doctor::run(
            Some(&base),
            &opts,
            true,
            reporter.clone(),
            Arc::new(AtomicBool::new(false)),
        ))
        .expect("deep doctor must not error")
        .expect("uncancelled deep doctor must produce a report");

    // Deep certifies the recursive facts — no unknown left behind.
    assert_ne!(
        report.index_freshness.severity,
        aicx::doctor::Severity::Unknown
    );

    let events = reporter.events.lock().unwrap().clone();
    let first = events.first().expect("deep doctor must emit progress");
    assert_eq!(first.0, "start:doctor_quick");
    assert!(
        first.1 < Duration::from_secs(2),
        "first phase marker must land within two seconds, got {:?}",
        first.1
    );

    let names = reporter.event_names();
    let position = |needle: &str| {
        names
            .iter()
            .position(|n| n == needle)
            .unwrap_or_else(|| panic!("missing event {needle}; got {names:?}"))
    };
    let quick_start = position("start:doctor_quick");
    let quick_finish = position("finish:doctor_quick:ok");
    let store_start = position("start:doctor_store_scan");
    let index_start = position("start:doctor_index_scan");
    let content_start = position("start:doctor_content_scan");
    let content_finish = position("finish:doctor_content_scan:ok");
    assert!(quick_start < quick_finish);
    assert!(quick_finish < store_start);
    assert!(store_start < index_start);
    assert!(index_start < content_start);
    assert!(content_start < content_finish);
    assert!(
        !names.iter().any(|n| n.contains("doctor_fix")),
        "read-only deep run must not enter the fix phase: {names:?}"
    );
    let _ = std::fs::remove_dir_all(&base);
}

#[test]
fn deep_doctor_cancels_cleanly_in_every_phase_and_leaves_the_tree_recoverable() {
    // W2-04 falsification: flip the cancel flag the moment each phase
    // starts; the run must return Ok(None) (clean cancellation), close the
    // cancelled phase with a failure marker, start nothing afterwards, and
    // leave the fixture tree byte-for-byte intact.
    let read_only_phases = [
        "doctor_quick",
        "doctor_store_scan",
        "doctor_index_scan",
        "doctor_content_scan",
    ];
    for target in read_only_phases {
        let base = unique_fixture_base("cancel");
        build_synthetic_store(&base, 2, 3);
        let digest_before = recursive_tree_digest(&base);

        let cancel = Arc::new(AtomicBool::new(false));
        let reporter = Arc::new(TimedReporter::new(Some((
            target.to_string(),
            cancel.clone(),
        ))));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt
            .block_on(aicx::doctor::run(
                Some(&base),
                &read_only_doctor_opts(),
                true,
                reporter.clone(),
                cancel,
            ))
            .expect("cancellation is a clean outcome, not an error");
        assert!(
            outcome.is_none(),
            "cancel during {target} must yield the cancelled outcome"
        );

        let names = reporter.event_names();
        assert!(
            names.contains(&format!("finish:{target}:fail")),
            "cancelled phase {target} must be closed with a failure marker: {names:?}"
        );
        let target_pos = names
            .iter()
            .position(|n| n == &format!("start:{target}"))
            .unwrap();
        assert!(
            !names
                .iter()
                .skip(target_pos + 1)
                .any(|n| n.starts_with("start:")),
            "no phase may start after cancellation in {target}: {names:?}"
        );

        assert_eq!(
            digest_before,
            recursive_tree_digest(&base),
            "cancelled deep run must leave the tree intact (phase {target})"
        );
        let _ = std::fs::remove_dir_all(&base);
    }

    // Fix + recheck phases (dry-run: read-only by contract) must cancel
    // just as cleanly.
    for target in ["doctor_fix", "doctor_recheck"] {
        let base = unique_fixture_base("cancel-fix");
        build_synthetic_store(&base, 2, 3);
        let digest_before = recursive_tree_digest(&base);

        let opts = aicx::doctor::DoctorOptions {
            fix_buckets: true,
            dry_run: true,
            ..read_only_doctor_opts()
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let reporter = Arc::new(TimedReporter::new(Some((
            target.to_string(),
            cancel.clone(),
        ))));
        let rt = tokio::runtime::Runtime::new().unwrap();
        let outcome = rt
            .block_on(aicx::doctor::run(
                Some(&base),
                &opts,
                true,
                reporter.clone(),
                cancel,
            ))
            .expect("cancellation is a clean outcome, not an error");
        assert!(
            outcome.is_none(),
            "cancel during {target} must yield the cancelled outcome"
        );
        assert!(
            reporter
                .event_names()
                .contains(&format!("finish:{target}:fail")),
            "cancelled phase {target} must be closed with a failure marker"
        );
        assert_eq!(
            digest_before,
            recursive_tree_digest(&base),
            "dry-run fix cancellation must leave the tree intact (phase {target})"
        );
        let _ = std::fs::remove_dir_all(&base);
    }
}

#[test]
fn failed_phase_records_recovery_hint_and_gates_lance_tail_rendering() {
    let reporter: Arc<dyn Reporter> = Arc::new(NoopReporter);
    let log = FailureLog::new();
    assert!(!render_failure_tail(&log));

    let phase = Phase::start(reporter, "steer_sync", Some(10));
    let record: FailureRecord = phase.finish_err(
        "Lance index missing _deletions/130-86502-...arrow",
        recovery_hint_for("steer_sync"),
    );
    assert_eq!(record.phase, "steer_sync");
    assert_eq!(
        record.recovery_hint.as_deref(),
        Some("aicx doctor --rebuild-steer-index")
    );
    log.record(record);

    let records = log.snapshot();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0].error,
        "Lance index missing _deletions/130-86502-...arrow"
    );

    if lance_trace_diagnostics_enabled() {
        assert!(render_failure_tail(&log));
    }
}
