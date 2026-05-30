use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use indicatif::{ProgressBar, ProgressStyle};

// ─────────────────────────────────────────────────────────────────────
// Event sink layer
//
// A parallel observability surface that consumes richer per-step events
// (the wire-up phase binds `E = aicx_progress_contracts::IndexEvent`)
// while leaving the legacy `Reporter`/`Phase` API untouched. Sinks are
// generic over the event type `E` so this module compiles standalone
// against any future event shape; integration just plugs the concrete
// event type and a translator closure in at the call site.
//
// Three sinks ship here:
//
// * [`FanOut`]        — multi-sink dispatch, ordered.
// * [`IndicatifSink`] — TTY-aware progress bar via `indicatif`, with
//                        rate-limited stderr fallback for non-TTY runs.
// * [`TracingSink`]   — pure-`tracing::info!` line per event.
// ─────────────────────────────────────────────────────────────────────

/// Sink consuming richer pipeline events. Generic over the event type so
/// the wire-up phase can bind `E = aicx_progress_contracts::IndexEvent`
/// (or any future shape) without forcing a contracts-crate dependency on
/// this module today.
pub trait EventSink<E>: Send + Sync {
    fn on_event(&self, event: &E);
}

/// Snapshot of progress derived from an arbitrary event by the closure
/// passed to [`IndicatifSink::new`]. The closure is the translator: it
/// tells the sink how to drive the bar without coupling this module to
/// the concrete event shape.
#[derive(Clone, Debug, Default)]
pub struct ProgressUpdate {
    /// Current position. Pass through unchanged to the bar.
    pub position: u64,
    /// Optional new length. When `Some`, the bar resets its denominator
    /// (useful when total count is learned mid-stream).
    pub length: Option<u64>,
    /// Optional inline status. Rendered in the `{msg}` slot.
    pub message: Option<String>,
    /// When `true`, the bar finishes with the current message and the
    /// non-interactive fallback emits a final line regardless of the
    /// rate-limit guard.
    pub finished: bool,
}

/// Multi-sink dispatcher. Registered sinks receive `on_event` in
/// insertion order. Cheap to clone — the inner `Vec<Arc<...>>` is shared.
pub struct FanOut<E> {
    sinks: Vec<Arc<dyn EventSink<E>>>,
}

impl<E> FanOut<E> {
    pub fn new() -> Self {
        Self { sinks: Vec::new() }
    }

    pub fn push(&mut self, sink: Arc<dyn EventSink<E>>) {
        self.sinks.push(sink);
    }

    pub fn builder() -> FanOutBuilder<E> {
        FanOutBuilder { sinks: Vec::new() }
    }

    pub fn len(&self) -> usize {
        self.sinks.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sinks.is_empty()
    }
}

impl<E> Default for FanOut<E> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: Send + Sync> EventSink<E> for FanOut<E> {
    fn on_event(&self, event: &E) {
        for sink in &self.sinks {
            sink.on_event(event);
        }
    }
}

/// Fluent builder for [`FanOut`]. Order of `with` calls is the dispatch
/// order (first registered = first invoked).
pub struct FanOutBuilder<E> {
    sinks: Vec<Arc<dyn EventSink<E>>>,
}

impl<E> FanOutBuilder<E> {
    pub fn with(mut self, sink: Arc<dyn EventSink<E>>) -> Self {
        self.sinks.push(sink);
        self
    }

    pub fn build(self) -> FanOut<E> {
        FanOut { sinks: self.sinks }
    }
}

/// Translator closure: maps an event to an optional [`ProgressUpdate`].
/// Returning `None` means this event does not advance the bar (e.g. a
/// configuration-change event that other sinks consume but the bar
/// ignores).
type RenderFn<E> = Box<dyn Fn(&E) -> Option<ProgressUpdate> + Send + Sync + 'static>;

/// TTY-aware progress sink built on `indicatif::ProgressBar`. In
/// interactive mode the bar renders inline and updates per event. In
/// non-interactive mode the sink falls back to rate-limited `eprintln!`
/// at most once per second, plus a forced final line when an event
/// signals `finished`. The translator closure is the only event-shape
/// coupling: callers wire it up to whatever concrete event type the
/// pipeline emits.
pub struct IndicatifSink<E> {
    progress_bar: Option<ProgressBar>,
    render: RenderFn<E>,
    last_line_at: Mutex<Instant>,
}

impl<E> IndicatifSink<E> {
    /// Construct a new sink. `total` is the initial bar length (it can
    /// be replaced later via [`ProgressUpdate::length`]). `interactive`
    /// is typically `io::stderr().is_terminal() && !structured`. The
    /// translator closure tells the sink how to derive a
    /// [`ProgressUpdate`] from each event.
    pub fn new<F>(total: u64, interactive: bool, render: F) -> Self
    where
        F: Fn(&E) -> Option<ProgressUpdate> + Send + Sync + 'static,
    {
        let progress_bar = if interactive {
            let pb = ProgressBar::new(total);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} | {msg} | ETA {eta_precise}",
                    )
                    .expect("invalid indicatif progress template")
                    .progress_chars("#>-"),
            );
            Some(pb)
        } else {
            None
        };

        Self {
            progress_bar,
            render: Box::new(render),
            // Initialise the rate-limit anchor in the past so the first
            // non-interactive event is allowed to render immediately.
            last_line_at: Mutex::new(Instant::now() - Duration::from_secs(5)),
        }
    }

    /// Interleave an info line without breaking the bar. When the bar is
    /// present this goes through `progress_bar.println` so the spinner
    /// stays on the bottom row; otherwise it falls through to `stderr`.
    pub fn println(&self, line: &str) {
        if let Some(progress_bar) = &self.progress_bar {
            progress_bar.println(line);
        } else {
            eprintln!("{line}");
        }
    }

    /// Whether the sink rendered in interactive (progress-bar) mode.
    /// Surface for tests and for callers that want to interleave their
    /// own structured output only when the bar is absent.
    pub fn is_interactive(&self) -> bool {
        self.progress_bar.is_some()
    }
}

impl<E: Send + Sync> EventSink<E> for IndicatifSink<E> {
    fn on_event(&self, event: &E) {
        let Some(update) = (self.render)(event) else {
            return;
        };

        if let Some(progress_bar) = &self.progress_bar {
            if let Some(length) = update.length {
                progress_bar.set_length(length);
            }
            progress_bar.set_position(update.position);
            if let Some(message) = update.message.clone() {
                progress_bar.set_message(message);
            }
            if update.finished {
                let final_msg = update.message.unwrap_or_else(|| "complete".to_string());
                progress_bar.finish_with_message(final_msg);
            }
            return;
        }

        // Non-interactive fallback: rate-limit to one line per second
        // unless this event signals completion.
        let now = Instant::now();
        let mut guard = self.last_line_at.lock().unwrap_or_else(|e| e.into_inner());
        if !update.finished && now.duration_since(*guard) < Duration::from_secs(1) {
            return;
        }
        *guard = now;
        drop(guard);

        let msg = update.message.unwrap_or_default();
        match update.length {
            Some(length) => {
                eprintln!("[aicx] {}/{} {}", update.position, length, msg);
            }
            None => {
                eprintln!("[aicx] {} {}", update.position, msg);
            }
        }
    }
}

/// Thin sink that emits one `tracing::info!` per event. Useful when the
/// operator wants structured machine-readable output alongside (or
/// instead of) the progress bar. Requires `E: Debug` so the event can
/// be rendered as a field.
pub struct TracingSink;

impl TracingSink {
    pub fn new() -> Self {
        Self
    }
}

impl Default for TracingSink {
    fn default() -> Self {
        Self::new()
    }
}

impl<E: std::fmt::Debug + Send + Sync> EventSink<E> for TracingSink {
    fn on_event(&self, event: &E) {
        tracing::info!(?event, "aicx event");
    }
}
