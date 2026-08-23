//! Source-driven lexical index.
//!
//! The durable corpus is the session catalog plus live source files. This
//! module parses each cataloged source once, keeps only user/assistant signal,
//! writes at most one readable extract per session, and publishes Tantivy
//! directly. It never reads or writes per-frame store cards or embedding
//! NDJSON intermediates.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};
use std::time::Instant;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::SecondsFormat;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::catalog::CatalogEntry;
use crate::timeline::{FrameKind, TimelineEntry};

const MAX_MESSAGE_CHARS: usize = 256 * 1024;
const MAX_EXTRACT_CHARS: usize = 4 * 1024 * 1024;
const MAX_UNBROKEN_TOKEN_CHARS: usize = 4096;
const MAX_FULL_PARSE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_JSONL_RECORD_BYTES: usize = 2 * 1024 * 1024;
const VIBECRAFTED_RETENTION_BYTES: usize = 64 * 1024;
const IN_FLIGHT_FATAL_GRACE: Duration = Duration::from_secs(5 * 60);

/// Bump whenever signal filtering or extract body shaping changes.
///
/// The catalog fingerprint alone is not enough: a CURRENT generation built
/// before thought-token stripping still matched the same catalog bytes and
/// short-circuited forever, leaving search previews full of
/// `{"type":"thought","data":"..."}` spam. Including this constant forces a
/// one-shot rebuild so index truth tracks filter truth.
const SIGNAL_FILTER_VERSION: &str = "signal-v3-workspace-metadata-strip";
/// Bump when typed terminal classification or parser support changes.
const DISPOSITION_POLICY_VERSION: &str = "source-disposition-v2-missing-terminal";

const PARSE_STATE_SCHEMA: &str = "aicx.source_parse_state.v1";
const PARSE_STATE_RELPATH: &str = "indexed/_all/source_parse_state.v1.json";

#[derive(Debug, Clone, Serialize)]
pub struct SourceIndexReport {
    pub catalog_path: String,
    pub sources_total: usize,
    pub sources_parsed: usize,
    /// Sessions whose extract was reused without re-parsing the live source.
    pub sources_reused: usize,
    /// Unchanged zero-signal or typed terminal dispositions reused from the ledger.
    pub terminal_reused: usize,
    pub sources_skipped: usize,
    pub zero_signal_sources: usize,
    pub terminal_skip_sources: usize,
    pub retryable_error_sources: usize,
    pub deferred_live_changed_sources: usize,
    pub raw_frames: usize,
    pub signal_frames: usize,
    pub filtered_frames: usize,
    pub extracts_written: usize,
    pub lexical_docs: usize,
    /// Dense vectors published when `semantic` was requested; 0 for lexical-only.
    #[serde(default)]
    pub dense_docs: usize,
    /// `optional_not_built` | `exact_mmap_v1` | …
    #[serde(default)]
    pub dense_kind: String,
    /// Whether this run requested `aicx index --semantic`.
    #[serde(default)]
    pub semantic_requested: bool,
    pub unchanged: bool,
    pub wall_ms: u64,
    pub manifest_path: Option<String>,
    pub skipped_by_agent: BTreeMap<String, usize>,
}

/// Honest corpus coverage attached to every CLI search surface.
///
/// `scanned_sessions` is the number of catalog sessions present in the
/// published CURRENT lexical generation. `total_sessions` is the durable
/// catalog size. Missing rows are grouped by extractor so an empty result
/// cannot silently hide an unreadable agent source.
#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct SearchCoverage {
    pub scanned_sessions: usize,
    pub total_sessions: usize,
    pub skipped: BTreeMap<String, usize>,
}

impl SearchCoverage {
    pub fn single_session(scanned: bool, agent: &str) -> Self {
        let mut skipped = BTreeMap::new();
        if !scanned {
            skipped.insert(format!("{}_unreadable", coverage_agent_key(agent)), 1);
        }
        Self {
            scanned_sessions: usize::from(scanned),
            total_sessions: 1,
            skipped,
        }
    }

    pub fn render_line(&self) -> String {
        let skipped = if self.skipped.is_empty() {
            "none".to_string()
        } else {
            self.skipped
                .iter()
                .map(|(reason, count)| format!("{reason}={count}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        format!(
            "scanned {} of {} sessions; skipped: {}",
            self.scanned_sessions, self.total_sessions, skipped
        )
    }
}

/// One stable, source-ordered passage from a catalog session.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionPassage {
    pub passage: usize,
    pub line_span: LineSpan,
    pub match_lines: Vec<usize>,
    pub text: String,
    pub source_path: String,
    pub document_path: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct LineSpan {
    pub start: usize,
    pub end: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionPassageReport {
    pub session_id: String,
    pub agent: String,
    pub query: String,
    pub mode: &'static str,
    pub context: usize,
    pub cache_hit: bool,
    pub passages: Vec<SessionPassage>,
    pub coverage: SearchCoverage,
}

#[derive(Debug)]
struct SessionDocument {
    body: String,
    source_path: String,
    document_path: PathBuf,
    cache_hit: bool,
}

/// Durable per-session parse ledger for true incremental index builds.
///
/// Whole-catalog fingerprint short-circuit covers the no-op case. When the
/// catalog grows (new sessions) this ledger lets the indexer re-parse only the
/// changed rows and reuse cached extracts for the rest — the audit failure was
/// "+28 sessions → full multi-ten-minute reparse".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct SourceParseState {
    schema: String,
    signal_filter_version: String,
    /// Hash of normalized checkout prefixes from `$AICX_HOME/.aicxignore`.
    /// Old ledgers deserialize to empty and are deliberately not reusable.
    #[serde(default)]
    repo_path_ignore_fingerprint: String,
    #[serde(default)]
    disposition_policy_version: String,
    /// Whole-snapshot fingerprint used by CURRENT. Empty on legacy ledgers.
    #[serde(default)]
    source_fingerprint: String,
    sessions: BTreeMap<String, SessionParseRecord>,
    /// Durable accounting exists independently of extract caching.
    #[serde(default)]
    dispositions: BTreeMap<String, SourceDispositionRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SessionParseRecord {
    source_path: String,
    /// Live source size when this extract was produced (bytes).
    #[serde(default)]
    source_len: u64,
    /// Live source mtime when this extract was produced (unix nanoseconds).
    #[serde(default)]
    source_mtime_ns: u64,
    extract_relpath: String,
    extract_sha256: String,
    raw_frames: usize,
    signal_frames: usize,
    filtered_frames: usize,
    project: Option<String>,
    date: Option<String>,
    cwd: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TerminalSkipReason {
    ParserFatal,
    UnsupportedAgent,
    OversizedUnsupportedSource,
    SourceMissing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RetryableErrorReason {
    SourceUnavailable,
    ParserError,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
enum SourceDisposition {
    Indexed,
    ZeroSignal,
    TerminalSkip { reason: TerminalSkipReason },
    RetryableError { reason: RetryableErrorReason },
    DeferredLiveChanged,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SourceDispositionRecord {
    source_path: String,
    #[serde(default)]
    source_len: u64,
    #[serde(default)]
    source_mtime_ns: u64,
    project: Option<String>,
    disposition: SourceDisposition,
    #[serde(default)]
    raw_frames: usize,
    #[serde(default)]
    signal_frames: usize,
    #[serde(default)]
    filtered_frames: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct SourceAccounting {
    pub total: usize,
    pub indexed: usize,
    pub zero_signal: usize,
    pub terminal_skip: usize,
    pub retryable_error: usize,
    pub deferred_live_changed: usize,
    pub source_drift: usize,
    pub source_fingerprint: String,
    pub snapshot_matches: Option<bool>,
}

impl SourceAccounting {
    fn is_complete(&self) -> bool {
        self.total
            == self.indexed
                + self.zero_signal
                + self.terminal_skip
                + self.retryable_error
                + self.deferred_live_changed
            && self.retryable_error == 0
            && self.deferred_live_changed == 0
    }

    pub(crate) fn lexical_pending(&self, lexical_docs: usize) -> usize {
        self.indexed.saturating_sub(lexical_docs)
            + self.retryable_error
            + self.deferred_live_changed
    }
}

enum SourceParseOutcome {
    Parsed(Vec<TimelineEntry>),
    Terminal(TerminalSkipReason),
    DeferredLiveChanged,
}

/// Build or preview the global lexical index from the durable catalog.
///
/// Incremental truth is bounded by catalog rows that carry live source
/// fingerprints (size + mtime-ns). When the catalog snapshot and every
/// selected source fingerprint match the CURRENT generation, reuse is free.
/// A catalog rebuild that re-stats sources admits appends/edits; per-session
/// parse state reuses only extracts whose source fingerprint still matches.
pub fn build(
    aicx_home: &Path,
    project_filters: &[String],
    dry_run: bool,
    full_rescan: bool,
    cache_extracts: bool,
    semantic: bool,
) -> Result<SourceIndexReport> {
    let started = Instant::now();
    if !dry_run && !project_filters.is_empty() {
        anyhow::bail!(
            "project-scoped index publishing is retired; run `aicx index` once for the global \
             catalog, then filter queries with `aicx search -p <project>` (use `aicx index -p \
             <project> --dry-run` only to inspect a project slice)"
        );
    }
    if semantic && dry_run {
        anyhow::bail!(
            "`aicx index --semantic --dry-run` cannot preview dense embedding without writing; \
             drop --dry-run to embed, or run lexical `aicx index --dry-run` alone"
        );
    }
    let catalog_path = crate::catalog::sessions_path_for(aicx_home);
    let entries = crate::catalog::read_entries_at(aicx_home)?;
    if entries.is_empty() {
        anyhow::bail!(
            "session catalog is empty at {}; run `aicx catalog rebuild` first",
            catalog_path.display()
        );
    }

    let selected: Vec<CatalogEntry> = entries
        .into_iter()
        .filter(|entry| project_selected(entry.project.as_deref(), project_filters))
        .collect();
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let ignore = crate::legacy_archive::load_repo_path_ignore(aicx_home, &user_home)?;
    let repo_path_ignore_fingerprint = ignore.fingerprint();
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    // Digest includes LIVE size+mtime so appends without catalog rebuild still
    // move the generation fingerprint (source-change incremental).
    let source_fingerprint = source_fingerprint(
        aicx_home,
        &catalog_path,
        &selected,
        &source_allow,
        &repo_path_ignore_fingerprint,
    )?;
    // Incremental short-circuit applies to both publish and dry-run. A matching
    // live+catalog digest means CURRENT already reflects this snapshot —
    // re-parsing ~10k sources on every `index --dry-run` recreated the mill
    // latency the extracts-store cut was meant to kill. Use `--full-rescan` to
    // force a walk. Live source drift (append without catalog rebuild) moves
    // the digest so recent frames cannot stay invisible forever.
    //
    // `--semantic` refuses the lexical-only short-circuit when dense is absent
    // so operators can attach dense to an otherwise current corpus without
    // `--full-rescan`.
    let dense_missing = crate::vector_index::current_dense_not_built().unwrap_or(true);
    let current_accounting = current_source_accounting_at(aicx_home, None);
    if !full_rescan
        && project_filters.is_empty()
        && crate::vector_index::source_lexical_generation_matches(&source_fingerprint)?
        && current_accounting.as_ref().is_some_and(|accounting| {
            accounting.snapshot_matches == Some(true) && accounting.is_complete()
        })
        && !(semantic && dense_missing)
    {
        let (dense_kind, dense_docs) = current_dense_stats();
        let accounting = current_accounting.expect("matching accounting checked above");
        return Ok(SourceIndexReport {
            catalog_path: catalog_path.display().to_string(),
            sources_total: selected.len(),
            sources_parsed: 0,
            sources_reused: 0,
            terminal_reused: accounting.zero_signal + accounting.terminal_skip,
            sources_skipped: accounting.terminal_skip,
            zero_signal_sources: accounting.zero_signal,
            terminal_skip_sources: accounting.terminal_skip,
            retryable_error_sources: accounting.retryable_error,
            deferred_live_changed_sources: accounting.deferred_live_changed,
            raw_frames: 0,
            signal_frames: 0,
            filtered_frames: 0,
            extracts_written: 0,
            lexical_docs: crate::vector_index::current_lexical_doc_count()?.unwrap_or(0),
            dense_docs,
            dense_kind,
            semantic_requested: semantic,
            unchanged: true,
            wall_ms: started.elapsed().as_millis() as u64,
            manifest_path: crate::vector_index::hybrid_manifest_path(None)
                .ok()
                .map(|path| path.display().to_string()),
            skipped_by_agent: BTreeMap::new(),
        });
    }

    let mut chunks = Vec::with_capacity(selected.len());
    let mut sources_parsed = 0usize;
    let mut sources_reused = 0usize;
    let mut terminal_reused = 0usize;
    let mut sources_skipped = 0usize;
    let mut raw_frames = 0usize;
    let mut signal_frames = 0usize;
    let mut filtered_frames = 0usize;
    let mut extracts_written = 0usize;
    let mut skipped_by_agent = BTreeMap::new();
    let mut next_state = SourceParseState {
        schema: PARSE_STATE_SCHEMA.to_string(),
        signal_filter_version: SIGNAL_FILTER_VERSION.to_string(),
        repo_path_ignore_fingerprint: repo_path_ignore_fingerprint.clone(),
        disposition_policy_version: DISPOSITION_POLICY_VERSION.to_string(),
        source_fingerprint: source_fingerprint.clone(),
        sessions: BTreeMap::new(),
        dispositions: BTreeMap::new(),
    };

    let prior_state = if full_rescan {
        SourceParseState::default()
    } else {
        load_parse_state(aicx_home, &repo_path_ignore_fingerprint)
    };

    for entry in &selected {
        let session_key = session_state_key(&entry.agent, &entry.session_id);

        // True incremental: reuse a prior extract only when the source
        // fingerprint (path + size + mtime) still matches and extract bytes
        // have not been tampered with.
        if let Some(chunk) = try_reuse_cached_extract(
            aicx_home,
            entry,
            &prior_state,
            &session_key,
            &source_allow,
            &repo_path_ignore_fingerprint,
        ) {
            let record = prior_state
                .sessions
                .get(&session_key)
                .expect("reuse requires prior record");
            raw_frames += record.raw_frames;
            signal_frames += record.signal_frames;
            filtered_frames += record.filtered_frames;
            sources_reused += 1;
            next_state.sessions.insert(session_key, record.clone());
            next_state.dispositions.insert(
                session_state_key(&entry.agent, &entry.session_id),
                disposition_record(
                    entry,
                    record.source_len,
                    record.source_mtime_ns,
                    SourceDisposition::Indexed,
                    record.raw_frames,
                    record.signal_frames,
                    record.filtered_frames,
                ),
            );
            chunks.push(chunk);
            continue;
        }

        if let Some(record) = try_reuse_terminal_disposition(
            entry,
            &prior_state,
            &session_key,
            &source_allow,
            &repo_path_ignore_fingerprint,
        ) {
            raw_frames += record.raw_frames;
            signal_frames += record.signal_frames;
            filtered_frames += record.filtered_frames;
            terminal_reused += 1;
            if matches!(record.disposition, SourceDisposition::TerminalSkip { .. }) {
                sources_skipped += 1;
                *skipped_by_agent.entry(entry.agent.clone()).or_default() += 1;
            }
            next_state.dispositions.insert(session_key, record);
            continue;
        }

        // Resolve under approved roots before any parse/open.
        // Pass the catalog string through AsRef<Path> so Path::new lives only
        // inside the allowlist resolver (canonicalize + containment).
        let source_path = match source_allow.resolve_file(entry.source_path.as_str()) {
            Ok(path) => path,
            Err(error) => {
                crate::diagnostics::log_describe(&format!(
                    "source_index_skip agent={} session_id={} path={} error={error:#}",
                    entry.agent, entry.session_id, entry.source_path
                ));
                sources_skipped += 1;
                *skipped_by_agent.entry(entry.agent.clone()).or_default() += 1;
                let (source_len, source_mtime_ns) = catalog_entry_fingerprint(entry);
                let prior_disposition = prior_state.dispositions.get(&session_key);
                let disposition = if recurrent_missing_approved_catalog_source(
                    entry,
                    prior_disposition,
                    &source_allow,
                ) {
                    SourceDisposition::TerminalSkip {
                        reason: TerminalSkipReason::SourceMissing,
                    }
                } else {
                    SourceDisposition::RetryableError {
                        reason: RetryableErrorReason::SourceUnavailable,
                    }
                };
                next_state.dispositions.insert(
                    session_key,
                    disposition_record(entry, source_len, source_mtime_ns, disposition, 0, 0, 0),
                );
                continue;
            }
        };
        let before_fingerprint = crate::catalog::live_source_fingerprint(&source_path)
            .unwrap_or_else(|| resolve_entry_fingerprint(entry, &source_path));
        let mut frames = match parse_catalog_source_outcome(entry, &source_path, &source_allow) {
            Ok(SourceParseOutcome::Parsed(frames)) => frames,
            Ok(SourceParseOutcome::Terminal(reason)) => {
                sources_skipped += 1;
                *skipped_by_agent.entry(entry.agent.clone()).or_default() += 1;
                next_state.dispositions.insert(
                    session_key,
                    disposition_record(
                        entry,
                        before_fingerprint.0,
                        before_fingerprint.1,
                        SourceDisposition::TerminalSkip { reason },
                        0,
                        0,
                        0,
                    ),
                );
                continue;
            }
            Ok(SourceParseOutcome::DeferredLiveChanged) => {
                next_state.dispositions.insert(
                    session_key,
                    disposition_record(
                        entry,
                        before_fingerprint.0,
                        before_fingerprint.1,
                        SourceDisposition::DeferredLiveChanged,
                        0,
                        0,
                        0,
                    ),
                );
                continue;
            }
            Err(error) => {
                crate::diagnostics::log_describe(&format!(
                    "source_index_skip agent={} session_id={} path={} error={error:#}",
                    entry.agent,
                    entry.session_id,
                    source_path.display()
                ));
                sources_skipped += 1;
                *skipped_by_agent.entry(entry.agent.clone()).or_default() += 1;
                next_state.dispositions.insert(
                    session_key,
                    disposition_record(
                        entry,
                        before_fingerprint.0,
                        before_fingerprint.1,
                        SourceDisposition::RetryableError {
                            reason: RetryableErrorReason::ParserError,
                        },
                        0,
                        0,
                        0,
                    ),
                );
                continue;
            }
        };
        sources_parsed += 1;
        let raw_count = frames.len();
        raw_frames += raw_count;
        frames.sort_by_key(|frame| frame.timestamp);
        let before = frames.len();
        frames.retain(is_signal_frame);
        for frame in &mut frames {
            frame.message = clean_message(&frame.message);
        }
        frames.retain(|frame| !frame.message.trim().is_empty());
        drop_ignored_cwd_frames(&mut frames, &ignore);
        let signal_count = frames.len();
        signal_frames += signal_count;
        let filtered_count = before.saturating_sub(frames.len());
        filtered_frames += filtered_count;
        if crate::catalog::live_source_fingerprint(&source_path) != Some(before_fingerprint) {
            next_state.dispositions.insert(
                session_key,
                disposition_record(
                    entry,
                    before_fingerprint.0,
                    before_fingerprint.1,
                    SourceDisposition::DeferredLiveChanged,
                    raw_count,
                    signal_count,
                    filtered_count,
                ),
            );
            continue;
        }
        if frames.is_empty() {
            next_state.dispositions.insert(
                session_key,
                disposition_record(
                    entry,
                    before_fingerprint.0,
                    before_fingerprint.1,
                    SourceDisposition::ZeroSignal,
                    raw_count,
                    signal_count,
                    filtered_count,
                ),
            );
            continue;
        }

        let extract = render_extract(entry, &frames);
        if extract.trim().is_empty() {
            next_state.dispositions.insert(
                session_key,
                disposition_record(
                    entry,
                    before_fingerprint.0,
                    before_fingerprint.1,
                    SourceDisposition::ZeroSignal,
                    raw_count,
                    signal_count,
                    filtered_count,
                ),
            );
            continue;
        }
        let extract_path = extract_path_for(aicx_home, &entry.agent, &entry.session_id);
        if !dry_run
            && cache_extracts
            && write_if_changed(aicx_home, &extract_path, extract.as_bytes())?
        {
            extracts_written += 1;
        }
        let indexed_path = if !dry_run && cache_extracts {
            extract_path.clone()
        } else {
            source_path.to_path_buf()
        };
        let date = frames
            .last()
            .map(|frame| frame.timestamp.format("%Y-%m-%d").to_string())
            .or_else(|| entry.date.clone())
            .unwrap_or_default();
        let project = entry
            .project
            .clone()
            .unwrap_or_else(|| "_unknown".to_string());
        let metadata = serde_json::json!({
            "source_path": indexed_path.to_string_lossy(),
            "project": project,
            "agent": entry.agent,
            "date": date,
            "kind": "conversations",
            "session_id": entry.session_id,
            "logical_session_id": entry.logical_session_id,
            "frame_kind": "conversation",
            "cwd": entry.cwd,
            "source_catalog_path": entry.source_path,
            "preview_lines": extract_preview_lines(&frames),
        });
        chunks.push(aicx_retrieve::ChunkRef {
            id: format!("{}:{}", entry.agent, entry.session_id),
            source_path: indexed_path.display().to_string(),
            text: extract.clone(),
            metadata,
        });
        next_state.dispositions.insert(
            session_key.clone(),
            disposition_record(
                entry,
                before_fingerprint.0,
                before_fingerprint.1,
                SourceDisposition::Indexed,
                raw_count,
                signal_count,
                filtered_count,
            ),
        );

        if cache_extracts {
            let rel = extract_path
                .strip_prefix(aicx_home)
                .map(|p| p.to_path_buf())
                .unwrap_or_else(|_| extract_path.clone());
            // Always stamp LIVE stats into the parse ledger so reuse survives
            // catalog lag (append without catalog rebuild still reuses later
            // once CURRENT has the new extract).
            let (source_len, source_mtime_ns) =
                crate::catalog::live_source_fingerprint(&source_path)
                    .unwrap_or_else(|| resolve_entry_fingerprint(entry, &source_path));
            next_state.sessions.insert(
                session_key,
                SessionParseRecord {
                    source_path: entry.source_path.clone(),
                    source_len,
                    source_mtime_ns,
                    extract_relpath: rel.to_string_lossy().replace('\\', "/"),
                    extract_sha256: sha256_hex(extract.as_bytes()),
                    raw_frames: raw_count,
                    signal_frames: signal_count,
                    filtered_frames: filtered_count,
                    project: entry.project.clone(),
                    date: entry.date.clone().or(Some(date)),
                    cwd: entry.cwd.clone(),
                },
            );
        }
    }

    let accounting = accounting_from_state(&next_state, None, None);
    if !dry_run && project_filters.is_empty() {
        // Persist the complete attempt before publication. Retryable/deferred
        // gaps survive restart, while CURRENT is still untouched.
        write_parse_state(aicx_home, &next_state)?;
    }
    if !accounting.is_complete() {
        anyhow::bail!(
            "source snapshot incomplete: indexed={} zero_signal={} terminal_skip={} retryable_error={} deferred_live_changed={} total={}; CURRENT preserved",
            accounting.indexed,
            accounting.zero_signal,
            accounting.terminal_skip,
            accounting.retryable_error,
            accounting.deferred_live_changed,
            accounting.total
        );
    }
    if chunks.len() != accounting.indexed {
        anyhow::bail!(
            "source accounting mismatch: {} indexed disposition(s), {} lexical document(s); CURRENT preserved",
            accounting.indexed,
            chunks.len()
        );
    }
    if chunks.is_empty() {
        anyhow::bail!(
            "source-driven index produced zero expected documents from {} fully accounted source(s); CURRENT preserved",
            accounting.total
        );
    }

    let (manifest_path, dense_docs, dense_kind) = if dry_run {
        (
            None,
            0usize,
            if semantic {
                "would_build_semantic".to_string()
            } else {
                "optional_not_built".to_string()
            },
        )
    } else if semantic {
        let (dense_chunks, fingerprint) = embed_chunks_for_semantic(&chunks)?;
        let manifest = crate::vector_index::publish_source_hybrid_generation(
            &chunks,
            &dense_chunks,
            &source_fingerprint,
            &fingerprint,
        )?;
        let path = crate::vector_index::hybrid_manifest_path(None)?
            .display()
            .to_string();
        (
            Some(path).filter(|_| manifest.lexical_doc_count == chunks.len()),
            manifest.dense_count,
            manifest.dense_kind,
        )
    } else {
        let manifest =
            crate::vector_index::publish_source_lexical_generation(&chunks, &source_fingerprint)?;
        let path = crate::vector_index::hybrid_manifest_path(None)?
            .display()
            .to_string();
        (
            Some(path).filter(|_| manifest.lexical_doc_count == chunks.len()),
            0,
            "optional_not_built".to_string(),
        )
    };

    Ok(SourceIndexReport {
        catalog_path: catalog_path.display().to_string(),
        sources_total: selected.len(),
        sources_parsed,
        sources_reused,
        terminal_reused,
        sources_skipped,
        zero_signal_sources: accounting.zero_signal,
        terminal_skip_sources: accounting.terminal_skip,
        retryable_error_sources: accounting.retryable_error,
        deferred_live_changed_sources: accounting.deferred_live_changed,
        raw_frames,
        signal_frames,
        filtered_frames,
        extracts_written,
        lexical_docs: chunks.len(),
        dense_docs,
        dense_kind,
        semantic_requested: semantic,
        unchanged: false,
        wall_ms: started.elapsed().as_millis() as u64,
        manifest_path,
        skipped_by_agent,
    })
}

fn current_dense_stats() -> (String, usize) {
    let Ok(path) = crate::vector_index::hybrid_manifest_path(None) else {
        return ("missing".to_string(), 0);
    };
    if !path.is_file() {
        return ("missing".to_string(), 0);
    }
    match aicx_retrieve::Manifest::read_from_path(&path) {
        Ok(m) => (m.dense_kind, m.dense_count),
        Err(_) => ("unreadable".to_string(), 0),
    }
}

/// Embed session extracts for `--semantic` using the configured cloud/native engine.
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn embed_chunks_for_semantic(
    chunks: &[aicx_retrieve::ChunkRef],
) -> Result<(
    Vec<aicx_retrieve::DenseChunkRef>,
    aicx_retrieve::EmbedderFingerprint,
)> {
    let mut engine = crate::embedder::EmbeddingEngine::new().with_context(|| {
        "initialize embedder for `aicx index --semantic` (configure ~/.aicx/config.toml \
         [embedder.cloud] or native GGUF, then `aicx warmup`)"
            .to_string()
    })?;
    let info = engine.info().clone();
    let fingerprint = crate::vector_index::hybrid_embedder_fingerprint(&info);
    let batch_size = engine.embed_batch_size().max(1);
    let mut dense_chunks = Vec::with_capacity(chunks.len());
    let total = chunks.len();
    for (batch_idx, batch) in chunks.chunks(batch_size).enumerate() {
        let texts: Vec<String> = batch
            .iter()
            .map(|chunk| {
                // Bound embed payload: first ~8k chars keeps signal, avoids multi-MB HTTP.
                let text = chunk.text.as_str();
                if text.len() > 8_192 {
                    text.chars().take(8_192).collect()
                } else {
                    text.to_string()
                }
            })
            .collect();
        let vectors = engine.embed_batch(&texts).with_context(|| {
            format!(
                "embed batch {}/{} ({} texts) for index --semantic",
                batch_idx + 1,
                total.div_ceil(batch_size),
                texts.len()
            )
        })?;
        if vectors.len() != batch.len() {
            anyhow::bail!(
                "embedder returned {} vectors for {} texts in batch {}",
                vectors.len(),
                batch.len(),
                batch_idx + 1
            );
        }
        for (chunk, embedding) in batch.iter().zip(vectors) {
            if embedding.len() != fingerprint.dim {
                anyhow::bail!(
                    "embedder returned dim {} for chunk {}; config expects {}",
                    embedding.len(),
                    chunk.id,
                    fingerprint.dim
                );
            }
            dense_chunks.push(aicx_retrieve::DenseChunkRef {
                chunk: chunk.clone(),
                embedding,
            });
        }
        if batch_idx == 0 || (batch_idx + 1) % 10 == 0 || (batch_idx + 1) * batch_size >= total {
            eprintln!(
                "aicx index --semantic · embedded {}/{} session document(s)",
                dense_chunks.len(),
                total
            );
        }
    }
    Ok((dense_chunks, fingerprint))
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn embed_chunks_for_semantic(
    _chunks: &[aicx_retrieve::ChunkRef],
) -> Result<(
    Vec<aicx_retrieve::DenseChunkRef>,
    aicx_retrieve::EmbedderFingerprint,
)> {
    anyhow::bail!(
        "`aicx index --semantic` requires a build with `cloud-embedder` and/or `native-embedder` \
         features (default release builds include both)"
    )
}

/// Compare the durable catalog with the document identities actually committed
/// to CURRENT. This intentionally scans the exact id term dictionary rather
/// than trusting only the manifest count: the count alone cannot say which
/// extractor has holes.
pub fn current_search_coverage(aicx_home: &Path) -> SearchCoverage {
    let entries = match crate::catalog::read_entries_at(aicx_home) {
        Ok(entries) => entries,
        Err(_) => {
            return SearchCoverage {
                skipped: BTreeMap::from([("catalog_unreadable".to_string(), 1)]),
                ..SearchCoverage::default()
            };
        }
    };
    let total_sessions = entries.len();
    let indexed = match current_indexed_session_keys() {
        Ok(indexed) => indexed,
        Err(_) => {
            let mut skipped = BTreeMap::new();
            for entry in &entries {
                *skipped
                    .entry(format!(
                        "{}_index_unreadable",
                        coverage_agent_key(entry.agent.as_str())
                    ))
                    .or_default() += 1;
            }
            return SearchCoverage {
                scanned_sessions: 0,
                total_sessions,
                skipped,
            };
        }
    };
    let mut skipped = BTreeMap::new();
    let mut scanned_sessions = 0usize;

    for entry in &entries {
        if indexed.contains(&session_state_key(&entry.agent, &entry.session_id)) {
            scanned_sessions += 1;
        } else {
            *skipped
                .entry(format!(
                    "{}_unindexed",
                    coverage_agent_key(entry.agent.as_str())
                ))
                .or_default() += 1;
        }
    }
    let indexed_orphans = indexed.len().saturating_sub(scanned_sessions);
    if indexed_orphans > 0 {
        skipped.insert("index_orphaned".to_string(), indexed_orphans);
    }

    SearchCoverage {
        scanned_sessions,
        total_sessions,
        skipped,
    }
}

fn current_indexed_session_keys() -> Result<HashSet<String>> {
    let hybrid_root = crate::vector_index::hybrid_root_dir(None)?;
    let generation = crate::vector_index::resolve_hybrid_generation_dir(&hybrid_root);
    if !generation.join("manifest.json").is_file() {
        return Ok(HashSet::new());
    }
    let lexical = aicx_retrieve::TantivyAdapter::new(generation)?;
    Ok(lexical.scan_chunk_ids()?.into_iter().collect())
}

fn coverage_agent_key(agent: &str) -> String {
    let normalized: String = agent
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() {
                ch.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() {
        "unknown".to_string()
    } else {
        normalized.to_string()
    }
}

/// Search every matching passage inside one catalog session.
///
/// Cached extracts are read first. When no cache exists, the source is parsed
/// through the same signal-only parser and renderer used by `aicx index`; this
/// fallback is read-only and never materializes a cache file.
pub fn search_session_passages(
    aicx_home: &Path,
    session: &str,
    query: &str,
    context: usize,
    literal: bool,
) -> Result<SessionPassageReport> {
    if query.trim().is_empty() {
        anyhow::bail!("session passage query must not be empty");
    }
    let entry = crate::catalog::resolve_session(aicx_home, session)?.ok_or_else(|| {
        anyhow::anyhow!(
            "session `{session}` not in durable catalog; run `aicx catalog rebuild` first"
        )
    })?;
    let document = read_session_document(aicx_home, &entry)?;
    let hit_lines = matching_line_numbers(&document.body, query, literal)?;
    let spans = merge_context_spans(&hit_lines, document.body.lines().count(), context);
    let lines: Vec<&str> = document.body.lines().collect();
    let source_path = document.source_path;
    let document_path = document.document_path.display().to_string();
    let passages = spans
        .into_iter()
        .enumerate()
        .map(|(index, (start, end, match_lines))| SessionPassage {
            passage: index + 1,
            line_span: LineSpan { start, end },
            match_lines,
            text: lines[start - 1..end].join("\n"),
            source_path: source_path.clone(),
            document_path: document_path.clone(),
        })
        .collect();

    Ok(SessionPassageReport {
        session_id: entry.session_id,
        agent: entry.agent.clone(),
        query: query.to_string(),
        mode: if literal { "literal" } else { "token" },
        context,
        cache_hit: document.cache_hit,
        passages,
        coverage: SearchCoverage::single_session(true, &entry.agent),
    })
}

fn read_session_document(aicx_home: &Path, entry: &CatalogEntry) -> Result<SessionDocument> {
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let ignore = crate::legacy_archive::load_repo_path_ignore(aicx_home, &user_home)?;
    let repo_path_ignore_fingerprint = ignore.fingerprint();
    let cache_path = extract_path_for(aicx_home, &entry.agent, &entry.session_id);
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    let prior_state = load_parse_state(aicx_home, &repo_path_ignore_fingerprint);
    let session_key = session_state_key(&entry.agent, &entry.session_id);
    if let Some(chunk) = try_reuse_cached_extract(
        aicx_home,
        entry,
        &prior_state,
        &session_key,
        &source_allow,
        &repo_path_ignore_fingerprint,
    ) {
        return Ok(SessionDocument {
            body: chunk.text,
            source_path: entry.source_path.clone(),
            document_path: cache_path,
            cache_hit: true,
        });
    }
    let source_path = source_allow
        .resolve_file(entry.source_path.as_str())
        .with_context(|| {
            format!(
                "resolve catalog source agent={} session_id={}",
                entry.agent, entry.session_id
            )
        })?;
    let mut frames = parse_catalog_source(entry, &source_path, &source_allow)?;
    frames.sort_by_key(|frame| frame.timestamp);
    frames.retain(is_signal_frame);
    for frame in &mut frames {
        frame.message = clean_message(&frame.message);
    }
    frames.retain(|frame| !frame.message.trim().is_empty());
    drop_ignored_cwd_frames(&mut frames, &ignore);
    let body = render_extract(entry, &frames);
    Ok(SessionDocument {
        body,
        source_path: entry.source_path.clone(),
        document_path: source_path,
        cache_hit: false,
    })
}

fn matching_line_numbers(body: &str, query: &str, literal: bool) -> Result<Vec<usize>> {
    if literal {
        return Ok(body
            .lines()
            .enumerate()
            .filter(|(_, line)| contains_boundary_literal(line, query))
            .map(|(index, _)| index + 1)
            .collect());
    }

    let query_tokens = lexical_tokens(query);
    if query_tokens.is_empty() {
        anyhow::bail!("token query must contain at least one letter or digit");
    }
    Ok(body
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let line_tokens: HashSet<String> = lexical_tokens(line).into_iter().collect();
            query_tokens.iter().any(|token| line_tokens.contains(token))
        })
        .map(|(index, _)| index + 1)
        .collect())
}

fn lexical_tokens(value: &str) -> Vec<String> {
    value
        .split(|ch: char| !ch.is_alphanumeric())
        .filter(|token| !token.is_empty())
        .map(|token| token.to_lowercase())
        .collect()
}

fn contains_boundary_literal(line: &str, query: &str) -> bool {
    let Some(first) = query.chars().next() else {
        return false;
    };
    let Some(last) = query.chars().next_back() else {
        return false;
    };
    line.match_indices(query).any(|(start, matched)| {
        let end = start + matched.len();
        let before = line[..start].chars().next_back();
        let after = line[end..].chars().next();
        (!is_identifier_char(first) || before.is_none_or(|ch| !is_identifier_char(ch)))
            && (!is_identifier_char(last) || after.is_none_or(|ch| !is_identifier_char(ch)))
    })
}

fn is_identifier_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn merge_context_spans(
    hit_lines: &[usize],
    total_lines: usize,
    context: usize,
) -> Vec<(usize, usize, Vec<usize>)> {
    let mut spans: Vec<(usize, usize, Vec<usize>)> = Vec::new();
    for &line in hit_lines {
        let start = line.saturating_sub(context).max(1);
        let end = line.saturating_add(context).min(total_lines);
        if let Some((_, previous_end, previous_hits)) = spans.last_mut()
            && start <= previous_end.saturating_add(1)
        {
            *previous_end = (*previous_end).max(end);
            previous_hits.push(line);
        } else {
            spans.push((start, end, vec![line]));
        }
    }
    spans
}

fn parse_catalog_source(
    entry: &CatalogEntry,
    path: &Path,
    allow: &crate::source_path::SourceAllowlist,
) -> Result<Vec<TimelineEntry>> {
    match parse_catalog_source_outcome(entry, path, allow)? {
        SourceParseOutcome::Parsed(frames) => Ok(frames),
        SourceParseOutcome::Terminal(reason) => {
            anyhow::bail!("source has typed terminal disposition: {reason:?}")
        }
        SourceParseOutcome::DeferredLiveChanged => {
            anyhow::bail!("source parse is deferred while the source is actively changing")
        }
    }
}

fn parse_catalog_source_outcome(
    entry: &CatalogEntry,
    path: &Path,
    allow: &crate::source_path::SourceAllowlist,
) -> Result<SourceParseOutcome> {
    // Canonicalize + prove containment under approved source roots before any open.
    let path = allow
        .resolve_file(path)
        .with_context(|| format!("resolve catalog source {}", path.display()))?;

    if entry.agent == "vibecrafted" {
        let bytes = allow
            .read_bytes(&path)
            .with_context(|| format!("read runtime transcript {}", path.display()))?;
        let body = decode_vibecrafted_transcript(&bytes)
            .with_context(|| format!("decode runtime transcript {}", path.display()))?;
        // Token-stream runtime_runs logs interleave thought fragments with
        // visible text. Indexing the raw body made search surface
        // `{"type":"thought","data":"The"}` spam over real operator answers.
        let message = vibecrafted_signal_body(body);
        if message.trim().is_empty() {
            return Ok(SourceParseOutcome::Parsed(Vec::new()));
        }
        let timestamp = fs::metadata(&path)
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(chrono::DateTime::<chrono::Utc>::from)
            .unwrap_or_else(chrono::Utc::now);
        return Ok(SourceParseOutcome::Parsed(vec![TimelineEntry {
            timestamp,
            agent: entry.agent.clone(),
            session_id: entry.session_id.clone(),
            role: "assistant".to_string(),
            message,
            frame_kind: Some(FrameKind::AgentReply),
            branch: None,
            cwd: entry.cwd.clone(),
            timestamp_source: Some("source_mtime".to_string()),
            source_path: Some(entry.source_path.clone()),
            source_sha256: None,
            source_line_span: None,
        }]));
    }

    let source_bytes = fs::metadata(&path)
        .with_context(|| format!("stat source {}", path.display()))?
        .len();
    if entry.agent == "codex" && source_bytes > MAX_FULL_PARSE_BYTES {
        return parse_large_codex_signal(entry, &path, allow).map(SourceParseOutcome::Parsed);
    }
    if source_bytes > MAX_FULL_PARSE_BYTES {
        return Ok(SourceParseOutcome::Terminal(
            TerminalSkipReason::OversizedUnsupportedSource,
        ));
    }

    let agent = match entry.agent.as_str() {
        "claude" => aicx_parser::engine::AgentKind::Claude,
        "codex" => aicx_parser::engine::AgentKind::Codex,
        "gemini" => aicx_parser::engine::AgentKind::Gemini,
        "grok" => aicx_parser::engine::AgentKind::Grok,
        "junie" => aicx_parser::engine::AgentKind::Junie,
        _ => {
            return Ok(SourceParseOutcome::Terminal(
                TerminalSkipReason::UnsupportedAgent,
            ));
        }
    };
    let handle = crate::parser_dispatch::source_handle_for_file(
        agent,
        &entry.session_id,
        entry.logical_session_id.clone(),
        &path,
    )?;
    match aicx_parser::engine::ParserEngine::default().parse_registered(&handle)? {
        aicx_parser::engine::ValidatedParse::Session(parsed) => Ok(SourceParseOutcome::Parsed(
            crate::output::timeline_entries_from_model(parsed.model()),
        )),
        aicx_parser::engine::ValidatedParse::Fatal(_) => Ok(fatal_parse_outcome(&path)),
    }
}

/// Accept an approved-root `NotFound` as terminal only after the same catalog
/// identity was durably observed missing by an earlier incremental attempt.
fn recurrent_missing_approved_catalog_source(
    entry: &CatalogEntry,
    prior: Option<&SourceDispositionRecord>,
    allow: &crate::source_path::SourceAllowlist,
) -> bool {
    let candidate = Path::new(&entry.source_path);
    let (Some(source_len), Some(source_mtime_ns)) = (entry.source_len, entry.source_mtime_ns)
    else {
        return false;
    };
    candidate.is_absolute()
        && !candidate
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        && allow.roots().iter().any(|root| candidate.starts_with(root))
        && matches!(fs::metadata(candidate), Err(error) if error.kind() == std::io::ErrorKind::NotFound)
        && prior.is_some_and(|record| {
            record.source_path == entry.source_path
                && record.source_len == source_len
                && record.source_mtime_ns == source_mtime_ns
                && matches!(
                    record.disposition,
                    SourceDisposition::RetryableError {
                        reason: RetryableErrorReason::SourceUnavailable
                    } | SourceDisposition::TerminalSkip {
                        reason: TerminalSkipReason::SourceMissing
                    }
                )
        })
}

fn decode_vibecrafted_transcript(bytes: &[u8]) -> Result<&str> {
    if let Ok(body) = std::str::from_utf8(bytes) {
        return Ok(body);
    }
    if bytes.len() != VIBECRAFTED_RETENTION_BYTES {
        return std::str::from_utf8(bytes).map_err(Into::into);
    }
    let prefix = bytes
        .iter()
        .take(3)
        .take_while(|byte| **byte & 0b1100_0000 == 0b1000_0000)
        .count();
    if prefix == 0 {
        return std::str::from_utf8(bytes).map_err(Into::into);
    }
    std::str::from_utf8(&bytes[prefix..]).map_err(Into::into)
}

fn fatal_parse_outcome(path: &Path) -> SourceParseOutcome {
    if source_is_recently_modified(path) {
        SourceParseOutcome::DeferredLiveChanged
    } else {
        SourceParseOutcome::Terminal(TerminalSkipReason::ParserFatal)
    }
}

fn source_is_recently_modified(path: &Path) -> bool {
    let Some(modified) = path
        .metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
    else {
        return false;
    };
    SystemTime::now()
        .duration_since(modified)
        .map_or(true, |age| age <= IN_FLIGHT_FATAL_GRACE)
}

/// Read one cataloged session through the same allowlisted, signal-only parser
/// used by the lexical index.
///
/// Intent retrieval uses this path directly instead of reconstructing evidence
/// from retired per-frame cards. The returned path is canonical and proven to
/// live below one of the operator-approved source roots before any source open.
pub(crate) fn read_catalog_signal_at(
    aicx_home: &Path,
    entry: &CatalogEntry,
    frame_kind: FrameKind,
) -> Result<(PathBuf, Vec<TimelineEntry>)> {
    let (source_path, mut frames) = read_catalog_conversation_at(aicx_home, entry)?;
    frames.retain(|frame| frame_matches_kind(frame, frame_kind));
    Ok((source_path, frames))
}

/// Read one cataloged session as the clean user/assistant conversation used by
/// current operator surfaces. System prompts, tool payloads, and thought
/// frames are removed before callers build previews or retrieval records.
pub(crate) fn read_catalog_conversation_at(
    aicx_home: &Path,
    entry: &CatalogEntry,
) -> Result<(PathBuf, Vec<TimelineEntry>)> {
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    let source_path = source_allow
        .resolve_file(entry.source_path.as_str())
        .with_context(|| {
            format!(
                "resolve catalog source agent={} session_id={}",
                entry.agent, entry.session_id
            )
        })?;
    let mut frames = parse_catalog_source(entry, &source_path, &source_allow)?;
    frames.sort_by_key(|frame| frame.timestamp);
    frames.retain(is_signal_frame);
    for frame in &mut frames {
        frame.message = clean_message(&frame.message);
    }
    frames.retain(|frame| !frame.message.trim().is_empty());
    let ignore = crate::legacy_archive::load_repo_path_ignore(aicx_home, &user_home)?;
    drop_ignored_cwd_frames(&mut frames, &ignore);
    Ok((source_path, frames))
}

fn drop_ignored_cwd_frames(
    frames: &mut Vec<TimelineEntry>,
    ignore: &crate::legacy_archive::RepoPathIgnoreMatcher,
) {
    if ignore.is_empty() {
        return;
    }
    frames.retain(|frame| !ignore.ignores_cwd(frame.cwd.as_deref()));
}

fn frame_matches_kind(frame: &TimelineEntry, requested: FrameKind) -> bool {
    frame.frame_kind.unwrap_or(match frame.role.as_str() {
        "user" => FrameKind::UserMsg,
        "assistant" => FrameKind::AgentReply,
        _ => FrameKind::SystemNote,
    }) == requested
}

/// Bounded signal-only reader for oversized Codex rollouts.
///
/// Historical rollouts can exceed hundreds of MB because tool results and
/// pasted artifacts share the JSONL. The full canonical projection pays for
/// all of that noise. This path drains over-cap records without allocating
/// them and deserializes only bounded message records.
fn parse_large_codex_signal(
    entry: &CatalogEntry,
    path: &Path,
    allow: &crate::source_path::SourceAllowlist,
) -> Result<Vec<TimelineEntry>> {
    // `path` is already resolve_file'd by the caller; open through the allowlist.
    let file = allow
        .open_file(path)
        .with_context(|| format!("open source {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut frames = Vec::new();
    let mut line_no = 0u64;
    let mut current_cwd = entry.cwd.clone();
    while let Some(record) = crate::sanitize::read_line_capped(&mut reader, MAX_JSONL_RECORD_BYTES)?
    {
        line_no += 1;
        if record.exceeded || record.line.trim().is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&record.line) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) == Some("turn_context") {
            if let Some(cwd) = value
                .get("payload")
                .and_then(|payload| payload.get("cwd"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|cwd| !cwd.is_empty())
            {
                current_cwd = Some(cwd.to_string());
            }
            continue;
        }
        if value.get("type").and_then(serde_json::Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(payload) = value.get("payload") else {
            continue;
        };
        if payload.get("type").and_then(serde_json::Value::as_str) != Some("message") {
            continue;
        }
        let Some(role) = payload.get("role").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let frame_kind = match role {
            "user" => FrameKind::UserMsg,
            "assistant" => FrameKind::AgentReply,
            _ => continue,
        };
        let message = payload
            .get("content")
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|item| {
                let kind = item.get("type")?.as_str()?;
                if !matches!(kind, "input_text" | "output_text" | "text") {
                    return None;
                }
                item.get("text")?.as_str()
            })
            .collect::<Vec<_>>()
            .join("\n");
        if message.trim().is_empty() {
            continue;
        }
        let timestamp = value
            .get("timestamp")
            .and_then(serde_json::Value::as_str)
            .and_then(|raw| chrono::DateTime::parse_from_rfc3339(raw).ok())
            .map(|timestamp| timestamp.with_timezone(&chrono::Utc))
            .unwrap_or_else(chrono::Utc::now);
        frames.push(TimelineEntry {
            timestamp,
            agent: entry.agent.clone(),
            session_id: entry.session_id.clone(),
            role: role.to_string(),
            message,
            frame_kind: Some(frame_kind),
            branch: None,
            cwd: current_cwd.clone(),
            timestamp_source: Some("record".to_string()),
            source_path: Some(entry.source_path.clone()),
            source_sha256: None,
            source_line_span: Some((line_no, line_no)),
        });
    }
    Ok(frames)
}

fn is_signal_frame(frame: &TimelineEntry) -> bool {
    let signal_kind = match frame.frame_kind {
        Some(FrameKind::UserMsg | FrameKind::AgentReply) => true,
        Some(FrameKind::ToolCall | FrameKind::InternalThought | FrameKind::SystemNote) => false,
        None => matches!(frame.role.as_str(), "user" | "assistant"),
    };
    signal_kind
        && !crate::extraction::is_harness_injected_noise(&frame.role, &frame.message)
        && !looks_like_binary_payload(&frame.message)
}

/// Collapse a vibecrafted `runtime_runs/*/transcript.log` into indexable text.
///
/// Keeps visible `text` tokens and nested `agent_message` bodies; drops pure
/// `thought` token streams. Non-JSON lines (plain markdown transcripts) pass
/// through unchanged.
fn vibecrafted_signal_body(body: &str) -> String {
    let mut out = String::new();
    let mut saw_json_line = false;
    for line in body.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            if !saw_json_line {
                out.push_str(line);
                out.push('\n');
            }
            continue;
        };
        saw_json_line = true;
        let Some(ty) = value.get("type").and_then(serde_json::Value::as_str) else {
            continue;
        };
        match ty {
            "thought" => continue,
            "text" => {
                if let Some(data) = value.get("data").and_then(serde_json::Value::as_str) {
                    out.push_str(data);
                }
            }
            "item.completed" => {
                if let Some(item) = value.get("item") {
                    let item_ty = item.get("type").and_then(serde_json::Value::as_str);
                    if matches!(item_ty, Some("agent_message") | Some("message"))
                        && let Some(text) = item.get("text").and_then(serde_json::Value::as_str)
                    {
                        if !out.is_empty() && !out.ends_with('\n') {
                            out.push('\n');
                        }
                        out.push_str(text);
                        out.push('\n');
                    }
                }
            }
            "agent_message" | "message" => {
                if let Some(text) = value.get("text").and_then(serde_json::Value::as_str) {
                    if !out.is_empty() && !out.ends_with('\n') {
                        out.push('\n');
                    }
                    out.push_str(text);
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
    clean_message(&out)
}

fn looks_like_binary_payload(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    if lower.contains("data:image/") && lower.contains(";base64,") {
        return true;
    }
    message
        .split_whitespace()
        .any(|token| token.chars().count() > MAX_UNBROKEN_TOKEN_CHARS)
}

fn clean_message(message: &str) -> String {
    let message = strip_known_harness_blocks(message);
    let mut cleaned = String::new();
    for line in message.lines() {
        if line.chars().count() > MAX_UNBROKEN_TOKEN_CHARS
            || (line.to_ascii_lowercase().contains("base64")
                && line.chars().count() > MAX_UNBROKEN_TOKEN_CHARS / 2)
        {
            continue;
        }
        if cleaned.chars().count() + line.chars().count() + 1 > MAX_MESSAGE_CHARS {
            cleaned.push_str("\n[message truncated by source index]\n");
            break;
        }
        cleaned.push_str(line);
        cleaned.push('\n');
    }
    cleaned.trim().to_string()
}

fn strip_known_harness_blocks(message: &str) -> String {
    let mut cleaned = message.to_string();
    for tag in ["user_info", "git_status"] {
        let opening = format!("<{tag}>");
        let closing = format!("</{tag}>");
        while let Some(start) = cleaned.find(&opening) {
            let content_start = start + opening.len();
            let Some(relative_end) = cleaned[content_start..].find(&closing) else {
                break;
            };
            let end = content_start + relative_end + closing.len();
            cleaned.replace_range(start..end, "");
        }
    }
    cleaned
}

fn render_extract(entry: &CatalogEntry, frames: &[TimelineEntry]) -> String {
    let mut out = format!(
        "# AICX session extract\n\n- session: `{}`\n- agent: `{}`\n- project: `{}`\n- source: `{}`\n\n",
        entry.session_id,
        entry.agent,
        entry.project.as_deref().unwrap_or("_unknown"),
        entry.source_path
    );
    for frame in frames {
        let role = if frame.role == "user" {
            "user"
        } else {
            "assistant"
        };
        let header = format!(
            "## {} · {}\n\n",
            frame.timestamp.to_rfc3339_opts(SecondsFormat::Millis, true),
            role
        );
        if out.chars().count() + header.chars().count() + frame.message.chars().count()
            > MAX_EXTRACT_CHARS
        {
            out.push_str("\n[session extract truncated by source index]\n");
            break;
        }
        out.push_str(&header);
        out.push_str(frame.message.trim());
        out.push_str("\n\n");
    }
    out
}

fn extract_preview_lines(frames: &[TimelineEntry]) -> Vec<String> {
    frames
        .iter()
        .flat_map(|frame| frame.message.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .take(6)
        .map(|line| line.chars().take(240).collect())
        .collect()
}

fn project_selected(project: Option<&str>, filters: &[String]) -> bool {
    filters.is_empty()
        || project.is_some_and(|project| {
            filters
                .iter()
                .any(|filter| project.eq_ignore_ascii_case(filter))
        })
}

fn session_state_key(agent: &str, session_id: &str) -> String {
    format!("{agent}:{session_id}")
}

fn parse_state_path(aicx_home: &Path) -> PathBuf {
    aicx_home.join(PARSE_STATE_RELPATH)
}

fn disposition_record(
    entry: &CatalogEntry,
    source_len: u64,
    source_mtime_ns: u64,
    disposition: SourceDisposition,
    raw_frames: usize,
    signal_frames: usize,
    filtered_frames: usize,
) -> SourceDispositionRecord {
    SourceDispositionRecord {
        source_path: entry.source_path.clone(),
        source_len,
        source_mtime_ns,
        project: entry.project.clone(),
        disposition,
        raw_frames,
        signal_frames,
        filtered_frames,
    }
}

fn catalog_entry_fingerprint(entry: &CatalogEntry) -> (u64, u64) {
    (
        entry.source_len.unwrap_or(0),
        entry.source_mtime_ns.unwrap_or(0),
    )
}

fn accounting_from_state(
    state: &SourceParseState,
    project: Option<&str>,
    snapshot_matches: Option<bool>,
) -> SourceAccounting {
    let project = project.map(str::trim).filter(|value| !value.is_empty());
    let mut accounting = SourceAccounting {
        source_fingerprint: state.source_fingerprint.clone(),
        snapshot_matches,
        ..SourceAccounting::default()
    };
    for record in state.dispositions.values().filter(|record| {
        project.is_none_or(|filter| {
            record
                .project
                .as_deref()
                .is_some_and(|value| value.eq_ignore_ascii_case(filter))
        })
    }) {
        accounting.total += 1;
        match record.disposition {
            SourceDisposition::Indexed => accounting.indexed += 1,
            SourceDisposition::ZeroSignal => accounting.zero_signal += 1,
            SourceDisposition::TerminalSkip { .. } => accounting.terminal_skip += 1,
            SourceDisposition::RetryableError { .. } => accounting.retryable_error += 1,
            SourceDisposition::DeferredLiveChanged => accounting.deferred_live_changed += 1,
        }
    }
    accounting
}

/// Read durable source accounting and independently verify its live snapshot.
///
/// `None` is explicit legacy compatibility: CURRENT predates the disposition
/// ledger, so status must not reconstruct expected documents by subtracting
/// catalog rows from lexical documents.
pub(crate) fn current_source_accounting_at(
    aicx_home: &Path,
    project: Option<&str>,
) -> Option<SourceAccounting> {
    let entries = crate::catalog::read_entries_at(aicx_home).ok()?;
    if entries.is_empty() {
        return None;
    }
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let ignore = crate::legacy_archive::load_repo_path_ignore(aicx_home, &user_home).ok()?;
    let ignore_fingerprint = ignore.fingerprint();
    let state = load_parse_state(aicx_home, &ignore_fingerprint);
    if state.dispositions.is_empty() || state.source_fingerprint.is_empty() {
        return None;
    }
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    let records_match = entries.len() == state.dispositions.len()
        && entries.iter().all(|entry| {
            state
                .dispositions
                .get(&session_state_key(&entry.agent, &entry.session_id))
                .is_some_and(|record| disposition_record_matches(entry, record, &source_allow))
        });
    let catalog_path = crate::catalog::sessions_path_for(aicx_home);
    let current_fingerprint = source_fingerprint(
        aicx_home,
        &catalog_path,
        &entries,
        &source_allow,
        &ignore_fingerprint,
    )
    .ok();
    let snapshot_matches =
        records_match && current_fingerprint.as_deref() == Some(state.source_fingerprint.as_str());
    let mut accounting = accounting_from_state(&state, project, Some(snapshot_matches));
    accounting.source_drift = entries
        .iter()
        .filter(|entry| {
            !state
                .dispositions
                .get(&session_state_key(&entry.agent, &entry.session_id))
                .is_some_and(|record| disposition_record_matches(entry, record, &source_allow))
        })
        .count()
        + state.dispositions.len().saturating_sub(entries.len());
    Some(accounting)
}

#[cfg(test)]
pub(crate) fn current_source_fingerprint_at(aicx_home: &Path) -> Result<Option<String>> {
    let entries = crate::catalog::read_entries_at(aicx_home)?;
    if entries.is_empty() {
        return Ok(None);
    }
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let ignore = crate::legacy_archive::load_repo_path_ignore(aicx_home, &user_home)?;
    let source_allow = crate::source_path::SourceAllowlist::for_operator(&user_home, aicx_home);
    let catalog_path = crate::catalog::sessions_path_for(aicx_home);
    source_fingerprint(
        aicx_home,
        &catalog_path,
        &entries,
        &source_allow,
        &ignore.fingerprint(),
    )
    .map(Some)
}

fn load_parse_state(aicx_home: &Path, repo_path_ignore_fingerprint: &str) -> SourceParseState {
    let path = parse_state_path(aicx_home);
    if !path.is_file() {
        return SourceParseState::default();
    }
    let Ok(raw) = crate::source_path::read_under_aicx_home(aicx_home, &path) else {
        return SourceParseState::default();
    };
    let Ok(mut state) = serde_json::from_str::<SourceParseState>(&raw) else {
        return SourceParseState::default();
    };
    if state.schema != PARSE_STATE_SCHEMA
        || state.signal_filter_version != SIGNAL_FILTER_VERSION
        || (!state.disposition_policy_version.is_empty()
            && state.disposition_policy_version != DISPOSITION_POLICY_VERSION)
        || state.repo_path_ignore_fingerprint != repo_path_ignore_fingerprint
    {
        return SourceParseState::default();
    }
    // Additive compatibility: old ledgers recorded only indexed extracts.
    // They remain reusable, but source accounting stays explicitly legacy
    // until a fresh build stamps a whole-snapshot fingerprint.
    for (session_key, record) in &state.sessions {
        state
            .dispositions
            .entry(session_key.clone())
            .or_insert_with(|| SourceDispositionRecord {
                source_path: record.source_path.clone(),
                source_len: record.source_len,
                source_mtime_ns: record.source_mtime_ns,
                project: record.project.clone(),
                disposition: SourceDisposition::Indexed,
                raw_frames: record.raw_frames,
                signal_frames: record.signal_frames,
                filtered_frames: record.filtered_frames,
            });
    }
    state
}

fn write_parse_state(aicx_home: &Path, state: &SourceParseState) -> Result<()> {
    let path = parse_state_path(aicx_home);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create parse-state dir {}", parent.display()))?;
    }
    let body = serde_json::to_vec_pretty(state).context("serialize source parse state")?;
    let write_path = crate::sanitize::validate_write_path(&path)
        .with_context(|| format!("validate parse-state path {}", path.display()))?;
    let tmp = write_path.with_extension("json.tmp");
    let tmp_write = crate::sanitize::validate_write_path(&tmp)
        .with_context(|| format!("validate parse-state tmp {}", tmp.display()))?;
    fs::write(&tmp_write, body)
        .with_context(|| format!("write parse-state tmp {}", tmp_write.display()))?;
    fs::rename(&tmp_write, &write_path).with_context(|| {
        format!(
            "publish parse-state {} -> {}",
            tmp_write.display(),
            write_path.display()
        )
    })?;
    Ok(())
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

fn disposition_record_matches(
    entry: &CatalogEntry,
    record: &SourceDispositionRecord,
    source_allow: &crate::source_path::SourceAllowlist,
) -> bool {
    if record.source_path != entry.source_path
        || record.source_len == 0
        || record.source_mtime_ns == 0
    {
        return false;
    }
    let Ok(live_path) = source_allow.resolve_file(entry.source_path.as_str()) else {
        return false;
    };
    crate::catalog::live_source_fingerprint(&live_path).is_some_and(|(live_len, live_mtime)| {
        live_len == record.source_len && live_mtime == record.source_mtime_ns
    })
}

fn try_reuse_terminal_disposition(
    entry: &CatalogEntry,
    prior: &SourceParseState,
    session_key: &str,
    source_allow: &crate::source_path::SourceAllowlist,
    repo_path_ignore_fingerprint: &str,
) -> Option<SourceDispositionRecord> {
    if prior.signal_filter_version != SIGNAL_FILTER_VERSION
        || prior.disposition_policy_version != DISPOSITION_POLICY_VERSION
        || prior.repo_path_ignore_fingerprint != repo_path_ignore_fingerprint
    {
        return None;
    }
    let record = prior.dispositions.get(session_key)?;
    if !matches!(
        record.disposition,
        SourceDisposition::ZeroSignal | SourceDisposition::TerminalSkip { .. }
    ) || !disposition_record_matches(entry, record, source_allow)
    {
        return None;
    }
    let mut reused = record.clone();
    reused.project = entry.project.clone();
    Some(reused)
}

fn try_reuse_cached_extract(
    aicx_home: &Path,
    entry: &CatalogEntry,
    prior: &SourceParseState,
    session_key: &str,
    source_allow: &crate::source_path::SourceAllowlist,
    repo_path_ignore_fingerprint: &str,
) -> Option<aicx_retrieve::ChunkRef> {
    if prior.signal_filter_version != SIGNAL_FILTER_VERSION
        || prior.repo_path_ignore_fingerprint != repo_path_ignore_fingerprint
    {
        return None;
    }
    let record = prior.sessions.get(session_key)?;
    if record.source_path != entry.source_path {
        return None;
    }
    // Zeroed legacy records (pre-fingerprint schema) never reuse.
    if record.source_len == 0 || record.source_mtime_ns == 0 {
        return None;
    }
    // LIVE source fingerprint is the reuse gate. Catalog-embedded size/mtime
    // lag until rebuild; requiring them to match the ledger forced full
    // reparse after a source-change index that already stamped live stats.
    if let Ok(live_path) = source_allow.resolve_file(entry.source_path.as_str()) {
        let (live_len, live_mtime) = crate::catalog::live_source_fingerprint(&live_path)?;
        if live_len != record.source_len || live_mtime != record.source_mtime_ns {
            return None;
        }
    } else {
        // Unreadable path: fall back to catalog-admitted fields only.
        if entry.source_len != Some(record.source_len)
            || entry.source_mtime_ns != Some(record.source_mtime_ns)
        {
            return None;
        }
    }
    let extract_path = aicx_home.join(&record.extract_relpath);
    let body = crate::source_path::read_under_aicx_home(aicx_home, &extract_path).ok()?;
    if sha256_hex(body.as_bytes()) != record.extract_sha256 {
        return None;
    }
    if body.trim().is_empty() {
        return None;
    }
    let project = entry
        .project
        .clone()
        .or_else(|| record.project.clone())
        .unwrap_or_else(|| "_unknown".to_string());
    let date = entry
        .date
        .clone()
        .or_else(|| record.date.clone())
        .unwrap_or_default();
    let preview_lines: Vec<String> = body
        .lines()
        .filter(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .take(6)
        .map(str::to_string)
        .collect();
    let metadata = serde_json::json!({
        "source_path": extract_path.to_string_lossy(),
        "project": project,
        "agent": entry.agent,
        "date": date,
        "kind": "conversations",
        "session_id": entry.session_id,
        "frame_kind": "conversation",
        "cwd": entry.cwd.clone().or_else(|| record.cwd.clone()),
        "source_catalog_path": entry.source_path,
        "preview_lines": preview_lines,
        "incremental_reuse": true,
    });
    Some(aicx_retrieve::ChunkRef {
        id: format!("{}:{}", entry.agent, entry.session_id),
        source_path: extract_path.display().to_string(),
        text: body,
        metadata,
    })
}

fn source_fingerprint(
    aicx_home: &Path,
    catalog_path: &Path,
    entries: &[CatalogEntry],
    source_allow: &crate::source_path::SourceAllowlist,
    repo_path_ignore_fingerprint: &str,
) -> Result<String> {
    let mut hasher = Sha256::new();
    // Filter generation first so a catalog-identical CURRENT cannot hide a
    // pre-filter corpus after signal-body rules change.
    hasher.update(SIGNAL_FILTER_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(DISPOSITION_POLICY_VERSION.as_bytes());
    hasher.update([0]);
    // Privacy rules are part of corpus identity. A rule edit must invalidate
    // CURRENT even when the catalog and every source file are unchanged.
    hasher.update(repo_path_ignore_fingerprint.as_bytes());
    hasher.update([0]);
    // Catalog membership (session ids + project attribution) is part of the
    // digest so new rows always admit a rebuild even when live stats match.
    hasher.update(
        crate::source_path::read_bytes_under_aicx_home(aicx_home, catalog_path)
            .with_context(|| format!("read catalog {}", catalog_path.display()))?,
    );
    // Per-source live size+mtime is the change detector. Catalog-embedded
    // fingerprints lag until rebuild; hashing LIVE stats means an append to
    // an existing JSONL moves the generation digest without inventing a new
    // session id (audit P0: source-CHANGE incremental, not only session-ADD).
    for entry in entries {
        hasher.update(entry.agent.as_bytes());
        hasher.update([0]);
        hasher.update(entry.session_id.as_bytes());
        hasher.update([0]);
        hasher.update(entry.source_path.as_bytes());
        hasher.update([0]);
        let (len, mtime) = live_or_catalog_fingerprint(entry, source_allow);
        hasher.update(len.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
    }
    Ok(hex::encode(hasher.finalize()))
}

/// Prefer live size+mtime; fall back to catalog-admitted values when the file
/// is temporarily unreadable (quarantine races, transient IO).
fn live_or_catalog_fingerprint(
    entry: &CatalogEntry,
    source_allow: &crate::source_path::SourceAllowlist,
) -> (u64, u64) {
    if let Ok(path) = source_allow.resolve_file(entry.source_path.as_str())
        && let Some(live) = crate::catalog::live_source_fingerprint(&path)
    {
        return live;
    }
    (
        entry.source_len.unwrap_or(0),
        entry.source_mtime_ns.unwrap_or(0),
    )
}

fn resolve_entry_fingerprint(entry: &CatalogEntry, source_path: &Path) -> (u64, u64) {
    if let (Some(len), Some(mtime)) = (entry.source_len, entry.source_mtime_ns) {
        return (len, mtime);
    }
    crate::catalog::live_source_fingerprint(source_path).unwrap_or((0, 0))
}

fn extract_path_for(aicx_home: &Path, agent: &str, session_id: &str) -> PathBuf {
    let mut safe: String = session_id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    safe = safe.trim_matches(['.', '_']).to_string();
    if safe.is_empty() || safe.len() > 180 {
        let digest = Sha256::digest(session_id.as_bytes());
        safe = format!("session-{}", &hex::encode(digest)[..16]);
    }
    aicx_home
        .join("extracts")
        .join(agent)
        .join(format!("{safe}_conversation.md"))
}

fn write_if_changed(aicx_home: &Path, path: &Path, bytes: &[u8]) -> Result<bool> {
    // Extracts live under aicx_home/extracts — prove containment before any IO.
    let allow = crate::source_path::SourceAllowlist::from_roots([aicx_home.to_path_buf()]);
    if path.exists() {
        let existing = allow.read_bytes(path).ok();
        if existing.as_deref() == Some(bytes) {
            return Ok(false);
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("extract path has no parent: {}", path.display()))?;
    fs::create_dir_all(parent)
        .with_context(|| format!("create extract dir {}", parent.display()))?;
    let tmp = path.with_extension("md.tmp");
    // Write path is derived solely from aicx_home + agent + sanitized session id
    // (see extract_path_for); validate the final destination under aicx_home.
    let write_path = crate::sanitize::validate_write_path(path)
        .with_context(|| format!("validate extract write path {}", path.display()))?;
    let tmp_write = crate::sanitize::validate_write_path(&tmp)
        .with_context(|| format!("validate extract tmp path {}", tmp.display()))?;
    fs::write(&tmp_write, bytes)
        .with_context(|| format!("write extract tmp {}", tmp_write.display()))?;
    fs::rename(&tmp_write, &write_path).with_context(|| {
        format!(
            "publish extract {} -> {}",
            tmp_write.display(),
            write_path.display()
        )
    })?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persistent_project_slice_cannot_replace_the_global_generation() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-project-safety-{}",
            std::process::id()
        ));
        let error = build(
            &root,
            &["vetcoders/vibecrafted".to_string()],
            false,
            false,
            false,
            false,
        )
        .expect_err("project-scoped publish must fail before touching the index");

        assert!(
            error
                .to_string()
                .contains("project-scoped index publishing is retired")
        );
    }

    #[test]
    fn vibecrafted_signal_body_drops_thought_tokens_and_keeps_visible_text() {
        let raw = r#"{"type":"thought","data":"The"}
{"type":"thought","data":" user"}
{"type":"text","data":"I'll"}
{"type":"text","data":" start"}
{"type":"text","data":" with"}
{"type":"text","data":" catalog"}
{"type":"item.completed","item":{"id":"item_1","type":"agent_message","text":"Routing strzałek is W2-B-4c."}}
"#;
        let cleaned = vibecrafted_signal_body(raw);
        assert!(
            cleaned.contains("I'll start with catalog"),
            "visible text tokens must reassemble; got {cleaned:?}"
        );
        assert!(
            cleaned.contains("Routing strzałek is W2-B-4c."),
            "agent_message bodies must survive; got {cleaned:?}"
        );
        assert!(
            !cleaned.contains("thought") && !cleaned.contains("\"data\":\"The\""),
            "thought token streams must not enter the index; got {cleaned:?}"
        );
    }

    #[test]
    fn vibecrafted_signal_body_keeps_plain_markdown_transcripts() {
        let raw = "# implement report\n\nRouting strzałek taby landed in W2-B-4c.\n";
        let cleaned = vibecrafted_signal_body(raw);
        assert!(cleaned.contains("Routing strzałek taby landed in W2-B-4c."));
    }

    #[test]
    fn clean_message_strips_workspace_bootstrap_blocks() {
        let raw = "<user_info>\nOS Version: macos\n</user_info>\n\
                   <git_status>\nM src/main.rs\n</git_status>\n\
                   Build the live continuity path.";
        let cleaned = clean_message(raw);
        assert_eq!(cleaned, "Build the live continuity path.");
    }

    #[test]
    fn signal_filter_version_is_non_empty_and_stable_for_this_cut() {
        // Guard against accidental empty version (would collapse fingerprints
        // across filter generations without meaning to).
        assert!(!SIGNAL_FILTER_VERSION.is_empty());
        assert!(SIGNAL_FILTER_VERSION.starts_with("signal-v"));
    }

    #[test]
    fn large_codex_signal_tracks_turn_context_cwd_per_frame() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-large-codex-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let source_path = root.join("rollout.jsonl");
        let body = concat!(
            "{\"timestamp\":\"2026-08-22T00:00:00Z\",\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/repo/public\"}}\n",
            "{\"timestamp\":\"2026-08-22T00:00:01Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"user\",\"content\":[{\"type\":\"input_text\",\"text\":\"public marker\"}]}}\n",
            "{\"timestamp\":\"2026-08-22T00:00:02Z\",\"type\":\"turn_context\",\"payload\":{\"cwd\":\"/repo/private\"}}\n",
            "{\"timestamp\":\"2026-08-22T00:00:03Z\",\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"private marker\"}]}}\n"
        );
        fs::write(&source_path, body).unwrap();
        let entry = CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "large-codex".to_string(),
            agent: "codex".to_string(),
            project: Some("owner/repo".to_string()),
            date: Some("2026-08-22".to_string()),
            cwd: Some("/repo/initial".to_string()),
            source_path: source_path.display().to_string(),
            source_len: None,
            source_mtime_ns: None,
            title: None,
            machine: None,
            logical_session_id: None,
        };
        let allow = crate::source_path::SourceAllowlist::from_roots([root.clone()]);

        let frames = parse_large_codex_signal(&entry, &source_path, &allow).unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].cwd.as_deref(), Some("/repo/public"));
        assert_eq!(frames[1].cwd.as_deref(), Some("/repo/private"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn source_fingerprint_changes_with_checkout_deny_list() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-ignore-fingerprint-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("catalog")).unwrap();
        let catalog_path = root.join("catalog/sessions.jsonl");
        fs::write(&catalog_path, "catalog snapshot\n").unwrap();
        let allow = crate::source_path::SourceAllowlist::from_roots([root.clone()]);

        let before = source_fingerprint(&root, &catalog_path, &[], &allow, "deny-a").unwrap();
        let after = source_fingerprint(&root, &catalog_path, &[], &allow, "deny-b").unwrap();
        assert_ne!(before, after);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn checkout_deny_list_drops_only_matching_frames() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-ignore-frames-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        let aicx_home = root.join(".aicx");
        fs::create_dir_all(&aicx_home).unwrap();
        fs::write(
            aicx_home.join(crate::legacy_archive::AICX_IGNORE_FILENAME),
            "/repo/private\n",
        )
        .unwrap();
        let ignore = crate::legacy_archive::load_repo_path_ignore(&aicx_home, &root).unwrap();
        let frame = |message: &str, cwd: &str| TimelineEntry {
            timestamp: chrono::Utc::now(),
            agent: "codex".to_string(),
            session_id: "multi-root".to_string(),
            role: "user".to_string(),
            message: message.to_string(),
            frame_kind: Some(FrameKind::UserMsg),
            branch: None,
            cwd: Some(cwd.to_string()),
            timestamp_source: Some("record".to_string()),
            source_path: None,
            source_sha256: None,
            source_line_span: None,
        };
        let mut frames = vec![
            frame("keep public", "/repo/public"),
            frame("drop private", "/repo/private/nested"),
            frame("keep public again", "/repo/public"),
        ];

        drop_ignored_cwd_frames(&mut frames, &ignore);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].message, "keep public");
        assert_eq!(frames[1].message, "keep public again");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parse_state_reuse_requires_matching_source_path_and_extract_hash() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-index-reuse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("extracts/claude")).unwrap();
        let extract_rel = "extracts/claude/session_conversation.md";
        let extract_path = root.join(extract_rel);
        let body = "# claude session\n\nuser: hello routing\nassistant: arrows vc-frame landed\n";
        fs::write(&extract_path, body).unwrap();

        // Real source file under a root the allowlist accepts (HOME + aicx_home).
        let source_path = root
            .join(".claude")
            .join("projects")
            .join("x")
            .join("session.jsonl");
        fs::create_dir_all(source_path.parent().unwrap()).unwrap();
        fs::write(&source_path, "{\"type\":\"user\",\"text\":\"hello\"}\n").unwrap();
        let (source_len, source_mtime_ns) =
            crate::catalog::live_source_fingerprint(&source_path).expect("source fingerprint");

        let entry = CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "session".to_string(),
            agent: "claude".to_string(),
            project: Some("vetcoders/vibecrafted".to_string()),
            date: Some("2026-07-22".to_string()),
            cwd: Some("/tmp/work".to_string()),
            source_path: source_path.display().to_string(),
            source_len: Some(source_len),
            source_mtime_ns: Some(source_mtime_ns),
            title: Some("routing".to_string()),
            machine: None,
            logical_session_id: None,
        };
        let mut prior = SourceParseState {
            schema: PARSE_STATE_SCHEMA.to_string(),
            signal_filter_version: SIGNAL_FILTER_VERSION.to_string(),
            repo_path_ignore_fingerprint: "ignore-fingerprint".to_string(),
            disposition_policy_version: DISPOSITION_POLICY_VERSION.to_string(),
            source_fingerprint: String::new(),
            sessions: BTreeMap::new(),
            dispositions: BTreeMap::new(),
        };
        prior.sessions.insert(
            session_state_key("claude", "session"),
            SessionParseRecord {
                source_path: entry.source_path.clone(),
                source_len,
                source_mtime_ns,
                extract_relpath: extract_rel.to_string(),
                extract_sha256: sha256_hex(body.as_bytes()),
                raw_frames: 4,
                signal_frames: 2,
                filtered_frames: 2,
                project: entry.project.clone(),
                date: entry.date.clone(),
                cwd: entry.cwd.clone(),
            },
        );

        let key = session_state_key("claude", "session");
        // Allowlist roots: treat test root as both HOME and aicx_home.
        let allow = crate::source_path::SourceAllowlist::for_operator(&root, &root);
        let reused =
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow, "ignore-fingerprint")
                .expect("matching source fingerprint+hash must reuse");
        assert!(reused.text.contains("arrows vc-frame"));
        assert_eq!(reused.id, "claude:session");

        // Source path drift invalidates reuse.
        let mut drifted = entry.clone();
        drifted.source_path = "/elsewhere/session.jsonl".to_string();
        assert!(
            try_reuse_cached_extract(&root, &drifted, &prior, &key, &allow, "ignore-fingerprint")
                .is_none()
        );

        // Catalog-only size drift must NOT invalidate when live file is unchanged
        // (catalog lag after a live-stamped reparse is expected).
        let mut catalog_lag = entry.clone();
        catalog_lag.source_len = Some(source_len + 64);
        assert!(
            try_reuse_cached_extract(
                &root,
                &catalog_lag,
                &prior,
                &key,
                &allow,
                "ignore-fingerprint"
            )
            .is_some(),
            "stale catalog size alone must not force reparse when live matches"
        );

        // Live source growth (append) invalidates reuse.
        fs::write(
            &source_path,
            "{\"type\":\"user\",\"text\":\"hello\"}\n{\"type\":\"user\",\"text\":\"more\"}\n",
        )
        .unwrap();
        // Coarse FS mtime: touch content size always changes here.
        assert!(
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow, "ignore-fingerprint")
                .is_none(),
            "live append must invalidate reuse"
        );
        // Restore live bytes so later checks use the original fingerprint.
        fs::write(&source_path, "{\"type\":\"user\",\"text\":\"hello\"}\n").unwrap();
        // mtime may have moved; refresh ledger to match restored content.
        let (restored_len, restored_mtime) =
            crate::catalog::live_source_fingerprint(&source_path).expect("restored fp");
        prior.sessions.get_mut(&key).unwrap().source_len = restored_len;
        prior.sessions.get_mut(&key).unwrap().source_mtime_ns = restored_mtime;

        // Zeroed legacy records never reuse (forces reparse after upgrade).
        prior.sessions.get_mut(&key).unwrap().source_len = 0;
        assert!(
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow, "ignore-fingerprint")
                .is_none()
        );
        prior.sessions.get_mut(&key).unwrap().source_len = restored_len;

        assert!(
            try_reuse_cached_extract(
                &root,
                &entry,
                &prior,
                &key,
                &allow,
                "changed-ignore-fingerprint"
            )
            .is_none(),
            "deny-list drift must invalidate cached extracts"
        );

        // Corrupt extract bytes invalidate reuse.
        fs::write(&extract_path, "tampered").unwrap();
        assert!(
            try_reuse_cached_extract(&root, &entry, &prior, &key, &allow, "ignore-fingerprint")
                .is_none()
        );

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn serialized_dispositions_drive_expected_documents_and_retryable_pending() {
        let mut state = SourceParseState {
            schema: PARSE_STATE_SCHEMA.to_string(),
            signal_filter_version: SIGNAL_FILTER_VERSION.to_string(),
            repo_path_ignore_fingerprint: "ignore-v1".to_string(),
            disposition_policy_version: DISPOSITION_POLICY_VERSION.to_string(),
            source_fingerprint: "snapshot-v1".to_string(),
            sessions: BTreeMap::new(),
            dispositions: BTreeMap::new(),
        };
        for ordinal in 0..10_050usize {
            let disposition = if ordinal < 10_015 {
                SourceDisposition::Indexed
            } else if ordinal < 10_046 {
                SourceDisposition::TerminalSkip {
                    reason: TerminalSkipReason::ParserFatal,
                }
            } else {
                SourceDisposition::ZeroSignal
            };
            state.dispositions.insert(
                format!("codex:session-{ordinal}"),
                SourceDispositionRecord {
                    source_path: format!("/sources/session-{ordinal}.jsonl"),
                    source_len: 1,
                    source_mtime_ns: ordinal as u64 + 1,
                    project: Some("Loctree/aicx".to_string()),
                    disposition,
                    raw_frames: 1,
                    signal_frames: usize::from(ordinal < 10_015),
                    filtered_frames: usize::from(ordinal >= 10_015),
                },
            );
        }

        let accounting = accounting_from_state(&state, None, Some(true));
        assert_eq!(accounting.total, 10_050);
        assert_eq!(accounting.indexed, 10_015);
        assert_eq!(accounting.terminal_skip, 31);
        assert_eq!(accounting.zero_signal, 4);
        assert_eq!(accounting.lexical_pending(10_015), 0);

        let serialized = serde_json::to_value(&state).expect("serialize disposition ledger");
        assert_eq!(serialized["source_fingerprint"], "snapshot-v1");
        assert_eq!(
            serialized["dispositions"]["codex:session-10015"]["disposition"]["state"],
            "terminal_skip"
        );
        assert_eq!(
            serialized["dispositions"]["codex:session-10015"]["disposition"]["reason"],
            "parser_fatal"
        );

        state
            .dispositions
            .get_mut("codex:session-10014")
            .unwrap()
            .disposition = SourceDisposition::RetryableError {
            reason: RetryableErrorReason::SourceUnavailable,
        };
        let retryable = accounting_from_state(&state, None, Some(true));
        assert_eq!(retryable.indexed, 10_014);
        assert_eq!(retryable.retryable_error, 1);
        assert_eq!(retryable.lexical_pending(10_014), 1);
        assert!(!retryable.is_complete());
    }

    #[test]
    fn terminal_reuse_is_cache_independent_and_fingerprint_scoped() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-terminal-cache-independent-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&root);
        let source_path = root.join("zero-signal.jsonl");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source_path, "{}\n").unwrap();
        let (source_len, source_mtime_ns) =
            crate::catalog::live_source_fingerprint(&source_path).unwrap();
        let entry = CatalogEntry {
            schema: crate::catalog::CATALOG_SCHEMA.to_string(),
            session_id: "zero-signal".to_string(),
            agent: "codex".to_string(),
            project: None,
            date: None,
            cwd: None,
            source_path: source_path.display().to_string(),
            source_len: Some(source_len),
            source_mtime_ns: Some(source_mtime_ns),
            title: None,
            machine: None,
            logical_session_id: None,
        };
        let key = session_state_key(&entry.agent, &entry.session_id);
        let mut prior = SourceParseState {
            schema: PARSE_STATE_SCHEMA.to_string(),
            signal_filter_version: SIGNAL_FILTER_VERSION.to_string(),
            repo_path_ignore_fingerprint: "ignore-v1".to_string(),
            disposition_policy_version: DISPOSITION_POLICY_VERSION.to_string(),
            source_fingerprint: "snapshot-v1".to_string(),
            sessions: BTreeMap::new(),
            dispositions: BTreeMap::new(),
        };
        prior.dispositions.insert(
            key.clone(),
            disposition_record(
                &entry,
                source_len,
                source_mtime_ns,
                SourceDisposition::ZeroSignal,
                1,
                0,
                1,
            ),
        );
        let allow = crate::source_path::SourceAllowlist::from_roots([root.clone()]);

        assert!(
            try_reuse_terminal_disposition(&entry, &prior, &key, &allow, "ignore-v1").is_some(),
            "zero-signal disposition must reuse without an extract cache"
        );
        fs::write(&source_path, "{}\n{}\n").unwrap();
        assert!(
            try_reuse_terminal_disposition(&entry, &prior, &key, &allow, "ignore-v1").is_none(),
            "live fingerprint change must invalidate terminal reuse"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn recent_parser_fatal_is_deferred_by_typed_control_flow() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-recent-fatal-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let path = root.join("actively-written.jsonl");
        fs::write(&path, "unterminated active record").unwrap();

        assert!(matches!(
            fatal_parse_outcome(&path),
            SourceParseOutcome::DeferredLiveChanged
        ));
        let transient = SourceDisposition::RetryableError {
            reason: RetryableErrorReason::ParserError,
        };
        assert!(
            !matches!(transient, SourceDisposition::TerminalSkip { .. }),
            "transient parser errors must never become terminal by message matching"
        );

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn vibecrafted_retention_tail_accepts_only_truncated_utf8_prefix() {
        let mut retained = vec![b'x'; VIBECRAFTED_RETENTION_BYTES];
        retained[0] = 0x86;
        retained[1] = 0x92;
        let decoded = decode_vibecrafted_transcript(&retained).unwrap();
        assert_eq!(decoded.len(), VIBECRAFTED_RETENTION_BYTES - 2);
        assert!(decoded.bytes().all(|byte| byte == b'x'));

        let short = [0x86, b'x'];
        assert!(decode_vibecrafted_transcript(&short).is_err());

        let mut invalid_interior = vec![b'x'; VIBECRAFTED_RETENTION_BYTES];
        invalid_interior[17] = 0x86;
        assert!(decode_vibecrafted_transcript(&invalid_interior).is_err());
    }

    #[test]
    fn missing_approved_source_requires_matching_prior_evidence() {
        let root = std::env::temp_dir().join(format!(
            "aicx-source-incomplete-current-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("catalog")).unwrap();
        let source_path = root.join("runtime-transcript.log");
        fs::write(&source_path, "visible answer\n").unwrap();
        let (source_len, source_mtime_ns) =
            crate::catalog::live_source_fingerprint(&source_path).unwrap();
        let missing_path = root.join("missing-transcript.log");
        let entries = [
            CatalogEntry {
                schema: crate::catalog::CATALOG_SCHEMA.to_string(),
                session_id: "indexed".to_string(),
                agent: "vibecrafted".to_string(),
                project: Some("Loctree/aicx".to_string()),
                date: None,
                cwd: None,
                source_path: source_path.display().to_string(),
                source_len: Some(source_len),
                source_mtime_ns: Some(source_mtime_ns),
                title: None,
                machine: None,
                logical_session_id: None,
            },
            CatalogEntry {
                schema: crate::catalog::CATALOG_SCHEMA.to_string(),
                session_id: "retryable".to_string(),
                agent: "vibecrafted".to_string(),
                project: Some("Loctree/aicx".to_string()),
                date: None,
                cwd: None,
                source_path: missing_path.display().to_string(),
                source_len: Some(77),
                source_mtime_ns: Some(88),
                title: None,
                machine: None,
                logical_session_id: None,
            },
        ];
        let catalog = entries
            .iter()
            .map(|entry| serde_json::to_string(entry).unwrap())
            .collect::<Vec<_>>()
            .join("\n");
        fs::write(root.join("catalog/sessions.jsonl"), format!("{catalog}\n")).unwrap();
        let current = root.join("indexed/_all/hybrid/CURRENT");
        fs::create_dir_all(current.parent().unwrap()).unwrap();
        fs::write(&current, "last-good\n").unwrap();

        let error = build(&root, &[], false, true, false, false)
            .expect_err("first missing observation must block publication");
        assert!(error.to_string().contains("retryable_error=1"));
        assert_eq!(fs::read_to_string(&current).unwrap(), "last-good\n");

        let raw = fs::read_to_string(parse_state_path(&root)).expect("durable accounting attempt");
        let first_state: SourceParseState = serde_json::from_str(&raw).unwrap();
        assert!(matches!(
            first_state.dispositions["vibecrafted:indexed"].disposition,
            SourceDisposition::Indexed
        ));
        assert!(matches!(
            first_state.dispositions["vibecrafted:retryable"].disposition,
            SourceDisposition::RetryableError {
                reason: RetryableErrorReason::SourceUnavailable
            }
        ));
        let allow = crate::source_path::SourceAllowlist::for_operator(&root, &root);
        assert!(recurrent_missing_approved_catalog_source(
            &entries[1],
            first_state.dispositions.get("vibecrafted:retryable"),
            &allow
        ));

        build(&root, &[], false, false, false, false)
            .expect("matching recurrent missing evidence must allow publication");
        let raw = fs::read_to_string(parse_state_path(&root)).expect("terminal accounting");
        let state: SourceParseState = serde_json::from_str(&raw).unwrap();
        let prior = &state.dispositions["vibecrafted:retryable"];
        assert!(matches!(
            prior.disposition,
            SourceDisposition::TerminalSkip {
                reason: TerminalSkipReason::SourceMissing
            }
        ));

        assert!(recurrent_missing_approved_catalog_source(
            &entries[1],
            Some(prior),
            &allow
        ));

        let mut changed_path = entries[1].clone();
        changed_path.source_path = root.join("other-missing.log").display().to_string();
        assert!(!recurrent_missing_approved_catalog_source(
            &changed_path,
            Some(prior),
            &allow
        ));

        let mut changed_fingerprint = entries[1].clone();
        changed_fingerprint.source_len = Some(78);
        assert!(!recurrent_missing_approved_catalog_source(
            &changed_fingerprint,
            Some(prior),
            &allow
        ));

        let mut absent_fingerprint = entries[1].clone();
        absent_fingerprint.source_mtime_ns = None;
        assert!(!recurrent_missing_approved_catalog_source(
            &absent_fingerprint,
            Some(prior),
            &allow
        ));

        let mut empty_source = entries[1].clone();
        empty_source.source_len = Some(0);
        let mut empty_prior = prior.clone();
        empty_prior.source_len = 0;
        assert!(recurrent_missing_approved_catalog_source(
            &empty_source,
            Some(&empty_prior),
            &allow
        ));

        let mut outside = entries[1].clone();
        outside.source_path = "/not-an-approved-root/missing.log".to_string();
        assert!(!recurrent_missing_approved_catalog_source(
            &outside,
            Some(prior),
            &allow
        ));

        let mut traversal = entries[1].clone();
        traversal.source_path = root
            .join("nested")
            .join("..")
            .join("missing.log")
            .display()
            .to_string();
        assert!(!recurrent_missing_approved_catalog_source(
            &traversal,
            Some(prior),
            &allow
        ));

        let directory = root.join("not-a-file");
        fs::create_dir_all(&directory).unwrap();
        let mut non_file = entries[1].clone();
        non_file.source_path = directory.display().to_string();
        assert!(!recurrent_missing_approved_catalog_source(
            &non_file,
            Some(prior),
            &allow
        ));

        let _ = fs::remove_dir_all(root);
    }
}
