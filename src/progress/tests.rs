use super::terminal::{phase_detail, phase_label, terminal_status_lines};
use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct TestReporter {
    events: Mutex<Vec<String>>,
    starts: AtomicUsize,
    ticks: AtomicUsize,
    finishes: AtomicUsize,
}

impl Reporter for TestReporter {
    fn phase_start(&self, phase: &Phase) {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.events
            .lock()
            .unwrap()
            .push(format!("start:{}", phase.name));
    }

    fn phase_tick(&self, phase: &Phase, current: u64) {
        self.ticks.fetch_add(1, Ordering::SeqCst);
        self.events
            .lock()
            .unwrap()
            .push(format!("tick:{}:{current}", phase.name));
    }

    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome) {
        self.finishes.fetch_add(1, Ordering::SeqCst);
        let suffix = if outcome.is_ok() { "ok" } else { "fail" };
        self.events
            .lock()
            .unwrap()
            .push(format!("finish:{}:{suffix}", phase.name));
    }
}

#[test]
fn phase_finish_ok_records_elapsed_and_emits_finish() {
    let reporter = Arc::new(TestReporter::default());
    let phase = Phase::start(reporter.clone(), "steer_sync", None);
    let outcome = phase.finish_ok("synced 42 docs");

    assert!(outcome.is_ok());
    // elapsed should be readable (>=0) and reachable through the enum.
    let _ = outcome.elapsed_ms();
    assert_eq!(reporter.starts.load(Ordering::SeqCst), 1);
    assert_eq!(reporter.finishes.load(Ordering::SeqCst), 1);
    let events = reporter.events.lock().unwrap().clone();
    assert_eq!(events, vec!["start:steer_sync", "finish:steer_sync:ok"]);
}

#[test]
fn phase_finish_err_yields_failure_record_with_hint() {
    let reporter = Arc::new(TestReporter::default());
    let phase = Phase::start(reporter, "bm25_sync", Some(10));
    phase.tick(3);
    let record = phase.finish_err("disk full", Some("aicx doctor --rebuild-steer-index"));

    assert_eq!(record.phase, "bm25_sync");
    assert_eq!(record.error, "disk full");
    assert_eq!(
        record.recovery_hint.as_deref(),
        Some("aicx doctor --rebuild-steer-index")
    );
}

#[test]
fn failure_log_is_thread_safe_and_collects_records() {
    let log = FailureLog::new();
    assert!(log.is_empty());
    log.record(FailureRecord {
        phase: "steer_sync",
        elapsed_ms: 12,
        error: "boom".into(),
        recovery_hint: Some("aicx doctor --rebuild-steer-index".into()),
    });
    let snap = log.snapshot();
    assert!(!log.is_empty());
    assert_eq!(snap.len(), 1);
    assert_eq!(snap[0].phase, "steer_sync");
}

#[test]
fn recovery_hint_for_known_phases() {
    assert_eq!(
        recovery_hint_for("steer_sync"),
        Some("aicx doctor --rebuild-steer-index")
    );
    assert_eq!(
        recovery_hint_for("bm25_sync"),
        Some("aicx doctor --rebuild-steer-index")
    );
    assert_eq!(
        recovery_hint_for("extract"),
        Some("aicx store --full-rescan")
    );
    assert_eq!(recovery_hint_for("dedup"), Some("aicx store --full-rescan"));
    assert_eq!(
        recovery_hint_for("self_echo"),
        Some("aicx store --full-rescan")
    );
    assert_eq!(
        recovery_hint_for("segment"),
        Some("aicx store --full-rescan")
    );
    assert_eq!(recovery_hint_for("chunk"), Some("aicx store --full-rescan"));
    assert_eq!(recovery_hint_for("unknown"), None);
}

#[test]
fn phase_label_and_detail_cover_pre_write_phases() {
    for phase in ["extract", "dedup", "self_echo", "segment", "chunk"] {
        assert_ne!(phase_label(phase), "working", "label for {phase}");
        assert_ne!(
            phase_detail(phase),
            "progress is live; final summary prints below",
            "detail for {phase}"
        );
    }
}

// NOTE: Heartbeat behavior tests (periodic ticks + floor pinning) live in
// `tests/store_progress_markers.rs`. Keeping them out of the lib test
// binary avoids widening parallel-test scheduling windows around the
// pre-existing shared `diagnostics::STATE` race exercised by extraction
// tests and `diagnostics::tests::*`. The integration binary runs in a separate
// process with its own global state.
#[test]
fn heartbeat_stop_joins_thread_without_panic() {
    let reporter: Arc<dyn Reporter> = Arc::new(NoopReporter);
    let phase = Phase::start(reporter, "extract", Some(1));
    let hb = Heartbeat::spawn(phase.clone(), Duration::from_millis(60_000));
    hb.stop();
    phase.finish_ok("done");
}

#[test]
fn render_failure_tail_returns_false_when_empty() {
    let log = FailureLog::new();
    assert!(!render_failure_tail(&log));
}

#[test]
fn render_failure_tail_returns_true_when_failures_present() {
    let log = FailureLog::new();
    log.record(FailureRecord {
        phase: "steer_sync",
        elapsed_ms: 250,
        error: "Lance index corrupted".into(),
        recovery_hint: Some("aicx doctor --rebuild-steer-index".into()),
    });
    assert!(render_failure_tail(&log));
}

#[test]
fn structured_reporter_does_not_panic_under_concurrent_use() {
    let reporter: Arc<dyn Reporter> = Arc::new(StructuredReporter::new());
    let mut handles = Vec::new();
    for i in 0..4u8 {
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
fn terminal_status_lines_are_fixed_three_line_surface() {
    let phase = Phase {
        name: "chunk",
        started_at: Instant::now(),
        total: Some(100),
        reporter: Arc::new(NoopReporter),
    };

    let lines = terminal_status_lines(&phase, 25, 0);

    assert_eq!(lines.len(), 3);
    assert!(lines[0].contains("chunking canonical corpus"));
    assert!(lines[1].contains("25/100"));
    assert!(lines[1].contains("25%"));
    assert!(lines[2].contains("writing canonical markdown chunks"));
}

#[test]
fn structured_reporter_throttles_dense_ticks_but_keeps_percent_buckets() {
    let reporter = StructuredReporter::new();
    let phase = Phase {
        name: "chunk",
        started_at: Instant::now(),
        total: Some(100),
        reporter: Arc::new(NoopReporter),
    };

    assert!(reporter.should_emit_tick(&phase, 1));
    assert!(!reporter.should_emit_tick(&phase, 2));
    assert!(reporter.should_emit_tick(&phase, 10));
    assert!(!reporter.should_emit_tick(&phase, 11));
    assert!(reporter.should_emit_tick(&phase, 100));
}

#[test]
fn select_reporter_returns_structured_when_forced() {
    // We don't test TTY detection (depends on host); we test the
    // "structured" forcing path which is deterministic.
    let r = select_reporter(true);
    let phase = Phase::start(r, "extract", None);
    phase.finish_ok("0 entries");
}

/// Capture sink: records the stringified event for ordering checks.
struct CaptureSink {
    label: &'static str,
    log: Arc<Mutex<Vec<String>>>,
}

impl EventSink<String> for CaptureSink {
    fn on_event(&self, event: &String) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{}:{}", self.label, event));
    }
}

#[test]
fn fan_out_dispatches_to_all_sinks_in_order() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let mut fan = FanOut::<String>::new();
    fan.push(Arc::new(CaptureSink {
        label: "a",
        log: log.clone(),
    }));
    fan.push(Arc::new(CaptureSink {
        label: "b",
        log: log.clone(),
    }));
    fan.push(Arc::new(CaptureSink {
        label: "c",
        log: log.clone(),
    }));

    fan.on_event(&"one".to_string());
    fan.on_event(&"two".to_string());

    let captured = log.lock().unwrap().clone();
    assert_eq!(
        captured,
        vec![
            "a:one".to_string(),
            "b:one".to_string(),
            "c:one".to_string(),
            "a:two".to_string(),
            "b:two".to_string(),
            "c:two".to_string(),
        ]
    );
}

#[test]
fn fan_out_builder_preserves_order() {
    let log = Arc::new(Mutex::new(Vec::<String>::new()));
    let fan = FanOut::<String>::builder()
        .with(Arc::new(CaptureSink {
            label: "first",
            log: log.clone(),
        }))
        .with(Arc::new(CaptureSink {
            label: "second",
            log: log.clone(),
        }))
        .build();

    assert_eq!(fan.len(), 2);
    fan.on_event(&"x".to_string());
    let captured = log.lock().unwrap().clone();
    assert_eq!(
        captured,
        vec!["first:x".to_string(), "second:x".to_string()]
    );
}

#[test]
fn indicatif_sink_invokes_translator_for_every_event_non_interactive() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = calls.clone();
    let sink: IndicatifSink<u64> = IndicatifSink::new(0, false, move |event| {
        calls_inner.fetch_add(1, Ordering::SeqCst);
        Some(ProgressUpdate {
            position: *event,
            length: Some(10),
            message: Some(format!("item {event}")),
            finished: false,
        })
    });

    assert!(!sink.is_interactive());
    for i in 0..5u64 {
        sink.on_event(&i);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 5);
}

#[test]
fn indicatif_sink_finished_event_completes_without_panic() {
    // Interactive=true path: ensures finish_with_message is a clean
    // exit. We can't intercept stderr here, but a panic in the bar
    // would surface immediately.
    let sink: IndicatifSink<bool> = IndicatifSink::new(3, true, |finished| {
        Some(ProgressUpdate {
            position: 3,
            length: Some(3),
            message: Some("done".to_string()),
            finished: *finished,
        })
    });
    sink.on_event(&true);
    sink.println("post-finish info line should not panic");
}

#[test]
fn indicatif_sink_non_interactive_finished_bypasses_rate_limit() {
    // Two rapid events: only the second is `finished`. The first
    // should print (rate-limit anchor starts in the past), and the
    // second should print despite being <1s later because finished
    // forces a render. We can't intercept stderr; we assert that
    // both events reach the translator and the call returns.
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = calls.clone();
    let sink: IndicatifSink<bool> = IndicatifSink::new(0, false, move |finished| {
        calls_inner.fetch_add(1, Ordering::SeqCst);
        Some(ProgressUpdate {
            position: 1,
            length: None,
            message: None,
            finished: *finished,
        })
    });
    sink.on_event(&false);
    sink.on_event(&true);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn indicatif_sink_translator_returning_none_is_a_noop() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = calls.clone();
    let sink: IndicatifSink<u64> = IndicatifSink::new(0, false, move |_event| {
        calls_inner.fetch_add(1, Ordering::SeqCst);
        None
    });
    sink.on_event(&7);
    sink.on_event(&42);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    // No panic, no stderr churn — just translator invocation.
}

#[test]
fn tracing_sink_consumes_events_without_panic() {
    let sink = TracingSink::new();
    EventSink::<String>::on_event(&sink, &"hello".to_string());
    EventSink::<u64>::on_event(&sink, &42u64);
}

#[test]
fn fan_out_can_mix_indicatif_and_tracing_sinks() {
    let calls = Arc::new(AtomicUsize::new(0));
    let calls_inner = calls.clone();
    let indicatif: Arc<dyn EventSink<u64>> = Arc::new(IndicatifSink::new(0, false, move |e| {
        calls_inner.fetch_add(1, Ordering::SeqCst);
        Some(ProgressUpdate {
            position: *e,
            length: None,
            message: None,
            finished: false,
        })
    }));
    let tracing_sink: Arc<dyn EventSink<u64>> = Arc::new(TracingSink::new());
    let fan = FanOut::<u64>::builder()
        .with(indicatif)
        .with(tracing_sink)
        .build();

    fan.on_event(&1);
    fan.on_event(&2);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(fan.len(), 2);
}
