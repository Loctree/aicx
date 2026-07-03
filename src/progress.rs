//! Progress observability for the aicx store pipeline.
//!
//! Long-running phases (extract / chunk / steer_sync / bm25_sync) emit a
//! `Phase` event at start, optional `tick` updates, and a final `finish`
//! carrying the outcome and elapsed time. Output is routed through a
//! [`Reporter`] impl chosen once at subcommand entry by [`select_reporter`].
//!
//! The module is intentionally split by responsibility: phase lifecycle,
//! failure reporting, heartbeat threads, terminal/structured renderers, and
//! the richer generic event-sink layer used by index progress contracts.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2024-2026 Vetcoders

mod core;
mod events;
mod failures;
mod heartbeat;
mod reporters;
mod structured;
mod terminal;

pub use core::{Phase, PhaseOutcome, Reporter, recovery_hint_for};
pub use events::{EventSink, FanOut, FanOutBuilder, IndicatifSink, ProgressUpdate, TracingSink};
pub use failures::{FailureLog, FailureRecord, render_failure_tail};
pub use heartbeat::Heartbeat;
pub use reporters::{NoopReporter, select_reporter};
pub use structured::StructuredReporter;
pub use terminal::TerminalReporter;

#[cfg(test)]
mod tests;
