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
    /// Computed at `init`, materialized lazily on the first write. Read-only
    /// commands (health, doctor without records) must never create the run
    /// log just because the process started.
    pending_log_path: Option<PathBuf>,
    /// Set only once the log file actually exists on disk.
    log_path: Option<PathBuf>,
    log_writer: Option<BufWriter<File>>,
    log_open_failed: bool,
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
    // `init`) races with extraction tests that call production `record` →
    // `lock_state`. If any test panics
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
/// false keeps stderr quiet). The structured run log path is computed here but
/// the file is **not** created — it materializes on the first actual write
/// (see [`log_describe`]), so read-only commands leave `<state_dir>` byte-for-
/// byte untouched. Retention over closed run logs is enforced at that first
/// materialization, never during a run that writes nothing.
pub fn init(verbose: bool, state_dir: Option<PathBuf>) -> Result<()> {
    let mut state = lock_state();
    if state.initialized {
        // Idempotent: respect the strongest verbose request, do not re-rotate log.
        state.verbose = state.verbose || verbose;
        return Ok(());
    }

    let run_id = generate_run_id();
    state.pending_log_path = state_dir.map(|dir| dir.join(format!("diagnostics-{run_id}.log")));
    state.initialized = true;
    state.verbose = verbose;
    state.run_id = run_id;
    state.log_path = None;
    state.log_writer = None;
    Ok(())
}

/// Materialize the run log on first use: enforce retention over closed run
/// logs in the same directory, then open the file append-only. Failure is
/// remembered so a broken filesystem is reported once, not per line.
fn ensure_log_writer(state: &mut DiagnosticsState) {
    if state.log_writer.is_some() || state.log_open_failed {
        return;
    }
    let Some(path) = state.pending_log_path.clone() else {
        return;
    };
    if let (Some(dir), Some(active_name)) = (path.parent(), path.file_name()) {
        // Best-effort by design: retention must never block the run that is
        // trying to write its own diagnostics.
        let _ = enforce_retention(
            dir,
            active_name,
            Utc::now(),
            &DiagnosticsRetentionPolicy::default(),
        );
    }
    match open_log(&path) {
        Ok(writer) => {
            state.log_writer = Some(writer);
            state.log_path = Some(path);
        }
        Err(err) => {
            eprintln!(
                "diagnostics: failed to open run log at {}: {err}",
                path.display()
            );
            state.log_open_failed = true;
            state.log_failure_reported = true;
        }
    }
}

/// Retention policy for closed diagnostics run logs under `<state_dir>`.
///
/// Applied only when a new run materializes its own log. Bounds are joint:
/// age first, then file count, then total bytes — deterministic given the
/// same directory contents. The active run's log and logs owned by a still-
/// running process are never candidates.
#[derive(Debug, Clone)]
pub struct DiagnosticsRetentionPolicy {
    /// Keep at most this many run logs (protected files count toward the cap
    /// but are never deleted to satisfy it).
    pub max_files: usize,
    /// Keep at most this many bytes of run logs in total.
    pub max_total_bytes: u64,
    /// Delete closed run logs older than this, regardless of count/bytes.
    pub max_age: chrono::Duration,
}

impl Default for DiagnosticsRetentionPolicy {
    fn default() -> Self {
        Self {
            max_files: 32,
            max_total_bytes: 64 * 1024 * 1024,
            max_age: chrono::Duration::days(14),
        }
    }
}

/// Outcome of one retention pass, for tests and (future) doctor reporting.
#[derive(Debug, Default)]
pub struct DiagnosticsRetentionOutcome {
    pub deleted: Vec<PathBuf>,
    pub kept: usize,
}

/// One closed-run candidate parsed from `diagnostics-<stamp>-<pid>.log`.
struct RunLogEntry {
    path: PathBuf,
    stamp: chrono::DateTime<Utc>,
    size: u64,
}

/// Parse `diagnostics-<%Y%m%dT%H%M%SZ>-<pid>.log`. Returns `None` for
/// anything that does not match exactly — unknown files are never deleted.
fn parse_run_log_name(name: &str) -> Option<(chrono::DateTime<Utc>, u32)> {
    let rest = name.strip_prefix("diagnostics-")?.strip_suffix(".log")?;
    let (stamp_raw, pid_raw) = rest.rsplit_once('-')?;
    let pid: u32 = pid_raw.parse().ok()?;
    let stamp = chrono::NaiveDateTime::parse_from_str(stamp_raw, "%Y%m%dT%H%M%SZ")
        .ok()?
        .and_utc();
    Some((stamp, pid))
}

/// Best-effort liveness probe for the pid embedded in a run-log name.
/// `Some(true)` = alive (protected), `Some(false)` = dead (closed run),
/// `None` = cannot tell on this platform (protected unless past `max_age`).
fn run_log_pid_alive(pid: u32) -> Option<bool> {
    if pid == std::process::id() {
        return Some(true);
    }
    #[cfg(unix)]
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return None;
        }
        let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
        if rc == 0 {
            return Some(true);
        }
        match std::io::Error::last_os_error().raw_os_error() {
            Some(libc::ESRCH) => Some(false),
            // EPERM: process exists under another uid — alive.
            Some(libc::EPERM) => Some(true),
            _ => None,
        }
    }
    #[cfg(not(unix))]
    {
        None
    }
}

/// Enforce [`DiagnosticsRetentionPolicy`] over `dir`.
///
/// Never touches `active_name`, files whose embedded pid is still alive, or
/// files whose name does not parse as a run log. Deletion is per-file
/// `remove_file` (atomic at the filesystem level); order is deterministic:
/// age purge first, then oldest-first until the count cap holds, then
/// oldest-first until the byte cap holds.
pub(crate) fn enforce_retention(
    dir: &Path,
    active_name: &std::ffi::OsStr,
    now: chrono::DateTime<Utc>,
    policy: &DiagnosticsRetentionPolicy,
) -> DiagnosticsRetentionOutcome {
    let mut outcome = DiagnosticsRetentionOutcome::default();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return outcome;
    };

    let mut protected_count = 0usize;
    let mut protected_bytes = 0u64;
    let mut eligible: Vec<RunLogEntry> = Vec::new();

    for entry in entries.flatten() {
        let name_os = entry.file_name();
        let Some(name) = name_os.to_str() else {
            continue;
        };
        if !name.starts_with("diagnostics-") || !name.ends_with(".log") {
            continue;
        }
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        if name_os == active_name {
            protected_count += 1;
            protected_bytes += size;
            continue;
        }
        let Some((stamp, pid)) = parse_run_log_name(name) else {
            // Fail closed: a run-log-looking file we cannot parse is kept.
            protected_count += 1;
            protected_bytes += size;
            continue;
        };
        let age = now.signed_duration_since(stamp);
        let closed = match run_log_pid_alive(pid) {
            Some(true) => false,
            Some(false) => true,
            // Unknown liveness: only age can prove the run abandoned.
            None => age > policy.max_age,
        };
        if !closed {
            protected_count += 1;
            protected_bytes += size;
            continue;
        }
        eligible.push(RunLogEntry {
            path: entry.path(),
            stamp,
            size,
        });
    }

    // Deterministic order: oldest first, name as tie-break.
    eligible.sort_by(|a, b| (a.stamp, &a.path).cmp(&(b.stamp, &b.path)));

    let delete = |entry: &RunLogEntry, outcome: &mut DiagnosticsRetentionOutcome| {
        match std::fs::remove_file(&entry.path) {
            Ok(()) => outcome.deleted.push(entry.path.clone()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                outcome.deleted.push(entry.path.clone());
            }
            Err(_) => {}
        }
    };

    // 1. Age purge.
    let mut kept: Vec<RunLogEntry> = Vec::new();
    for entry in eligible {
        if now.signed_duration_since(entry.stamp) > policy.max_age {
            delete(&entry, &mut outcome);
        } else {
            kept.push(entry);
        }
    }

    // 2. Count cap (protected files count toward the cap, oldest closed go).
    while protected_count + kept.len() > policy.max_files && !kept.is_empty() {
        let entry = kept.remove(0);
        delete(&entry, &mut outcome);
    }

    // 3. Byte cap.
    let mut total_bytes = protected_bytes + kept.iter().map(|e| e.size).sum::<u64>();
    while total_bytes > policy.max_total_bytes && !kept.is_empty() {
        let entry = kept.remove(0);
        total_bytes = total_bytes.saturating_sub(entry.size);
        delete(&entry, &mut outcome);
    }

    outcome.kept = protected_count + kept.len();
    outcome
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

/// Append a fully-formatted line to the per-run diagnostic log. The log file
/// is materialized on the first call (lazy — see [`init`]); no-op if it could
/// not be opened. Always called regardless of verbosity.
pub fn log_describe(line: &str) {
    let mut state = lock_state();
    ensure_log_writer(&mut state);
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
    fn init_alone_creates_no_log_file() {
        // W2-04 red-first: a read-only command (health/doctor without
        // records) must not materialize `diagnostics-<run-id>.log`. The
        // log file may only appear once something is actually written.
        let dir = std::env::temp_dir().join(format!(
            "aicx-diag-lazy-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let _serial = lock_test_init(false, Some(dir.clone()));
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert!(
            entries.is_empty(),
            "init must not create a diagnostics log before the first write; found {entries:?}"
        );
        // First write materializes the log (and only then).
        log_describe("first line");
        let entries: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(
            entries.len(),
            1,
            "first log_describe must materialize exactly the run log"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn retention_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aicx-diag-retention-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Pid far above pid_max on Linux (4 194 304) and macOS (99 998) —
    /// guaranteed ESRCH, i.e. a closed run.
    const DEAD_PID: u32 = 999_999_999;

    fn write_log(dir: &Path, stamp: &str, pid: u32, bytes: usize) -> PathBuf {
        let path = dir.join(format!("diagnostics-{stamp}-{pid}.log"));
        std::fs::write(&path, vec![b'x'; bytes]).unwrap();
        path
    }

    #[test]
    fn retention_below_current_size_rotates_only_eligible_closed_runs() {
        // W2-04 red-first: limits set below the current directory size must
        // rotate ONLY eligible closed runs — never the active run, never a
        // log owned by a live process, never an unparsable name.
        let dir = retention_dir("eligible");
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
            .unwrap()
            .to_utc();

        let active_name = format!("diagnostics-20260722T115900Z-{}.log", std::process::id());
        let active = dir.join(&active_name);
        std::fs::write(&active, vec![b'x'; 4096]).unwrap();
        let live = write_log(&dir, "20260722T110000Z", std::process::id(), 4096);
        let weird = dir.join("diagnostics-not-a-run-log.log");
        std::fs::write(&weird, b"???").unwrap();
        let old_closed = write_log(&dir, "20260720T090000Z", DEAD_PID, 4096);
        let mid_closed = write_log(&dir, "20260721T090000Z", DEAD_PID, 4096);
        let new_closed = write_log(&dir, "20260722T090000Z", DEAD_PID, 4096);

        let policy = DiagnosticsRetentionPolicy {
            max_files: 4,
            max_total_bytes: 1, // far below current size — pressure everywhere
            max_age: chrono::Duration::days(14),
        };
        let outcome = enforce_retention(&dir, std::ffi::OsStr::new(&active_name), now, &policy);

        assert!(active.exists(), "active run must never be deleted");
        assert!(live.exists(), "live-pid run must never be deleted");
        assert!(
            weird.exists(),
            "unparsable run-log name must never be deleted"
        );
        assert!(
            !old_closed.exists(),
            "closed runs must rotate under pressure"
        );
        assert!(
            !mid_closed.exists(),
            "closed runs must rotate under pressure"
        );
        assert!(
            !new_closed.exists(),
            "closed runs must rotate under pressure"
        );
        assert_eq!(outcome.deleted.len(), 3);
        // Deterministic order: oldest closed first.
        assert_eq!(outcome.deleted[0], old_closed);
        assert_eq!(outcome.deleted[1], mid_closed);
        assert_eq!(outcome.deleted[2], new_closed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_count_and_age_caps_are_deterministic_and_keep_newest() {
        let dir = retention_dir("caps");
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
            .unwrap()
            .to_utc();

        // Ancient closed run — past max_age, must go regardless of count.
        let ancient = write_log(&dir, "20260601T000000Z", DEAD_PID, 10);
        // Three recent closed runs; count cap 2 keeps the newest two.
        let d1 = write_log(&dir, "20260722T080000Z", DEAD_PID, 10);
        let d2 = write_log(&dir, "20260722T090000Z", DEAD_PID, 10);
        let d3 = write_log(&dir, "20260722T100000Z", DEAD_PID, 10);

        let policy = DiagnosticsRetentionPolicy {
            max_files: 2,
            max_total_bytes: 1024,
            max_age: chrono::Duration::days(14),
        };
        let outcome = enforce_retention(
            &dir,
            std::ffi::OsStr::new("diagnostics-20260722T120000Z-1.log"),
            now,
            &policy,
        );

        assert!(!ancient.exists(), "past-max-age closed run must be purged");
        assert!(
            !d1.exists(),
            "oldest closed run beyond count cap must rotate"
        );
        assert!(d2.exists());
        assert!(d3.exists());
        assert_eq!(outcome.kept, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn retention_no_pressure_deletes_nothing() {
        let dir = retention_dir("idle");
        let now = chrono::DateTime::parse_from_rfc3339("2026-07-22T12:00:00Z")
            .unwrap()
            .to_utc();
        let a = write_log(&dir, "20260722T080000Z", DEAD_PID, 10);
        let b = write_log(&dir, "20260722T090000Z", DEAD_PID, 10);
        let outcome = enforce_retention(
            &dir,
            std::ffi::OsStr::new("diagnostics-20260722T120000Z-1.log"),
            now,
            &DiagnosticsRetentionPolicy::default(),
        );
        assert!(a.exists());
        assert!(b.exists());
        assert!(outcome.deleted.is_empty());
        assert_eq!(outcome.kept, 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_run_log_name_accepts_only_exact_shape() {
        assert!(parse_run_log_name("diagnostics-20260722T081443Z-80671.log").is_some());
        assert!(parse_run_log_name("diagnostics-20260722T081443Z-.log").is_none());
        assert!(parse_run_log_name("diagnostics-garbage-80671.log").is_none());
        assert!(parse_run_log_name("other-20260722T081443Z-80671.log").is_none());
        assert!(parse_run_log_name("diagnostics-20260722T081443Z-80671.txt").is_none());
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
