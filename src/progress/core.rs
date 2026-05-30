use std::sync::Arc;
use std::time::Instant;

use super::failures::FailureRecord;

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
    pub(super) reporter: Arc<dyn Reporter>,
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
        "extract" | "dedup" | "self_echo" | "segment" | "chunk" => Some("aicx store --full-rescan"),
        _ => None,
    }
}
