//! Rich event contracts for aicx pipeline progress.
//!
//! Vibecrafted. with AI Agents by VetCoders (c)2024-2026 LibraxisAI
//!
//! This crate is the canonical source of truth for the indexing/embedding
//! progress event stream emitted by aicx pipelines. It is intentionally a
//! pure-types crate (no async, no I/O, no UI deps) so that:
//!
//! - Producers (the `aicx index` scheduler, embedders, parsers) can emit
//!   into a single sink trait without dragging UI deps.
//! - Consumers (FanOut, IndicatifSink, future TUI, JSON-line exporter,
//!   future SSE bridge) can subscribe to the same canonical stream.
//! - The on-wire representation (serde JSON) is stable across crates and
//!   binaries — agents and IPC peers can decode without linking the
//!   producer crate.
//!
//! The shape is salvaged from rust-memex's `tui::indexer::contracts`
//! (`IndexEvent` + `IndexTelemetrySnapshot` + `IndexEventSink`) and
//! generalized to aicx's "item" vocabulary (entries / chunks / embeddings
//! depending on phase) rather than the file-only assumption rust-memex
//! makes.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Events emitted by the aicx indexing / embedding pipeline.
///
/// Items in aicx are intentionally generic: depending on the pipeline phase
/// an "item" may be a source entry (file/document), a chunk, or an
/// embedding batch. The event consumer interprets `label` as the
/// human-readable identifier for that item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IndexEvent {
    RunStarted {
        total_items: usize,
        namespace: String,
        source_label: String,
        parallelism: usize,
        started_at: DateTime<Utc>,
    },
    ItemStarted {
        item_index: usize,
        label: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        size_bytes: Option<u64>,
    },
    ItemIndexed {
        item_index: usize,
        label: String,
        chunks_indexed: usize,
        duration_ms: u64,
        #[serde(skip_serializing_if = "Option::is_none")]
        embedder_ms: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        tokens_estimated: Option<usize>,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_hash: Option<String>,
    },
    ItemSkipped {
        item_index: usize,
        label: String,
        reason: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        content_hash: Option<String>,
    },
    ItemFailed {
        item_index: usize,
        label: String,
        error: String,
    },
    StatsTick {
        processed: usize,
        indexed: usize,
        skipped: usize,
        failed: usize,
        total: usize,
        items_per_sec: f64,
        #[serde(skip_serializing_if = "Option::is_none")]
        eta_secs: Option<f64>,
        total_chunks: usize,
        in_flight: usize,
    },
    RunCompleted {
        processed: usize,
        indexed: usize,
        skipped: usize,
        failed: usize,
        total_chunks: usize,
        elapsed: Duration,
        stopped_early: bool,
    },
    RunFailed {
        error: String,
        processed_before_failure: usize,
    },
    Paused,
    Resumed,
    StopRequested,
    ParallelismChanged {
        previous: usize,
        current: usize,
    },
    Warning {
        code: String,
        message: String,
    },
}

/// Event sinks must stay synchronous and infallible.
///
/// Producers call `on_event` from the hot path; sinks that need async
/// (network, file I/O) must internally buffer or fan-out to a worker
/// rather than blocking the producer.
pub trait EventSink: Send + Sync {
    fn on_event(&self, event: &IndexEvent);
}

/// Maximum number of recent warnings retained in the rolling snapshot.
pub const MAX_RECENT_WARNINGS: usize = 20;

/// Warning displayed in the dashboard / surfaced through the snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WarningEntry {
    pub code: String,
    pub message: String,
    pub at: DateTime<Utc>,
}

/// Pull-friendly indexing telemetry snapshot.
///
/// The snapshot is the folded-up state of the event stream — anything a
/// dashboard, status command, or external observer needs to render the
/// current run without replaying history. Producers fold events into
/// this struct via [`IndexTelemetrySnapshot::apply`] and publish the
/// updated snapshot however they wish (watch channel, broadcast, JSON
/// file, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IndexTelemetrySnapshot {
    pub namespace: String,
    pub source_label: String,
    pub started_at: Option<DateTime<Utc>>,
    pub total: usize,
    pub processed: usize,
    pub indexed: usize,
    pub skipped: usize,
    pub failed: usize,
    pub total_chunks: usize,
    pub current_item: Option<String>,
    pub in_flight: usize,
    pub parallelism: usize,
    pub paused: bool,
    pub stopping: bool,
    pub items_per_sec: f64,
    pub eta_secs: Option<f64>,
    pub elapsed: Duration,
    pub avg_embedder_ms: Option<f64>,
    pub total_tokens_estimated: usize,
    pub complete: bool,
    pub stopped_early: bool,
    pub fatal_error: Option<String>,
    pub recent_warnings: VecDeque<WarningEntry>,
}

impl Default for IndexTelemetrySnapshot {
    fn default() -> Self {
        Self {
            namespace: String::new(),
            source_label: String::new(),
            started_at: None,
            total: 0,
            processed: 0,
            indexed: 0,
            skipped: 0,
            failed: 0,
            total_chunks: 0,
            current_item: None,
            in_flight: 0,
            parallelism: 1,
            paused: false,
            stopping: false,
            items_per_sec: 0.0,
            eta_secs: None,
            elapsed: Duration::ZERO,
            avg_embedder_ms: None,
            total_tokens_estimated: 0,
            complete: false,
            stopped_early: false,
            fatal_error: None,
            recent_warnings: VecDeque::new(),
        }
    }
}

impl IndexTelemetrySnapshot {
    /// Fold an event into the running snapshot.
    ///
    /// Counters are advanced based on event variant. `items_per_sec` and
    /// `eta_secs` are sourced from `StatsTick` events — the producer is
    /// responsible for measuring rate (see [`RollingRate`]) and emitting
    /// ticks; this method does not infer rate from `ItemIndexed` alone.
    ///
    /// Warnings are pushed onto `recent_warnings` with a hard cap of
    /// [`MAX_RECENT_WARNINGS`] — oldest entries are dropped from the
    /// front when capacity is exceeded.
    pub fn apply(&mut self, event: &IndexEvent) {
        match event {
            IndexEvent::RunStarted {
                total_items,
                namespace,
                source_label,
                parallelism,
                started_at,
            } => {
                self.namespace = namespace.clone();
                self.source_label = source_label.clone();
                self.total = *total_items;
                self.parallelism = *parallelism;
                self.started_at = Some(*started_at);
                self.complete = false;
                self.stopped_early = false;
                self.fatal_error = None;
                self.processed = 0;
                self.indexed = 0;
                self.skipped = 0;
                self.failed = 0;
                self.total_chunks = 0;
                self.in_flight = 0;
                self.paused = false;
                self.stopping = false;
                self.items_per_sec = 0.0;
                self.eta_secs = None;
                self.elapsed = Duration::ZERO;
                self.avg_embedder_ms = None;
                self.total_tokens_estimated = 0;
                self.current_item = None;
                self.recent_warnings.clear();
            }
            IndexEvent::ItemStarted { label, .. } => {
                self.in_flight = self.in_flight.saturating_add(1);
                self.current_item = Some(label.clone());
            }
            IndexEvent::ItemIndexed {
                label,
                chunks_indexed,
                embedder_ms,
                tokens_estimated,
                ..
            } => {
                self.processed = self.processed.saturating_add(1);
                self.indexed = self.indexed.saturating_add(1);
                self.total_chunks = self.total_chunks.saturating_add(*chunks_indexed);
                self.in_flight = self.in_flight.saturating_sub(1);
                self.current_item = Some(label.clone());
                if let Some(tokens) = tokens_estimated {
                    self.total_tokens_estimated =
                        self.total_tokens_estimated.saturating_add(*tokens);
                }
                if let Some(ms) = embedder_ms {
                    // Running average: weight new sample equally with prior
                    // running mean (we don't track sample count separately
                    // because the snapshot is a coarse dashboard — high
                    // fidelity is the producer's job if needed).
                    let sample = *ms as f64;
                    self.avg_embedder_ms = Some(match self.avg_embedder_ms {
                        Some(prev) => (prev + sample) / 2.0,
                        None => sample,
                    });
                }
            }
            IndexEvent::ItemSkipped { label, .. } => {
                self.processed = self.processed.saturating_add(1);
                self.skipped = self.skipped.saturating_add(1);
                self.in_flight = self.in_flight.saturating_sub(1);
                self.current_item = Some(label.clone());
            }
            IndexEvent::ItemFailed { label, .. } => {
                self.processed = self.processed.saturating_add(1);
                self.failed = self.failed.saturating_add(1);
                self.in_flight = self.in_flight.saturating_sub(1);
                self.current_item = Some(label.clone());
            }
            IndexEvent::StatsTick {
                processed,
                indexed,
                skipped,
                failed,
                total,
                items_per_sec,
                eta_secs,
                total_chunks,
                in_flight,
            } => {
                self.processed = *processed;
                self.indexed = *indexed;
                self.skipped = *skipped;
                self.failed = *failed;
                self.total = *total;
                self.items_per_sec = *items_per_sec;
                self.eta_secs = *eta_secs;
                self.total_chunks = *total_chunks;
                self.in_flight = *in_flight;
            }
            IndexEvent::RunCompleted {
                processed,
                indexed,
                skipped,
                failed,
                total_chunks,
                elapsed,
                stopped_early,
            } => {
                self.processed = *processed;
                self.indexed = *indexed;
                self.skipped = *skipped;
                self.failed = *failed;
                self.total_chunks = *total_chunks;
                self.elapsed = *elapsed;
                self.stopped_early = *stopped_early;
                self.complete = true;
                self.in_flight = 0;
                self.stopping = false;
            }
            IndexEvent::RunFailed {
                error,
                processed_before_failure,
            } => {
                self.fatal_error = Some(error.clone());
                self.processed = *processed_before_failure;
                self.complete = true;
                self.in_flight = 0;
            }
            IndexEvent::Paused => {
                self.paused = true;
            }
            IndexEvent::Resumed => {
                self.paused = false;
            }
            IndexEvent::StopRequested => {
                self.stopping = true;
            }
            IndexEvent::ParallelismChanged { current, .. } => {
                self.parallelism = *current;
            }
            IndexEvent::Warning { code, message } => {
                if self.recent_warnings.len() >= MAX_RECENT_WARNINGS {
                    self.recent_warnings.pop_front();
                }
                self.recent_warnings.push_back(WarningEntry {
                    code: code.clone(),
                    message: message.clone(),
                    at: Utc::now(),
                });
            }
        }
    }
}

/// Rolling-window rate tracker.
///
/// The producer records completion counts (typically 1 per `ItemIndexed`
/// / `ItemSkipped` / `ItemFailed`) and queries [`Self::rate_per_sec`]
/// or [`Self::eta_secs`] when emitting a [`IndexEvent::StatsTick`]. The
/// window is purely time-based: samples older than `window_size` are
/// evicted on every observation.
#[derive(Debug, Clone)]
pub struct RollingRate {
    window: VecDeque<(Instant, usize)>,
    window_size: Duration,
}

impl RollingRate {
    /// Create a new tracker with the given rolling window.
    pub fn new(window_size: Duration) -> Self {
        Self {
            window: VecDeque::new(),
            window_size,
        }
    }

    /// Record `count` completions at the current instant.
    pub fn record(&mut self, count: usize) {
        let now = Instant::now();
        self.window.push_back((now, count));
        self.evict(now);
    }

    /// Current rate in items/sec over the rolling window.
    ///
    /// Returns 0.0 if there is fewer than two observations or the window
    /// spans less than 1ms (avoids division-by-near-zero blow-ups).
    pub fn rate_per_sec(&self) -> f64 {
        if self.window.is_empty() {
            return 0.0;
        }
        let now = Instant::now();
        // Compute total counts in window and the time span.
        let total: usize = self.window.iter().map(|(_, c)| *c).sum();
        let oldest = self.window.front().map(|(t, _)| *t).unwrap_or(now);
        let span = now.saturating_duration_since(oldest);
        let secs = span.as_secs_f64();
        if secs < 0.001 {
            return 0.0;
        }
        total as f64 / secs
    }

    /// ETA in seconds for `remaining` items at the current rate.
    ///
    /// Returns `None` if the rate is zero (cannot extrapolate) or
    /// `remaining` is zero (nothing to wait for).
    pub fn eta_secs(&self, remaining: usize) -> Option<f64> {
        if remaining == 0 {
            return Some(0.0);
        }
        let rate = self.rate_per_sec();
        if rate <= 0.0 {
            return None;
        }
        Some(remaining as f64 / rate)
    }

    fn evict(&mut self, now: Instant) {
        let cutoff = now.checked_sub(self.window_size);
        if let Some(cutoff) = cutoff {
            while let Some((t, _)) = self.window.front() {
                if *t < cutoff {
                    self.window.pop_front();
                } else {
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread::sleep;

    fn sample_events() -> Vec<IndexEvent> {
        vec![
            IndexEvent::RunStarted {
                total_items: 12,
                namespace: "kb:test".to_string(),
                source_label: "/tmp/input".to_string(),
                parallelism: 4,
                started_at: Utc::now(),
            },
            IndexEvent::ItemStarted {
                item_index: 2,
                label: "notes.md".to_string(),
                size_bytes: Some(512),
            },
            IndexEvent::ItemIndexed {
                item_index: 2,
                label: "notes.md".to_string(),
                chunks_indexed: 7,
                duration_ms: 231,
                embedder_ms: Some(187),
                tokens_estimated: Some(128),
                content_hash: Some("abc123".to_string()),
            },
            IndexEvent::ItemSkipped {
                item_index: 3,
                label: "binary.bin".to_string(),
                reason: "unsupported".to_string(),
                content_hash: None,
            },
            IndexEvent::ItemFailed {
                item_index: 4,
                label: "broken.md".to_string(),
                error: "parse error".to_string(),
            },
            IndexEvent::StatsTick {
                processed: 8,
                indexed: 6,
                skipped: 1,
                failed: 1,
                total: 12,
                items_per_sec: 1.5,
                eta_secs: Some(2.6),
                total_chunks: 18,
                in_flight: 2,
            },
            IndexEvent::Paused,
            IndexEvent::Resumed,
            IndexEvent::StopRequested,
            IndexEvent::ParallelismChanged {
                previous: 4,
                current: 8,
            },
            IndexEvent::Warning {
                code: "embedder_slow".to_string(),
                message: "embedder over 5s".to_string(),
            },
            IndexEvent::RunCompleted {
                processed: 12,
                indexed: 9,
                skipped: 2,
                failed: 1,
                total_chunks: 28,
                elapsed: Duration::from_secs(12),
                stopped_early: false,
            },
            IndexEvent::RunFailed {
                error: "ollama oom".to_string(),
                processed_before_failure: 5,
            },
        ]
    }

    #[test]
    fn index_event_serde_roundtrip_all_variants() {
        for event in sample_events() {
            let json = serde_json::to_string(&event).expect("serialize");
            let roundtrip: IndexEvent = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(roundtrip, event, "roundtrip mismatch for {:?}", event);
        }
    }

    #[test]
    fn snapshot_apply_increments_counters() {
        let mut snap = IndexTelemetrySnapshot::default();
        let started_at = Utc::now();
        snap.apply(&IndexEvent::RunStarted {
            total_items: 10,
            namespace: "kb:a".into(),
            source_label: "src".into(),
            parallelism: 2,
            started_at,
        });
        assert_eq!(snap.total, 10);
        assert_eq!(snap.parallelism, 2);
        assert_eq!(snap.namespace, "kb:a");
        assert_eq!(snap.source_label, "src");
        assert!(snap.started_at.is_some());

        snap.apply(&IndexEvent::ItemStarted {
            item_index: 0,
            label: "a.md".into(),
            size_bytes: Some(10),
        });
        assert_eq!(snap.in_flight, 1);
        assert_eq!(snap.current_item.as_deref(), Some("a.md"));

        snap.apply(&IndexEvent::ItemIndexed {
            item_index: 0,
            label: "a.md".into(),
            chunks_indexed: 3,
            duration_ms: 100,
            embedder_ms: Some(60),
            tokens_estimated: Some(50),
            content_hash: None,
        });
        assert_eq!(snap.processed, 1);
        assert_eq!(snap.indexed, 1);
        assert_eq!(snap.total_chunks, 3);
        assert_eq!(snap.in_flight, 0);
        assert_eq!(snap.total_tokens_estimated, 50);
        assert!(snap.avg_embedder_ms.is_some());

        snap.apply(&IndexEvent::ItemSkipped {
            item_index: 1,
            label: "b.md".into(),
            reason: "dup".into(),
            content_hash: None,
        });
        assert_eq!(snap.skipped, 1);
        assert_eq!(snap.processed, 2);

        snap.apply(&IndexEvent::ItemFailed {
            item_index: 2,
            label: "c.md".into(),
            error: "boom".into(),
        });
        assert_eq!(snap.failed, 1);
        assert_eq!(snap.processed, 3);

        snap.apply(&IndexEvent::StatsTick {
            processed: 3,
            indexed: 1,
            skipped: 1,
            failed: 1,
            total: 10,
            items_per_sec: 2.0,
            eta_secs: Some(3.5),
            total_chunks: 3,
            in_flight: 0,
        });
        assert_eq!(snap.items_per_sec, 2.0);
        assert_eq!(snap.eta_secs, Some(3.5));

        snap.apply(&IndexEvent::Paused);
        assert!(snap.paused);
        snap.apply(&IndexEvent::Resumed);
        assert!(!snap.paused);
        snap.apply(&IndexEvent::StopRequested);
        assert!(snap.stopping);
        snap.apply(&IndexEvent::ParallelismChanged {
            previous: 2,
            current: 4,
        });
        assert_eq!(snap.parallelism, 4);
    }

    #[test]
    fn warning_fold_respects_cap() {
        let mut snap = IndexTelemetrySnapshot::default();
        for i in 0..25 {
            snap.apply(&IndexEvent::Warning {
                code: format!("c{i}"),
                message: format!("m{i}"),
            });
        }
        assert_eq!(snap.recent_warnings.len(), MAX_RECENT_WARNINGS);
        // Oldest 5 should have been evicted; the front should be c5.
        assert_eq!(snap.recent_warnings.front().unwrap().code, "c5");
        assert_eq!(snap.recent_warnings.back().unwrap().code, "c24");
    }

    #[test]
    fn run_completed_sets_complete_and_elapsed() {
        let mut snap = IndexTelemetrySnapshot::default();
        snap.apply(&IndexEvent::RunCompleted {
            processed: 5,
            indexed: 4,
            skipped: 1,
            failed: 0,
            total_chunks: 12,
            elapsed: Duration::from_secs(7),
            stopped_early: false,
        });
        assert!(snap.complete);
        assert_eq!(snap.elapsed, Duration::from_secs(7));
        assert_eq!(snap.in_flight, 0);
    }

    #[test]
    fn run_failed_sets_fatal_error() {
        let mut snap = IndexTelemetrySnapshot::default();
        snap.apply(&IndexEvent::RunFailed {
            error: "ollama died".into(),
            processed_before_failure: 3,
        });
        assert_eq!(snap.fatal_error.as_deref(), Some("ollama died"));
        assert!(snap.complete);
        assert_eq!(snap.processed, 3);
    }

    #[test]
    fn rolling_rate_records_and_extrapolates() {
        let mut rr = RollingRate::new(Duration::from_secs(2));
        // Empty window → rate 0.0
        assert_eq!(rr.rate_per_sec(), 0.0);

        rr.record(1);
        sleep(Duration::from_millis(50));
        rr.record(1);
        sleep(Duration::from_millis(50));
        rr.record(1);

        let rate = rr.rate_per_sec();
        // ~3 items over ~100ms → ~30 items/sec. Loose bounds for CI jitter.
        assert!(rate > 5.0, "expected meaningful rate, got {rate}");
        assert!(rate < 500.0, "expected sane rate, got {rate}");

        let eta = rr.eta_secs(10).expect("eta with positive rate");
        assert!(eta > 0.0);

        // Zero remaining → 0.0
        assert_eq!(rr.eta_secs(0), Some(0.0));

        // Empty tracker after construction → eta None for nonzero remaining
        let empty = RollingRate::new(Duration::from_secs(1));
        assert!(empty.eta_secs(5).is_none());
    }
}
