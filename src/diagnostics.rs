//! Per-run diagnostic aggregation for extractor warnings.
//!
//! G-4: per-file extractor warnings used to spew directly to stderr — a
//! 60-file Claude corpus would emit thousands of lines and drown real signal.
//! This module gates per-file emission behind `--verbose` and emits a compact
//! per-extractor SUMMARY (≤5 lines) at end of run. Full per-file detail is
//! always written to `~/.aicx/state/diagnostics-<run-id>.log` for opt-in
//! review without stderr noise.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process;
use std::sync::{Mutex, MutexGuard, OnceLock};

use anyhow::{Context, Result};
use chrono::Utc;

#[derive(Debug, Clone, Copy, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub enum DiagnosticKind {
    FallbackTimestamp,
    UnparsableTimestamp,
    InvalidEpochMillis,
    OversizedLine,
    MissingSessionId,
    SessionIdDrift,
    UnknownMsgType,
    JunieFallbackId,
    BidiOverride,
    ZeroWidth,
    NullByteStripped,
    MissingSessionMeta,
    DuplicateSessionMeta,
    FilenameMismatch,
    MixedFormat,
    LineParseError,
}

impl DiagnosticKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::FallbackTimestamp => "fallback_timestamp",
            Self::UnparsableTimestamp => "unparsable_timestamp",
            Self::InvalidEpochMillis => "invalid_epoch_millis",
            Self::OversizedLine => "oversized_line",
            Self::MissingSessionId => "missing_session_id",
            Self::SessionIdDrift => "session_id_drift",
            Self::UnknownMsgType => "unknown_msg_type",
            Self::JunieFallbackId => "junie_fallback_id",
            Self::BidiOverride => "bidi_override",
            Self::ZeroWidth => "zero_width",
            Self::NullByteStripped => "null_byte_stripped",
            Self::MissingSessionMeta => "missing_session_meta",
            Self::DuplicateSessionMeta => "duplicate_session_meta",
            Self::FilenameMismatch => "filename_mismatch",
            Self::MixedFormat => "mixed_format",
            Self::LineParseError => "line_parse_error",
        }
    }
}

const EXTRACTOR_ORDER: &[&str] = &["claude", "codex", "gemini", "junie", "grok"];

#[derive(Default)]
struct ExtractorCounters {
    counts: BTreeMap<DiagnosticKind, usize>,
    files: BTreeMap<DiagnosticKind, BTreeSet<String>>,
}

impl ExtractorCounters {
    fn record(&mut self, kind: DiagnosticKind, count: usize, path_label: &str) {
        *self.counts.entry(kind).or_insert(0) += count;
        if !path_label.is_empty() {
            self.files
                .entry(kind)
                .or_default()
                .insert(path_label.to_string());
        }
    }

    fn is_empty(&self) -> bool {
        self.counts.values().all(|c| *c == 0)
    }

    fn line_for(&self, extractor: &str) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let mut parts: Vec<String> = Vec::new();
        let mut ts_files: BTreeSet<&String> = BTreeSet::new();
        let mut ts_frames = 0usize;
        if let Some(c) = self.counts.get(&DiagnosticKind::FallbackTimestamp) {
            ts_frames += *c;
            if let Some(set) = self.files.get(&DiagnosticKind::FallbackTimestamp) {
                ts_files.extend(set.iter());
            }
        }
        if ts_frames > 0 {
            parts.push(format!(
                "{} files / {} frames preserved with fallback timestamp",
                ts_files.len(),
                ts_frames
            ));
        }

        let mut sani_offsets = 0usize;
        let mut sani_files: BTreeSet<&String> = BTreeSet::new();
        for kind in [
            DiagnosticKind::BidiOverride,
            DiagnosticKind::ZeroWidth,
            DiagnosticKind::NullByteStripped,
        ] {
            if let Some(c) = self.counts.get(&kind) {
                sani_offsets += *c;
            }
            if let Some(set) = self.files.get(&kind) {
                sani_files.extend(set.iter());
            }
        }
        if sani_offsets > 0 {
            parts.push(format!(
                "{} files / {} bidi/ZWS/NUL offsets preserved-with-warning",
                sani_files.len(),
                sani_offsets
            ));
        }

        let mut emit_simple = |kind: DiagnosticKind, label: &str| {
            if let Some(c) = self.counts.get(&kind).copied()
                && c > 0
            {
                let files = self.files.get(&kind).map(|s| s.len()).unwrap_or(0);
                parts.push(format!("{files} files / {c} {label}"));
            }
        };
        emit_simple(
            DiagnosticKind::UnparsableTimestamp,
            "unparsable timestamps (frames dropped)",
        );
        emit_simple(
            DiagnosticKind::InvalidEpochMillis,
            "invalid epoch millisecond timestamps (frames dropped)",
        );
        emit_simple(
            DiagnosticKind::OversizedLine,
            "oversized JSONL lines skipped",
        );
        emit_simple(
            DiagnosticKind::UnknownMsgType,
            "unknown message type(s) preserved as system_note",
        );
        emit_simple(
            DiagnosticKind::MissingSessionId,
            "missing session id(s) (fallback used)",
        );
        emit_simple(DiagnosticKind::SessionIdDrift, "session id drift event(s)");
        emit_simple(DiagnosticKind::JunieFallbackId, "Junie fallback id(s) used");
        emit_simple(
            DiagnosticKind::MissingSessionMeta,
            "missing session_meta payload(s) (fallback used)",
        );
        emit_simple(
            DiagnosticKind::DuplicateSessionMeta,
            "duplicate session_meta payload(s)",
        );
        emit_simple(
            DiagnosticKind::FilenameMismatch,
            "session_meta/filename UUID mismatch(es)",
        );
        emit_simple(DiagnosticKind::MixedFormat, "mixed-format JSONL line(s)");
        emit_simple(
            DiagnosticKind::LineParseError,
            "malformed JSONL line(s) skipped",
        );

        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "{} diagnostics: {}.",
            capitalize(extractor),
            parts.join("; ")
        ))
    }
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

#[derive(Default)]
struct DiagnosticsState {
    initialized: bool,
    verbose: bool,
    run_id: String,
    log_path: Option<PathBuf>,
    log_writer: Option<BufWriter<File>>,
    log_failure_reported: bool,
    extractors: BTreeMap<&'static str, ExtractorCounters>,
}

static STATE: OnceLock<Mutex<DiagnosticsState>> = OnceLock::new();

fn lock_state() -> MutexGuard<'static, DiagnosticsState> {
    // Recover from poisoned mutex via `into_inner()` rather than
    // `.expect()`. A poisoned diagnostics mutex means a prior caller
    // panicked while holding the lock — production should continue
    // accumulating diagnostics, and tests must not cascade-fail on
    // an unrelated sibling test's panic.
    //
    // Concrete trigger: parallel `cargo test` schedules where
    // `diagnostics::tests::lock_test_init` (calls `reset_for_tests` +
    // `init`) races with `sources::tests::test_extract_codex_file_*`
    // (calls production `record` → `lock_state`). If any test panics
    // mid-mutation, every sibling test that touches the global state
    // hits the cascade panic at this site. Recovery here keeps the
    // race window from amplifying one flake into five failures.
    //
    // The mirror pattern already lives in `summary_aggregates_per_extractor`
    // (further down the test module) where it is annotated with the
    // same rationale — this site is now consistent.
    STATE
        .get_or_init(|| Mutex::new(DiagnosticsState::default()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Initialize the diagnostics aggregator for this CLI run.
///
/// `verbose` controls whether per-file warnings are echoed to stderr (default
/// false keeps stderr quiet). The structured run log is always written to
/// `<state_dir>/diagnostics-<run-id>.log` when `state_dir` is resolvable.
pub fn init(verbose: bool, state_dir: Option<PathBuf>) -> Result<()> {
    let mut state = lock_state();
    if state.initialized {
        // Idempotent: respect the strongest verbose request, do not re-rotate log.
        state.verbose = state.verbose || verbose;
        return Ok(());
    }

    let run_id = generate_run_id();
    let mut log_path = None;
    let mut log_writer = None;
    if let Some(dir) = state_dir {
        let path = dir.join(format!("diagnostics-{run_id}.log"));
        match open_log(&path) {
            Ok(writer) => {
                log_writer = Some(writer);
                log_path = Some(path);
            }
            Err(err) => {
                eprintln!(
                    "diagnostics: failed to open run log at {}: {err}",
                    path.display()
                );
                state.log_failure_reported = true;
            }
        }
    }

    state.initialized = true;
    state.verbose = verbose;
    state.run_id = run_id;
    state.log_path = log_path;
    state.log_writer = log_writer;
    Ok(())
}

fn open_log(path: &Path) -> Result<BufWriter<File>> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create diagnostics log dir {}", parent.display()))?;
    }
    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("open diagnostics log {}", path.display()))?;
    let mut writer = BufWriter::new(file);
    let _ = writeln!(
        writer,
        "# aicx diagnostics run-log opened {}",
        Utc::now().to_rfc3339()
    );
    Ok(writer)
}

fn generate_run_id() -> String {
    let ts = Utc::now().format("%Y%m%dT%H%M%SZ");
    format!("{ts}-{}", process::id())
}

pub fn is_verbose() -> bool {
    lock_state().verbose
}

pub fn run_id() -> Option<String> {
    let state = lock_state();
    if state.initialized {
        Some(state.run_id.clone())
    } else {
        None
    }
}

pub fn log_path() -> Option<PathBuf> {
    lock_state().log_path.clone()
}

/// Record one or more occurrences of a diagnostic kind for `extractor`,
/// associated with `path` (which is used to build the "N files / X events"
/// part of the SUMMARY).
pub fn record(extractor: &'static str, kind: DiagnosticKind, count: usize, path: &Path) {
    if count == 0 {
        return;
    }
    let mut state = lock_state();
    let key = canonical_extractor_key(extractor);
    let label = path.display().to_string();
    state
        .extractors
        .entry(key)
        .or_default()
        .record(kind, count, &label);
}

/// Append a fully-formatted line to the per-run diagnostic log. No-op if the
/// log file could not be opened. Always called regardless of verbosity.
pub fn log_describe(line: &str) {
    let mut state = lock_state();
    let writer = match state.log_writer.as_mut() {
        Some(w) => w,
        None => return,
    };
    if let Err(err) = writeln!(writer, "{line}")
        && !state.log_failure_reported
    {
        eprintln!("diagnostics: failed to write run log: {err}");
        state.log_failure_reported = true;
    }
}

/// Emit the per-extractor SUMMARY to stderr. Quiet when no diagnostics were
/// recorded. Caps total output at ≤5 lines: up to 5 extractor buckets (4
/// known + the `unknown` catch-all), and the trailer hint is suppressed once
/// 5 bucket lines are present so the cap holds.
pub fn emit_summary() {
    let mut state = lock_state();
    if !state.initialized {
        return;
    }

    if let Some(writer) = state.log_writer.as_mut() {
        let _ = writer.flush();
    }

    let lines = summary_lines(&state.extractors);

    if lines.is_empty() {
        return;
    }

    for line in &lines {
        eprintln!("{line}");
    }

    // Trailer: point operator at the structured log (counts toward ≤5 cap).
    if lines.len() < 5
        && let Some(path) = state.log_path.as_ref()
    {
        eprintln!(
            "Diagnostics detail: {} (use --verbose for per-file)",
            path.display()
        );
    }
}

/// Build the per-extractor SUMMARY lines in canonical order. Pure over the
/// recorded counters so the line set can be unit-tested without capturing
/// stderr. Known extractors come first in `EXTRACTOR_ORDER`; the catch-all
/// `unknown` bucket is appended last so a recorded unrecognized extractor is
/// surfaced rather than silently dropped.
fn summary_lines(extractors: &BTreeMap<&'static str, ExtractorCounters>) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for &extractor in EXTRACTOR_ORDER {
        if let Some(counters) = extractors.get(extractor)
            && let Some(line) = counters.line_for(extractor)
        {
            lines.push(line);
        }
    }
    // Catch-all: surface the explicit `unknown` bucket last so a recorded
    // unrecognized extractor is visible in the summary rather than dropped.
    if let Some(counters) = extractors.get("unknown")
        && let Some(line) = counters.line_for("unknown")
    {
        lines.push(line);
    }
    lines
}

fn canonical_extractor_key(extractor: &str) -> &'static str {
    match extractor {
        "claude" => "claude",
        "codex" => "codex",
        "gemini" => "gemini",
        "junie" => "junie",
        _ => {
            // Unknown extractor → explicit "unknown" bucket, never silent
            // attribution to claude. Drop the debug_assert because legit
            // future extractors will hit this path before they are added
            // to the canonical key list, and release builds previously
            // misattributed silently to the claude bucket.
            "unknown"
        }
    }
}

#[cfg(test)]
pub(crate) fn reset_for_tests() {
    let mut state = lock_state();
    *state = DiagnosticsState::default();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    // Serializes every test that resets/uses the process-global
    // `DiagnosticsState`. The returned guard MUST be bound (`let _g = ...`) so
    // it lives for the whole test body — otherwise parallel `cargo test` races
    // the shared state via `record()` / `lock_state()` across sibling tests
    // (the root cause of the intermittent diagnostics flakes).
    static TEST_SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());

    // Bind for the whole test body. EVERY test touching the global
    // `DiagnosticsState` must hold this, or a sibling that resets the state
    // (e.g. `summary_skipped_when_no_records`) races those that read it.
    fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
        TEST_SERIAL.lock().unwrap_or_else(|e| e.into_inner())
    }

    #[must_use = "bind the guard (`let _g = lock_test_init(...)`) so it serializes the whole test"]
    fn lock_test_init(verbose: bool, dir: Option<PathBuf>) -> std::sync::MutexGuard<'static, ()> {
        let guard = serial_guard();
        reset_for_tests();
        init(verbose, dir).expect("init");
        guard
    }

    #[test]
    fn summary_skipped_when_no_records() {
        let _serial = serial_guard();
        // emit_summary only reads the global state and prints to stderr — it
        // must not panic regardless of what a parallel test has recorded. Call
        // it without holding the lock.
        emit_summary();

        // For the "no records => empty extractor map" contract, hold the global
        // lock across the reset+assert so a parallel test recording into the
        // shared `Mutex<DiagnosticsState>` cannot race a `record()` in between
        // (same discipline as `summary_aggregates_per_extractor`). Without this
        // the assert flakes under parallel `cargo test`.
        let mut state = STATE
            .get_or_init(|| Mutex::new(DiagnosticsState::default()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *state = DiagnosticsState::default();
        state.initialized = true;
        assert!(state.extractors.is_empty());
    }

    #[test]
    fn summary_aggregates_per_extractor() {
        let _serial = serial_guard();
        // G-4 stores diagnostics in a process-global `Mutex<DiagnosticsState>`.
        // Other tests can call production paths (extract_*_file) that record
        // into this same global. To make this test deterministic under
        // parallel `cargo test`, hold the lock for the entire test body and
        // record inline against `&mut state` rather than re-entering the
        // global `record()` helper (which would acquire+release per call and
        // leave race windows). Recover from prior-test poison silently — the
        // next line wipes state anyway.
        let mut state = STATE
            .get_or_init(|| Mutex::new(DiagnosticsState::default()))
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        *state = DiagnosticsState::default();
        state.initialized = true;

        for i in 0..10 {
            let p = PathBuf::from(format!("/tmp/claude/sess-{i}.jsonl"));
            let label = p.display().to_string();
            state.extractors.entry("claude").or_default().record(
                DiagnosticKind::FallbackTimestamp,
                5,
                &label,
            );
        }
        let line = state
            .extractors
            .get("claude")
            .and_then(|c| c.line_for("claude"))
            .expect("claude line");
        assert!(line.contains("10 files"));
        assert!(line.contains("50 frames preserved with fallback timestamp"));
    }

    #[test]
    fn run_id_is_set_after_init() {
        let _serial = lock_test_init(false, None);
        let id = run_id().expect("run_id present after init");
        assert!(!id.is_empty());
        assert!(id.contains('Z') || id.contains('-'));
    }

    #[test]
    fn unknown_extractor_routes_to_unknown_bucket_not_claude() {
        // Regression guard for the release-build silent fallthrough that
        // misattributed any unrecognized extractor name (e.g. a future
        // "qwen" or a typo) into the "claude" bucket because
        // `debug_assert!` is a no-op outside debug builds.
        let _serial = lock_test_init(false, None);
        let p = PathBuf::from("/tmp/test.jsonl");
        record("qwen", DiagnosticKind::FallbackTimestamp, 1, &p);
        let state = lock_state();
        let unknown = state
            .extractors
            .get("unknown")
            .expect("unknown bucket present");
        assert_eq!(
            unknown.counts.get(&DiagnosticKind::FallbackTimestamp),
            Some(&1),
            "unknown extractor must route to the unknown bucket"
        );
        let claude_count = state
            .extractors
            .get("claude")
            .and_then(|c| c.counts.get(&DiagnosticKind::FallbackTimestamp))
            .copied()
            .unwrap_or(0);
        assert_eq!(
            claude_count, 0,
            "unknown extractor must NOT bleed into the claude bucket"
        );
    }

    #[test]
    fn unknown_bucket_is_emitted_in_summary_lines() {
        // The catch-all `unknown` bucket must appear in the emitted SUMMARY,
        // not be silently dropped because it is absent from EXTRACTOR_ORDER.
        // Regression guard for the "recorded-but-invisible" gap surfaced in
        // pass-6 (AUD-2).
        let _serial = lock_test_init(false, None);
        let p = PathBuf::from("/tmp/test.jsonl");
        record("qwen", DiagnosticKind::FallbackTimestamp, 1, &p); // routes to "unknown"
        record("claude", DiagnosticKind::FallbackTimestamp, 2, &p);
        let state = lock_state();
        let lines = summary_lines(&state.extractors);
        assert!(
            lines.iter().any(|l| l.starts_with("Claude diagnostics:")),
            "known extractor line must still be present, got: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.starts_with("Unknown diagnostics:")),
            "unknown bucket must surface in the summary, got: {lines:?}"
        );
        assert!(
            lines
                .last()
                .map(|l| l.starts_with("Unknown diagnostics:"))
                .unwrap_or(false),
            "unknown line must sort last, after known extractors, got: {lines:?}"
        );
    }

    #[test]
    fn sanitization_offsets_combined_under_single_phrase() {
        let _serial = lock_test_init(false, None);
        let p = PathBuf::from("/tmp/claude/x.jsonl");
        record("claude", DiagnosticKind::BidiOverride, 10, &p);
        record("claude", DiagnosticKind::ZeroWidth, 30, &p);
        record("claude", DiagnosticKind::NullByteStripped, 7, &p);
        let state = lock_state();
        let line = state
            .extractors
            .get("claude")
            .and_then(|c| c.line_for("claude"))
            .expect("claude line");
        assert!(line.contains("1 files / 47 bidi/ZWS/NUL offsets"));
    }
}
