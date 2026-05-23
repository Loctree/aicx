use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use super::core::Phase;

/// Background heartbeat that ticks `phase` on a fixed cadence until
/// dropped (or [`Heartbeat::stop`] is called). Used to keep the operator
/// surface alive during opaque sub-calls — large source scans, slow
/// segmentation passes, or any phase where inline per-item progress is
/// inconvenient to thread through. The tick payload is the heartbeat
/// counter, which keeps the [`StructuredReporter`] throttle happy
/// (every-2s baseline) and rotates the [`TerminalReporter`] spinner so a
/// healthy run never appears stalled.
///
/// The internal `floor` lets callers raise the heartbeat tick value when
/// real progress lands (so the spinner doesn't regress to a tiny counter
/// after a meaningful jump).
pub struct Heartbeat {
    handle: Option<JoinHandle<()>>,
    stop: Arc<AtomicBool>,
    floor: Arc<std::sync::atomic::AtomicU64>,
}

impl Heartbeat {
    /// Spawn a heartbeat that ticks every `interval`. `interval` is
    /// clamped to at least 250ms so a stray zero from a caller doesn't
    /// hot-spin a thread.
    pub fn spawn(phase: Phase, interval: Duration) -> Self {
        // Constant-interval variant is a degenerate backoff where the
        // upper bound equals the initial value. Delegating keeps the
        // backoff loop the single source of truth.
        Self::spawn_with_backoff(phase, interval, interval)
    }

    /// Spawn a heartbeat that ticks on an exponential-backoff schedule:
    /// `initial`, `2*initial`, `4*initial`, ..., capped at `max`. Use
    /// this for long opaque phases (multi-minute segmentation, large
    /// extract scans) where a constant 2s tick floods the structured
    /// log without telling the operator anything new past the first few
    /// ticks. Initial ticks land fast so operators see the phase came
    /// alive; later ticks settle to the cap so a 20-minute phase emits
    /// ~20 heartbeat lines instead of ~600.
    ///
    /// `initial` is clamped to ≥250ms (anti hot-spin) and `max` is
    /// clamped to ≥`initial` (so callers can't accidentally invert
    /// the schedule).
    pub fn spawn_with_backoff(phase: Phase, initial: Duration, max: Duration) -> Self {
        let initial = initial.max(Duration::from_millis(250));
        let max = max.max(initial);
        let stop = Arc::new(AtomicBool::new(false));
        let floor = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let stop_clone = stop.clone();
        let floor_clone = floor.clone();
        let phase_clone = phase.clone();
        let handle = thread::spawn(move || {
            let mut count: u64 = 0;
            let mut interval = initial;
            while !stop_clone.load(Ordering::Relaxed) {
                thread::sleep(interval);
                if stop_clone.load(Ordering::Relaxed) {
                    break;
                }
                count = count.saturating_add(1);
                let f = floor_clone.load(Ordering::Relaxed);
                let value = count.max(f);
                phase_clone.tick(value);
                // Double the interval for the next sleep, capped at `max`.
                interval = interval.checked_mul(2).unwrap_or(max).min(max);
            }
        });
        Self {
            handle: Some(handle),
            stop,
            floor,
        }
    }

    /// Raise the floor for the next heartbeat tick. Use this when real
    /// progress lands (e.g. an agent's extract just returned N entries)
    /// so the next emitted heartbeat tick reflects accumulated work
    /// instead of regressing to the raw heartbeat count.
    pub fn raise_floor(&self, value: u64) {
        let prev = self.floor.load(Ordering::Relaxed);
        if value > prev {
            self.floor.store(value, Ordering::Relaxed);
        }
    }

    /// Stop the heartbeat eagerly and join the thread. Equivalent to
    /// dropping, but explicit at the call site.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}
