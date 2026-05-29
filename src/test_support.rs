//! Shared test-only tracing capture.
//!
//! Replaces the per-module `tracing::subscriber::with_default` capture helpers,
//! which were subject to a rare parallel flake: with no global default
//! subscriber the process dispatcher is `NoSubscriber`, which reports
//! `Interest::never` for every callsite. When any concurrent test registers a
//! fresh callsite, tracing rebuilds the process-global interest cache against
//! the current dispatcher; on a thread that is not inside a `with_default`
//! scope that is the `NoSubscriber` default, so a target callsite gets cached
//! as disabled and a thread-local capture installed later silently misses the
//! event (e.g. `regenerate_logs_detailed_reason_without_leaking_403_body`).
//!
//! This module installs a single process-global subscriber once, at max level
//! TRACE, so every callsite is always enabled and can never be cached as
//! disabled. The subscriber routes each event to a *thread-local* buffer that
//! [`capture_logs`] swaps in for the duration of the captured closure; outside
//! a capture scope, events are discarded. Capture is therefore immune to the
//! global interest cache and to cross-thread races: each thread writes only to
//! its own buffer.

use std::cell::RefCell;
use std::io;
use std::sync::{Arc, Mutex, Once};

thread_local! {
    /// The active capture buffer for the current thread, if any.
    static CAPTURE_BUFFER: RefCell<Option<Arc<Mutex<Vec<u8>>>>> = const { RefCell::new(None) };
}

/// `MakeWriter`/`Write` that appends to the calling thread's active capture
/// buffer, or discards when none is set.
struct ThreadLocalCaptureWriter;

impl io::Write for ThreadLocalCaptureWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        CAPTURE_BUFFER.with(|cell| {
            if let Some(target) = cell.borrow().as_ref() {
                target
                    .lock()
                    .expect("capture buffer poisoned")
                    .extend_from_slice(buf);
            }
        });
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for ThreadLocalCaptureWriter {
    type Writer = ThreadLocalCaptureWriter;

    fn make_writer(&'a self) -> Self::Writer {
        ThreadLocalCaptureWriter
    }
}

fn ensure_global_capture_subscriber() {
    static INIT: Once = Once::new();
    INIT.call_once(|| {
        let subscriber = tracing_subscriber::fmt()
            .with_writer(ThreadLocalCaptureWriter)
            .with_ansi(false)
            .without_time()
            .with_max_level(tracing::Level::TRACE)
            .finish();
        // Authoritative global default for the test binary. Nothing else in the
        // unit-test build installs one; if that ever changes, capture would
        // return empty and the affected test fails loudly (never flakily).
        let _ = tracing::subscriber::set_global_default(subscriber);
    });
}

/// Capture all tracing output emitted **on the current thread** during `f`,
/// returning `f`'s result alongside the captured log text.
///
/// Deterministic under parallel `cargo test`: the global subscriber is always
/// enabled and each thread routes to its own buffer, so no interest-cache race
/// or cross-thread leak can drop or misattribute an event.
pub(crate) fn capture_logs<R>(f: impl FnOnce() -> R) -> (R, String) {
    ensure_global_capture_subscriber();

    let buffer = Arc::new(Mutex::new(Vec::new()));
    CAPTURE_BUFFER.with(|cell| *cell.borrow_mut() = Some(Arc::clone(&buffer)));
    // Restore the thread-local even if `f` panics, so a failing test cannot
    // leave a dangling buffer attached to this worker thread.
    let restore = RestoreCaptureBuffer;
    let result = f();
    drop(restore);

    let logs = String::from_utf8(buffer.lock().expect("capture buffer poisoned").clone())
        .expect("captured logs are valid utf8");
    (result, logs)
}

struct RestoreCaptureBuffer;

impl Drop for RestoreCaptureBuffer {
    fn drop(&mut self) {
        CAPTURE_BUFFER.with(|cell| *cell.borrow_mut() = None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // NOTE: do NOT add a test that calls `tracing::callsite::rebuild_interest_cache()`
    // here. That is a process-global operation; under parallel `cargo test` it
    // re-evaluates every callsite's interest against the calling thread's current
    // dispatcher and can transiently disable callsites for *sibling* tests — i.e.
    // such a "regression test" becomes a flake source itself. The reloadable
    // global subscriber's correctness relies on nothing poisoning the global
    // interest cache; production code never calls rebuild, so this stays robust.

    #[test]
    fn capture_is_scoped_to_its_own_closure() {
        let (_, inside) = capture_logs(|| tracing::warn!("scoped_marker_beta"));
        assert!(inside.contains("scoped_marker_beta"));

        // A fresh capture must not inherit the previous scope's output.
        let (_, after) = capture_logs(|| {});
        assert!(
            !after.contains("scoped_marker_beta"),
            "buffers must not leak across capture scopes; after={after:?}"
        );
    }
}
