use std::collections::HashMap;
use std::io::{self, Write};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::core::{Phase, PhaseOutcome, Reporter};

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

    pub(super) fn should_emit_tick(&self, phase: &Phase, current: u64) -> bool {
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
