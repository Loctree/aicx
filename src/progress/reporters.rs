use std::io::{self, IsTerminal};
use std::sync::Arc;

use super::core::{Phase, PhaseOutcome, Reporter};
use super::structured::StructuredReporter;
use super::terminal::TerminalReporter;

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
