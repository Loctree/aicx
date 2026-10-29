//! Latest-value preview of the canonical indexing event stream.
use std::sync::Mutex;
use std::time::Instant;

use aicx_progress_contracts::{EventSink, IndexEvent, IndexTelemetrySnapshot};
use tokio::sync::watch;

/// Folds pipeline events into a snapshot, including rate and ETA derived from
/// completed items. Sending retains state even before a dashboard subscribes.
pub struct IndexProgressMonitor {
    sender: watch::Sender<IndexTelemetrySnapshot>,
    started_at: Mutex<Option<Instant>>,
}

impl Default for IndexProgressMonitor {
    fn default() -> Self {
        let (sender, _) = watch::channel(IndexTelemetrySnapshot::default());
        Self {
            sender,
            started_at: Mutex::new(None),
        }
    }
}

impl IndexProgressMonitor {
    pub fn subscribe(&self) -> watch::Receiver<IndexTelemetrySnapshot> {
        self.sender.subscribe()
    }

    pub fn snapshot(&self) -> IndexTelemetrySnapshot {
        self.sender.borrow().clone()
    }
}

impl EventSink for IndexProgressMonitor {
    fn on_event(&self, event: &IndexEvent) {
        let mut started_at = self.started_at.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(event, IndexEvent::RunStarted { .. }) {
            *started_at = Some(Instant::now());
        }
        self.sender.send_modify(|snapshot| {
            snapshot.apply(event);
            if !snapshot.complete && !matches!(event, IndexEvent::StatsTick { .. }) {
                snapshot.elapsed = started_at.map(|start| start.elapsed()).unwrap_or_default();
                let seconds = snapshot.elapsed.as_secs_f64();
                snapshot.items_per_sec = if seconds > 0.0 {
                    snapshot.processed as f64 / seconds
                } else {
                    0.0
                };
                snapshot.eta_secs = (snapshot.items_per_sec > 0.0).then(|| {
                    snapshot.total.saturating_sub(snapshot.processed) as f64
                        / snapshot.items_per_sec
                });
            }
            if snapshot.complete {
                snapshot.current_item = None;
                snapshot.paused = false;
                snapshot.stopping = false;
                snapshot.eta_secs = if snapshot.fatal_error.is_none() && !snapshot.stopped_early {
                    Some(0.0)
                } else {
                    None
                };
                if !matches!(event, IndexEvent::RunCompleted { .. }) {
                    snapshot.elapsed = started_at.map(|start| start.elapsed()).unwrap_or_default();
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn start(monitor: &IndexProgressMonitor, total: usize) {
        monitor.on_event(&IndexEvent::RunStarted {
            total_items: total,
            namespace: "index_embed".into(),
            source_label: "test".into(),
            parallelism: 1,
            started_at: Default::default(),
        });
    }

    #[test]
    fn late_subscriber_sees_completed_work_without_counting_in_flight() {
        let monitor = IndexProgressMonitor::default();
        start(&monitor, 2);
        monitor.on_event(&IndexEvent::ItemStarted {
            item_index: 0,
            label: "a".into(),
            size_bytes: None,
        });
        assert_eq!(monitor.snapshot().processed, 0);
        assert_eq!(monitor.snapshot().eta_secs, None);
        monitor.on_event(&IndexEvent::ItemIndexed {
            item_index: 0,
            label: "a".into(),
            chunks_indexed: 1,
            duration_ms: 10,
            embedder_ms: Some(10),
            tokens_estimated: None,
            content_hash: None,
        });
        let rx = monitor.subscribe();
        assert_eq!(rx.borrow().processed, 1);
        assert_eq!(rx.borrow().in_flight, 0);
        assert!(rx.borrow().items_per_sec > 0.0);
        assert!(rx.borrow().eta_secs.unwrap() > 0.0);
    }

    #[test]
    fn failure_is_terminal_and_next_run_resets_preview() {
        let monitor = IndexProgressMonitor::default();
        start(&monitor, 2);
        monitor.on_event(&IndexEvent::ItemStarted {
            item_index: 0,
            label: "a".into(),
            size_bytes: None,
        });
        monitor.on_event(&IndexEvent::RunFailed {
            error: "embedder timeout".into(),
            processed_before_failure: 0,
        });
        let snapshot = monitor.snapshot();
        assert!(snapshot.complete);
        assert_eq!(snapshot.in_flight, 0);
        assert_eq!(snapshot.current_item, None);
        assert_eq!(snapshot.eta_secs, None);
        assert_eq!(snapshot.fatal_error.as_deref(), Some("embedder timeout"));
        start(&monitor, 3);
        assert!(!monitor.snapshot().complete);
        assert_eq!(monitor.snapshot().fatal_error, None);
        assert_eq!(monitor.snapshot().processed, 0);
    }
}
