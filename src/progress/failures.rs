use std::io::{self, Write};
use std::sync::{Arc, Mutex};

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
        "dedup" => "dedup pass aborted before completion; rerun with --full-rescan to re-evaluate",
        "self_echo" => "self-echo filter aborted; rerun --full-rescan to retry cleanup",
        "segment" => "segmentation aborted; no semantic segments produced this run",
        "chunk" => "canonical corpus not updated; downstream indexes unchanged",
        _ => "downstream readers may see stale or partial data",
    }
}
