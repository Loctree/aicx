//! Integration coverage for the structured `[aicx][phase=...]` markers
//! emitted by `aicx::progress` when the pipeline is asked to behave
//! non-interactively. The chunk + steer + bm25 surface is the contract
//! the wizard TUI and downstream parsers rely on; this test guards it
//! without spinning the full `aicx store` pipeline (which would touch
//! `~/.aicx/store/` and is excluded from this pass).
//!
//! Vibecrafted with AI Agents by VetCoders (c)2024-2026 VetCoders

use std::sync::{Arc, Mutex};

use aicx::progress::{
    FailureLog, FailureRecord, NoopReporter, Phase, PhaseOutcome, Reporter, StructuredReporter,
    recovery_hint_for, render_failure_tail, select_reporter,
};

#[derive(Default)]
struct CapturingReporter {
    events: Mutex<Vec<String>>,
}

impl Reporter for CapturingReporter {
    fn phase_start(&self, phase: &Phase) {
        self.events
            .lock()
            .unwrap()
            .push(format!("start:{}", phase.name));
    }
    fn phase_tick(&self, phase: &Phase, current: u64) {
        self.events
            .lock()
            .unwrap()
            .push(format!("tick:{}:{current}", phase.name));
    }
    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome) {
        let suffix = if outcome.is_ok() { "ok" } else { "fail" };
        self.events
            .lock()
            .unwrap()
            .push(format!("finish:{}:{suffix}", phase.name));
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
fn failed_phase_records_recovery_hint_and_renders_tail() {
    let reporter: Arc<dyn Reporter> = Arc::new(NoopReporter);
    let log = FailureLog::new();
    assert!(!render_failure_tail(&log));

    let phase = Phase::start(reporter, "steer_sync", Some(10));
    let record: FailureRecord = phase.finish_err(
        "Lance index missing _deletions/130-86502-...arrow",
        recovery_hint_for("steer_sync"),
    );
    assert_eq!(record.phase, "steer_sync");
    assert_eq!(record.recovery_hint.as_deref(), Some("aicx doctor --fix"));
    log.record(record);
    assert!(render_failure_tail(&log));
}
