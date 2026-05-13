//! Progress observability for the aicx store pipeline.
//!
//! Long-running phases (extract / chunk / steer_sync / bm25_sync) emit a
//! `Phase` event at start, optional `tick` updates, and a final `finish`
//! carrying the outcome and elapsed time. Output is routed through a
//! [`Reporter`] impl chosen once at subcommand entry by [`select_reporter`]:
//!
//! * [`TerminalReporter`] — compact `\r`-rewrite line for interactive TTY.
//! * [`StructuredReporter`] — one `[aicx][phase=...]` marker per event,
//!   line-buffered, used for JSON-emit / non-TTY runs and downstream
//!   parsers (the wizard TUI will consume the same surface).
//! * [`NoopReporter`] — silent, used by callers that don't want
//!   instrumentation (existing public API shims keep this).
//!
//! Failures recorded via [`FailureLog::record`] surface in a tail block
//! with a recovery hint and turn the subcommand exit code non-zero so the
//! operator's shell prompt visibly flips.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2024-2026 VetCoders

use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Clone, Debug)]
pub enum PhaseOutcome {
    Ok {
        elapsed_ms: u64,
        summary: String,
    },
    Failed {
        elapsed_ms: u64,
        error: String,
        recovery_hint: Option<String>,
    },
}

impl PhaseOutcome {
    pub fn elapsed_ms(&self) -> u64 {
        match self {
            PhaseOutcome::Ok { elapsed_ms, .. } | PhaseOutcome::Failed { elapsed_ms, .. } => {
                *elapsed_ms
            }
        }
    }

    pub fn is_ok(&self) -> bool {
        matches!(self, PhaseOutcome::Ok { .. })
    }
}

pub trait Reporter: Send + Sync {
    fn phase_start(&self, phase: &Phase);
    fn phase_tick(&self, phase: &Phase, current: u64);
    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome);
}

#[derive(Clone)]
pub struct Phase {
    pub name: &'static str,
    pub started_at: Instant,
    pub total: Option<u64>,
    reporter: Arc<dyn Reporter>,
}

impl Phase {
    pub fn start(reporter: Arc<dyn Reporter>, name: &'static str, total: Option<u64>) -> Self {
        let phase = Self {
            name,
            started_at: Instant::now(),
            total,
            reporter,
        };
        phase.reporter.phase_start(&phase);
        phase
    }

    pub fn tick(&self, current: u64) {
        self.reporter.phase_tick(self, current);
    }

    pub fn finish_ok(self, summary: impl Into<String>) -> PhaseOutcome {
        let outcome = PhaseOutcome::Ok {
            elapsed_ms: self.started_at.elapsed().as_millis() as u64,
            summary: summary.into(),
        };
        self.reporter.phase_finish(&self, &outcome);
        outcome
    }

    pub fn finish_err(
        self,
        error: impl std::fmt::Display,
        recovery_hint: Option<&'static str>,
    ) -> FailureRecord {
        let elapsed_ms = self.started_at.elapsed().as_millis() as u64;
        let outcome = PhaseOutcome::Failed {
            elapsed_ms,
            error: error.to_string(),
            recovery_hint: recovery_hint.map(str::to_string),
        };
        self.reporter.phase_finish(&self, &outcome);
        FailureRecord {
            phase: self.name,
            elapsed_ms,
            error: error.to_string(),
            recovery_hint: recovery_hint.map(str::to_string),
        }
    }
}

/// Default recovery hint for a known phase. Returns `None` for unknown
/// phase names so callers can decide whether to fall back to a generic
/// hint or omit the line entirely.
pub fn recovery_hint_for(phase: &str) -> Option<&'static str> {
    match phase {
        "steer_sync" | "bm25_sync" => Some("aicx doctor --fix"),
        "extract" | "chunk" => Some("aicx store --full-rescan"),
        _ => None,
    }
}

/// Single recorded failure. Crops up in the tail block and informs the
/// non-zero exit code.
#[derive(Clone, Debug)]
pub struct FailureRecord {
    pub phase: &'static str,
    pub elapsed_ms: u64,
    pub error: String,
    pub recovery_hint: Option<String>,
}

/// Thread-safe failure buffer shared across pipeline phases.
#[derive(Clone, Default)]
pub struct FailureLog {
    inner: Arc<Mutex<Vec<FailureRecord>>>,
}

impl FailureLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record(&self, record: FailureRecord) {
        if let Ok(mut guard) = self.inner.lock() {
            guard.push(record);
        }
    }

    pub fn is_empty(&self) -> bool {
        self.inner
            .lock()
            .map(|guard| guard.is_empty())
            .unwrap_or(true)
    }

    pub fn snapshot(&self) -> Vec<FailureRecord> {
        self.inner
            .lock()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }
}

/// Render the failure tail block to `stderr` and return whether any
/// failures were present (so the caller can choose the exit code).
pub fn render_failure_tail(log: &FailureLog) -> bool {
    let records = log.snapshot();
    if records.is_empty() {
        return false;
    }
    let mut err = io::stderr().lock();
    let _ = writeln!(err, "─────────────────────────────────");
    for record in &records {
        let secs = record.elapsed_ms as f64 / 1000.0;
        let _ = writeln!(err, "✗ {} FAILED after {:.1}s", record.phase, secs);
        let _ = writeln!(err, "  cause: {}", record.error);
        let _ = writeln!(err, "  impact: {}", impact_for(record.phase));
        if let Some(hint) = &record.recovery_hint {
            let _ = writeln!(err, "  recover: {hint}");
        }
    }
    let _ = writeln!(err, "─────────────────────────────────");
    let _ = err.flush();
    true
}

fn impact_for(phase: &str) -> &'static str {
    match phase {
        "steer_sync" => "search/steer return STALE data until index is rebuilt",
        "bm25_sync" => "BM25 candidate set incomplete; semantic fallback still serves results",
        "extract" => "no entries collected for this run; store left at previous watermark",
        "chunk" => "canonical corpus not updated; downstream indexes unchanged",
        _ => "downstream readers may see stale or partial data",
    }
}

/// Choose the reporter based on whether stderr is a TTY and whether the
/// caller asked for structured (`json` / non-interactive) output.
pub fn select_reporter(structured: bool) -> Arc<dyn Reporter> {
    if !structured && io::stderr().is_terminal() {
        Arc::new(TerminalReporter::new())
    } else {
        Arc::new(StructuredReporter::new())
    }
}

/// No-op reporter for callers that don't want instrumentation.
#[derive(Default)]
pub struct NoopReporter;

impl Reporter for NoopReporter {
    fn phase_start(&self, _phase: &Phase) {}
    fn phase_tick(&self, _phase: &Phase, _current: u64) {}
    fn phase_finish(&self, _phase: &Phase, _outcome: &PhaseOutcome) {}
}

/// Compact terminal reporter with a fixed three-line status surface:
/// phase spinner, progress bar, and one stable detail line. This keeps
/// long corpus runs readable while still leaving the final summary as
/// normal append-only log text.
pub struct TerminalReporter {
    state: Mutex<TerminalState>,
}

#[derive(Default)]
struct TerminalState {
    lines: usize,
    frame: usize,
}

impl TerminalReporter {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(TerminalState::default()),
        }
    }

    fn paint(&self, phase: &Phase, current: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let lines = terminal_status_lines(phase, current, state.frame);
        state.frame = state.frame.wrapping_add(1);

        let mut err = io::stderr().lock();
        if state.lines > 0 {
            let _ = write!(err, "\x1b[{}A", state.lines);
        }
        for line in &lines {
            let _ = writeln!(err, "\r\x1b[2K{line}");
        }
        state.lines = lines.len();
        let _ = err.flush();
    }

    fn clear(&self) {
        let mut err = io::stderr().lock();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.lines > 0 {
            let _ = write!(err, "\x1b[{}A", state.lines);
            for _ in 0..state.lines {
                let _ = writeln!(err, "\r\x1b[2K");
            }
            let _ = err.flush();
            state.lines = 0;
        }
    }
}

impl Default for TerminalReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for TerminalReporter {
    fn phase_start(&self, phase: &Phase) {
        self.paint(phase, 0);
    }

    fn phase_tick(&self, phase: &Phase, current: u64) {
        self.paint(phase, current);
    }

    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome) {
        self.clear();
        let mut err = io::stderr().lock();
        match outcome {
            PhaseOutcome::Ok {
                elapsed_ms,
                summary,
            } => {
                let secs = *elapsed_ms as f64 / 1000.0;
                if summary.is_empty() {
                    let _ = writeln!(err, "  ✓ {} ({:.1}s)", phase.name, secs);
                } else {
                    let _ = writeln!(err, "  ✓ {} ({:.1}s) — {summary}", phase.name, secs);
                }
            }
            PhaseOutcome::Failed {
                elapsed_ms, error, ..
            } => {
                let secs = *elapsed_ms as f64 / 1000.0;
                let _ = writeln!(err, "  ✗ {} ({:.1}s) — {error}", phase.name, secs);
            }
        }
        let _ = err.flush();
    }
}

fn terminal_status_lines(phase: &Phase, current: u64, frame: usize) -> [String; 3] {
    let spinner = ["|", "/", "-", "\\"][frame % 4];
    let elapsed = phase.started_at.elapsed().as_secs_f64();
    let title = format!("  aicx {spinner} {}", phase_label(phase.name));

    let progress = match phase.total {
        Some(total) if total > 0 => {
            let ratio = (current as f64 / total as f64).clamp(0.0, 1.0);
            let pct = (ratio * 100.0).round() as u64;
            let filled = (ratio * 32.0).round() as usize;
            let bar = format!("{}{}", "#".repeat(filled), "-".repeat(32 - filled));
            let eta = if current > 0 && current < total {
                let per_unit = elapsed / current as f64;
                format!(" | ETA {:.0}s", per_unit * (total - current) as f64)
            } else {
                String::new()
            };
            format!(
                "  [{bar}] {current}/{total} {pct:>3}% | {:.1}s{eta}",
                elapsed
            )
        }
        _ => format!("  processed {current} | {:.1}s", elapsed),
    };

    let detail = format!("  log: {}", phase_detail(phase.name));
    [title, progress, detail]
}

fn phase_label(phase: &str) -> &'static str {
    match phase {
        "extract" => "extracting sources",
        "chunk" => "chunking canonical corpus",
        "steer_sync" => "syncing steer index",
        "bm25_sync" => "syncing BM25 index",
        _ => "working",
    }
}

fn phase_detail(phase: &str) -> &'static str {
    match phase {
        "extract" => "reading agent stores; source counts print after scan",
        "chunk" => "writing canonical markdown chunks; final buckets print below",
        "steer_sync" => "refreshing metadata retrieval index",
        "bm25_sync" => "refreshing lexical candidate index",
        _ => "progress is live; final summary prints below",
    }
}

/// One-line marker per event. Stable enough for downstream parsers (the
/// wizard TUI will consume this surface unchanged) and free of `\r`
/// rewrites that confuse non-TTY consumers. Dense ticks are throttled so
/// captured logs stay readable during large corpus runs.
pub struct StructuredReporter {
    tick_state: Mutex<HashMap<&'static str, StructuredTickState>>,
}

#[derive(Clone, Copy)]
struct StructuredTickState {
    last_emit: Instant,
    last_bucket: u64,
}

impl StructuredReporter {
    pub fn new() -> Self {
        Self {
            tick_state: Mutex::new(HashMap::new()),
        }
    }

    fn should_emit_tick(&self, phase: &Phase, current: u64) -> bool {
        const MIN_INTERVAL: Duration = Duration::from_secs(2);
        const PERCENT_BUCKET: u64 = 10;

        let now = Instant::now();
        let bucket = phase
            .total
            .filter(|total| *total > 0)
            .map(|total| ((current.saturating_mul(100)) / total) / PERCENT_BUCKET)
            .unwrap_or(current / 100);
        let is_terminal_tick = phase.total.is_some_and(|total| current >= total);

        let mut guard = self.tick_state.lock().unwrap_or_else(|e| e.into_inner());
        let Some(state) = guard.get_mut(phase.name) else {
            guard.insert(
                phase.name,
                StructuredTickState {
                    last_emit: now,
                    last_bucket: bucket,
                },
            );
            return true;
        };

        if is_terminal_tick
            || bucket > state.last_bucket
            || now.duration_since(state.last_emit) >= MIN_INTERVAL
        {
            state.last_emit = now;
            state.last_bucket = bucket;
            true
        } else {
            false
        }
    }
}

impl Default for StructuredReporter {
    fn default() -> Self {
        Self::new()
    }
}

impl Reporter for StructuredReporter {
    fn phase_start(&self, phase: &Phase) {
        if let Ok(mut guard) = self.tick_state.lock() {
            guard.remove(phase.name);
        }
        let total = phase
            .total
            .map(|t| format!(" total={t}"))
            .unwrap_or_default();
        let mut err = io::stderr().lock();
        let _ = writeln!(err, "[aicx][phase={} event=start{total}]", phase.name);
        let _ = err.flush();
    }

    fn phase_tick(&self, phase: &Phase, current: u64) {
        if !self.should_emit_tick(phase, current) {
            return;
        }
        let elapsed_ms = phase.started_at.elapsed().as_millis() as u64;
        let total = phase
            .total
            .map(|t| format!(" total={t}"))
            .unwrap_or_default();
        let mut err = io::stderr().lock();
        let _ = writeln!(
            err,
            "[aicx][phase={} event=tick elapsed_ms={elapsed_ms} current={current}{total}]",
            phase.name
        );
        let _ = err.flush();
    }

    fn phase_finish(&self, phase: &Phase, outcome: &PhaseOutcome) {
        if let Ok(mut guard) = self.tick_state.lock() {
            guard.remove(phase.name);
        }
        let mut err = io::stderr().lock();
        match outcome {
            PhaseOutcome::Ok {
                elapsed_ms,
                summary,
            } => {
                let _ = writeln!(
                    err,
                    "[aicx][phase={} event=finish status=ok elapsed_ms={elapsed_ms} summary={:?}]",
                    phase.name, summary
                );
            }
            PhaseOutcome::Failed {
                elapsed_ms,
                error,
                recovery_hint,
            } => {
                let hint = recovery_hint
                    .as_deref()
                    .map(|h| format!(" recover={h:?}"))
                    .unwrap_or_default();
                let _ = writeln!(
                    err,
                    "[aicx][phase={} event=finish status=failed elapsed_ms={elapsed_ms} error={:?}{hint}]",
                    phase.name, error
                );
            }
        }
        let _ = err.flush();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        let record = phase.finish_err("disk full", Some("aicx doctor --fix"));

        assert_eq!(record.phase, "bm25_sync");
        assert_eq!(record.error, "disk full");
        assert_eq!(record.recovery_hint.as_deref(), Some("aicx doctor --fix"));
    }

    #[test]
    fn failure_log_is_thread_safe_and_collects_records() {
        let log = FailureLog::new();
        assert!(log.is_empty());
        log.record(FailureRecord {
            phase: "steer_sync",
            elapsed_ms: 12,
            error: "boom".into(),
            recovery_hint: Some("aicx doctor --fix".into()),
        });
        let snap = log.snapshot();
        assert!(!log.is_empty());
        assert_eq!(snap.len(), 1);
        assert_eq!(snap[0].phase, "steer_sync");
    }

    #[test]
    fn recovery_hint_for_known_phases() {
        assert_eq!(recovery_hint_for("steer_sync"), Some("aicx doctor --fix"));
        assert_eq!(recovery_hint_for("bm25_sync"), Some("aicx doctor --fix"));
        assert_eq!(
            recovery_hint_for("extract"),
            Some("aicx store --full-rescan")
        );
        assert_eq!(recovery_hint_for("chunk"), Some("aicx store --full-rescan"));
        assert_eq!(recovery_hint_for("unknown"), None);
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
            recovery_hint: Some("aicx doctor --fix".into()),
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
}
