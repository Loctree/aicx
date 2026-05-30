use std::io::{self, Write};
use std::sync::Mutex;

use super::core::{Phase, PhaseOutcome, Reporter};

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

pub(super) fn terminal_status_lines(phase: &Phase, current: u64, frame: usize) -> [String; 3] {
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

pub(super) fn phase_label(phase: &str) -> &'static str {
    match phase {
        "extract" => "extracting sources",
        "dedup" => "deduplicating entries",
        "self_echo" => "filtering self-echo entries",
        "segment" => "building semantic segments",
        "chunk" => "chunking canonical corpus",
        "steer_sync" => "syncing steer index",
        "bm25_sync" => "syncing BM25 index",
        _ => "working",
    }
}

pub(super) fn phase_detail(phase: &str) -> &'static str {
    match phase {
        "extract" => "reading agent stores; source counts print after scan",
        "dedup" => "comparing entries against persisted seen-hashes",
        "self_echo" => "stripping aicx tool-echo entries that would feed back into the corpus",
        "segment" => "grouping entries into repo-scoped sessions before any write",
        "chunk" => "writing canonical markdown chunks; final buckets print below",
        "steer_sync" => "refreshing metadata retrieval index",
        "bm25_sync" => "refreshing lexical candidate index",
        _ => "progress is live; final summary prints below",
    }
}
