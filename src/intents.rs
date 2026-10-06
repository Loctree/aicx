//! Intention Engine for ai-contexters.
//!
//! Elevates stored chunk `[signals]` metadata and matching raw conversation
//! lines into first-class, queryable intent records.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::chunker::{
    intent_keywords, is_decision_tag, is_local_command_artifact_line, is_outcome_tag,
    is_result_line, normalize_key, parse_checklist_task, truncate_signal_line,
};
use crate::extraction::conversation::{projection_kind_for_role, projection_role_for_role};
use crate::extraction::projection::{ProjectionKind, ProjectionRole, ProjectionSpec};
use crate::extraction::{IntentLineModality, intent_line_modality, is_harness_injected_noise};
use crate::legacy_archive;
use crate::sanitize;
use crate::timeline::FrameKind;
#[cfg(feature = "app")]
use crate::timeline::TimelineEntry;
use crate::types::{EntryState, EntryType, IntentEntry, Link, LinkType};

mod display;
mod schema;
mod types;

pub use self::display::{
    IntentDisplayFilters, IntentDisplayResult, IntentSortOrder, UnresolvedMode,
    apply_display_filters, apply_display_filters_with_completeness, format_intents_json,
    format_intents_markdown, format_intents_oracle_json,
    format_intents_oracle_json_with_completeness,
};
use self::types::{
    CandidateAccumulator, IntentCandidate, SignalSection, StoredChunkFile, TaskAccumulator,
    TaskEvent, TranscriptEntry,
};
pub use self::types::{
    IntentExtraction, IntentExtractionStats, IntentKind, IntentProvenance, IntentRecord,
    IntentSourceFilter, IntentsCompleteness, IntentsConfig, MigrationReport, MixedScopeSession,
    ProjectResolutionScope, SourceSelection, UnplacedScopeSession,
};
// Lane 2-5 schema anchor (MASTER Phase 2 §3). Stages land incrementally; these
// types are the convergence point every lane stage must agree on.
pub use self::schema::{
    CLARIFY_MAX_QUESTIONS, ClaimRecord, ClaimSource, ClaimType, ClarifyQuestion, CodescribeParser,
    ContractFracture, EvidenceKind, EvidenceRecord, FractureSeverity, LANE_SCHEMA_VERSION,
    LaneExport, ResultRecord, ResultStatus, TimeCoverage, UTC_TIMEZONE_ASSUMPTION, UserIntentLine,
    VerificationStatus, audit_claims_against_evidence, classify_claim, collect_artifact_evidence,
    detect_contract_fractures, detect_fractures, extract_claims, extract_user_intent_lines,
    generate_clarify, is_agent_role, is_user_role, verify_claims,
};

/// E.6: hard upper bound on per-extraction candidate vectors. A pathological
/// input (huge transcript with many bullet lines) used to drag the whole
/// pipeline down by piling up candidates that dedup would later collapse to a
/// handful. Cap here so memory stays bounded; emit a diagnostic on stderr when
/// the cap is hit so the operator notices truncated extraction.
const MAX_CANDIDATES: usize = 5000;
const CARD_HEADER_READ_LIMIT: u64 = 64 * 1024;
pub const CATALOG_IDENTITY_SOURCE: &str = "catalog-v1";
pub const PERSISTED_IDENTITY_SOURCE: &str = "project-bucket-v1";
pub const PATH_HEURISTIC_IDENTITY_SOURCE: &str = "path-heuristic";
/// Sessions admitted straight from a live source-root scan (hot window),
/// bypassing the durable catalog census that has not admitted them yet.
pub const LIVE_SCAN_IDENTITY_SOURCE: &str = "live-scan-v1";

/// Inclusive UTC utterance window. Zero hours means all recorded history.
pub(crate) fn window_cutoff(now: DateTime<Utc>, hours: u64) -> DateTime<Utc> {
    if hours == 0 {
        return DateTime::UNIX_EPOCH;
    }
    i64::try_from(hours)
        .ok()
        .and_then(Duration::try_hours)
        .and_then(|duration| now.checked_sub_signed(duration))
        .unwrap_or(DateTime::<Utc>::MIN_UTC)
}

#[cfg(feature = "app")]
fn source_receipt(
    entry: &crate::catalog::CatalogEntry,
    admitted: bool,
    status: &str,
) -> SourceSelection {
    SourceSelection {
        agent: entry.agent.clone(),
        session_id: entry.session_id.clone(),
        path: entry.source_path.clone(),
        catalog_project: entry.project.clone(),
        admitted,
        status: status.into(),
        ..Default::default()
    }
}

#[cfg(feature = "app")]
fn frame_has_conversation_time(frame: &TimelineEntry) -> bool {
    frame.timestamp != DateTime::UNIX_EPOCH
        && !matches!(
            frame.timestamp_source.as_deref(),
            Some("source_mtime" | "wall_clock_fallback" | "session_provenance" | "unknown")
        )
}

/// Preserve parser coverage while applying the same role projection as the public reader.
#[cfg(feature = "app")]
fn read_intent_source(
    aicx_home: &Path,
    entry: &crate::catalog::CatalogEntry,
    frame_kind: FrameKind,
    source_errors: &mut usize,
    notes: &mut ScopeNotes,
) -> Result<(
    PathBuf,
    Vec<TimelineEntry>,
    crate::extraction::conversation::ScopeReport,
)> {
    let (path, mut frames, scope, coverage) =
        crate::overlay::read_cached_catalog_conversation_at(aicx_home, entry)?;
    notes.coverage.insert(
        (entry.agent.clone(), entry.session_id.clone()),
        coverage.receipt_label(),
    );
    if !matches!(
        coverage,
        crate::source_index::ConversationCoverage::CompleteVisible
    ) {
        *source_errors += 1;
        crate::diagnostics::log_describe(&format!(
            "intents_partial_source agent={} session_id={} coverage={coverage:?}",
            entry.agent, entry.session_id
        ));
    }
    frames.retain(|frame| crate::source_index::frame_matches_kind(frame, frame_kind));
    Ok((path, frames, scope))
}

pub fn extract_intents(config: &IntentsConfig) -> Result<Vec<IntentRecord>> {
    Ok(extract_intents_with_stats(config)?.records)
}

pub fn extract_intents_with_stats(config: &IntentsConfig) -> Result<IntentExtraction> {
    extract_intents_with_stats_filtered(config, &IntentSourceFilter::default())
}

pub fn extract_intents_with_stats_filtered(
    config: &IntentsConfig,
    source_filter: &IntentSourceFilter,
) -> Result<IntentExtraction> {
    let aicx_home = crate::aicx_home::ensure()?;
    extract_intents_from_root_at_with_stats_filtered(config, source_filter, &aicx_home, Utc::now())
}

pub fn extract_intents_with_stats_for_projects(
    config: &IntentsConfig,
    projects: &[String],
) -> Result<IntentExtraction> {
    extract_intents_with_stats_for_projects_filtered(
        config,
        projects,
        &IntentSourceFilter::default(),
    )
}

pub fn extract_intents_with_stats_for_projects_filtered(
    config: &IntentsConfig,
    projects: &[String],
    source_filter: &IntentSourceFilter,
) -> Result<IntentExtraction> {
    let aicx_home = crate::aicx_home::ensure()?;
    extract_intents_from_root_at_for_projects_with_stats_filtered(
        config,
        projects,
        source_filter,
        &aicx_home,
        Utc::now(),
    )
}

#[cfg(test)]
fn extract_intents_from_root_at(
    config: &IntentsConfig,
    aicx_home: &Path,
    now: DateTime<Utc>,
) -> Result<Vec<IntentRecord>> {
    Ok(extract_intents_from_root_at_with_stats(config, aicx_home, now)?.records)
}

pub(crate) fn extract_intents_from_root_at_with_stats(
    config: &IntentsConfig,
    aicx_home: &Path,
    now: DateTime<Utc>,
) -> Result<IntentExtraction> {
    extract_intents_from_root_at_with_stats_filtered(
        config,
        &IntentSourceFilter::default(),
        aicx_home,
        now,
    )
}

pub(crate) fn extract_intents_from_root_at_with_stats_filtered(
    config: &IntentsConfig,
    source_filter: &IntentSourceFilter,
    aicx_home: &Path,
    now: DateTime<Utc>,
) -> Result<IntentExtraction> {
    let cutoff = window_cutoff(now, config.hours);
    let mut notes = ScopeNotes {
        now: Some(now),
        ..Default::default()
    };
    let (files, source_errors, corpus_identity_source, live_sessions) =
        collect_intent_files(aicx_home, config, cutoff, source_filter, &mut notes)?;
    extract_intents_from_files_with_stats(
        config,
        files,
        source_errors,
        corpus_identity_source,
        live_sessions,
        source_filter,
        notes,
    )
}

/// What the lanes' project filters could not serve, noted before each filter
/// runs so that it survives a filter that removes every frame.
#[derive(Debug, Default)]
struct ScopeNotes {
    now: Option<DateTime<Utc>>,
    selection: Vec<SourceSelection>,
    #[cfg(feature = "app")]
    coverage: HashMap<(String, String), String>,
    /// Sessions that could not be served whole ([`note_mixed_scope`]).
    mixed: Vec<MixedScopeSession>,
    /// Sessions served without their unplaced frames ([`note_unplaced_frames`]).
    unplaced: Vec<UnplacedScopeSession>,
}

/// Record, once per (agent, session), the frames the project filter is about
/// to withhold because the turn window they sit in could not be placed.
///
/// Such a frame keeps the cataloged checkout as its `cwd`, so the session
/// reads as one repository with no conflict and is not a mixed candidate: its
/// placed frames are served, as they should be. The filter still drops every
/// unplaced frame, and without this note that removal was silent — the answer
/// looked complete, and a window that held only such work came back empty.
/// A session the lane already noted as unservable is left out: `mixed_scope`
/// withholds all of it.
#[cfg(feature = "app")]
fn note_unplaced_frames(
    unplaced: &mut Vec<UnplacedScopeSession>,
    agent: &str,
    session_id: &str,
    frames: &[TimelineEntry],
) {
    let withheld = frames
        .iter()
        .filter(|frame| frame.scope_unattributed)
        .count();
    if withheld == 0
        || unplaced
            .iter()
            .any(|seen| seen.agent == agent && seen.session_id == session_id)
    {
        return;
    }
    crate::diagnostics::log_describe(&format!(
        "intents_unplaced_frames agent={agent} session_id={session_id} frames={withheld}"
    ));
    unplaced.push(UnplacedScopeSession {
        agent: agent.to_string(),
        session_id: session_id.to_string(),
        frames: withheld,
    });
}

/// Record a mixed-workstream candidate once per (agent, session). Called both
/// from surviving files and from the lanes' pre-filter scope reports, so the
/// telemetry survives even when the fail-closed project filter removes every
/// frame of the session from the requested bucket.
///
/// `unservable` is the caller's verdict, and a lane with a catalog row passes
/// the very one its project filter ran on: `scope_foreign_to` the cataloged
/// checkout. `scope_mixed()` alone let a session re-scoped wholesale to ONE
/// foreign checkout — one repository, no conflict — lose every frame to the
/// filter and leave no trace, so `continuity` answered that window with a
/// successful empty pack. Only a file with no catalog row to judge against
/// falls back to `scope_mixed()`. Either way a branch switch inside one
/// checkout is one scope, and a scope hidden by `.aicxignore` is still a scope.
fn note_mixed_scope(
    mixed_scope: &mut Vec<MixedScopeSession>,
    agent: &str,
    session_id: &str,
    scope: &crate::extraction::conversation::ScopeReport,
    unservable: bool,
) {
    if !unservable {
        return;
    }
    if mixed_scope
        .iter()
        .any(|seen| seen.agent == agent && seen.session_id == session_id)
    {
        return;
    }
    crate::diagnostics::log_describe(&format!(
        "intents_mixed_scope agent={} session_id={} cwds={} branches={}",
        agent,
        session_id,
        scope.cwds.join(","),
        scope.branches.join(",")
    ));
    mixed_scope.push(MixedScopeSession {
        agent: agent.to_string(),
        session_id: session_id.to_string(),
        cwds: scope.cwds.clone(),
        branches: scope.branches.clone(),
        // The whole verdict travels: a session mixed only by a hidden scope
        // or a conflict has at most one visible cwd, and a consumer that
        // rebuilt the report from cwds alone would call it homogeneous.
        conflicts: scope.conflicts,
        hidden_scopes: scope.hidden_scopes,
        status: scope.status,
    });
}

fn extract_intents_from_files_with_stats(
    config: &IntentsConfig,
    mut files: Vec<StoredChunkFile>,
    source_errors: usize,
    corpus_identity_source: &str,
    live_sessions: usize,
    source_filter: &IntentSourceFilter,
    notes: ScopeNotes,
) -> Result<IntentExtraction> {
    let ScopeNotes {
        mixed: mut mixed_scope,
        unplaced: unplaced_scope,
        selection,
        now,
        ..
    } = notes;
    let mut selection = selection;
    materialize_transcripts_for_admission(&mut files, config, source_filter, now);
    order_files_for_admission(&mut files);
    for file in &files {
        let path = file.path.to_string_lossy().into_owned();
        let weight = file_human_messages(file);
        if let Some(row) = selection.iter_mut().find(|row| {
            row.agent == file.agent && row.session_id == file.session_id && row.path == path
        }) {
            if file.transcript_entries.is_some() {
                row.human_messages = weight;
            }
        } else {
            selection.push(SourceSelection {
                agent: file.agent.clone(),
                session_id: file.session_id.clone(),
                path,
                catalog_project: Some(file.project.clone()),
                admitted: file.identity_source != LIVE_SCAN_IDENTITY_SOURCE,
                status: "qualified".into(),
                qualified_frames: file.transcript_entries.as_ref().map_or(0, Vec::len),
                latest_activity: Some(file.timestamp.to_rfc3339()),
                human_messages: weight,
                ..Default::default()
            });
        }
    }
    let scanned_count = files.len();
    let source_paths_verified = source_errors == 0 && verify_stored_chunk_paths(&files);
    let matched_project_buckets = files
        .iter()
        .map(|file| file.project.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let identity_source = if corpus_identity_source == INDEX_IDENTITY_SOURCE {
        INDEX_IDENTITY_SOURCE
    } else if corpus_identity_source == CATALOG_IDENTITY_SOURCE {
        CATALOG_IDENTITY_SOURCE
    } else if files
        .iter()
        .any(|file| file.identity_source == PATH_HEURISTIC_IDENTITY_SOURCE)
    {
        PATH_HEURISTIC_IDENTITY_SOURCE
    } else {
        PERSISTED_IDENTITY_SOURCE
    }
    .to_string();
    let path_heuristic_sources: BTreeSet<String> = files
        .iter()
        .filter(|file| file.identity_source == PATH_HEURISTIC_IDENTITY_SOURCE)
        .map(|file| file.path.to_string_lossy().into_owned())
        .collect();
    for file in &files {
        // The lanes that attach a scope have already noted the session on
        // the verdict their project filter used; this pass is the floor for a
        // file that arrives with a scope and no such verdict.
        if let Some(scope) = file.scope.as_ref() {
            note_mixed_scope(
                &mut mixed_scope,
                &file.agent,
                &file.session_id,
                scope,
                scope.scope_mixed(),
            );
        }
    }

    let mut candidates = Vec::new();
    let mut task_events = Vec::new();
    let mut cap_warned = false;
    let mut dropped_candidates = 0usize;
    let mut dropped_task_events = 0usize;

    for file in files {
        let (signal_lines, transcript_entries) = if let Some(transcript_entries) =
            file.transcript_entries.as_ref()
        {
            (Vec::new(), transcript_entries.clone())
        } else if let Some(body) = file.body.as_ref() {
            // Served from the committed index: the document is already in
            // memory and needs no disk read or transcript re-parse.
            //
            // The census path narrows frames to the requested channel before
            // the classifier ever sees them; the index stores the whole
            // extract, so the same narrowing happens here instead. Without it
            // `--frame-kind user_msg` would silently classify assistant prose
            // as operator intent.
            let mut transcript_entries = parse_extract_document(body);
            let wanted = config.effective_frame_kind();
            transcript_entries
                .retain(|entry| FrameKind::parse(&entry.role).is_some_and(|kind| kind == wanted));
            (Vec::new(), transcript_entries)
        } else {
            let content = sanitize::read_to_string_validated(&file.path)
                .with_context(|| format!("Failed to read chunk file: {}", file.path.display()))?;
            parse_chunk_document(&content)
        };
        let mut transcript_entries = transcript_entries;
        let parsed_query_frames = transcript_entries.len();
        if let Some(now) = now {
            let cutoff = window_cutoff(now, config.hours);
            transcript_entries.retain(|entry| {
                entry
                    .timestamp
                    .is_none_or(|time| time >= cutoff && time <= now)
            });
        }
        if source_filter.date_lo.is_some() || source_filter.date_hi.is_some() {
            transcript_entries.retain(|entry| {
                entry.timestamp.is_some_and(|timestamp| {
                    source_date_matches_filter(
                        &timestamp.format("%Y-%m-%d").to_string(),
                        source_filter,
                    )
                })
            });
        }
        if file.identity_source == INDEX_IDENTITY_SOURCE
            && let Some(receipt) = selection.iter_mut().find(|receipt| {
                receipt.agent == file.agent
                    && receipt.session_id == file.session_id
                    && receipt.path == file.path.to_string_lossy()
                    && receipt.parsed_frames == 0
            })
        {
            receipt.parsed_frames = parsed_query_frames;
            receipt.scoped_frames = parsed_query_frames;
            receipt.qualified_frames = transcript_entries.len();
            receipt.outside_window_frames =
                parsed_query_frames.saturating_sub(transcript_entries.len());
            receipt.latest_activity = transcript_entries
                .iter()
                .filter_map(|entry| entry.timestamp)
                .max()
                .map(|timestamp| timestamp.to_rfc3339());
            receipt.status = if transcript_entries.is_empty() {
                "outside_window"
            } else {
                "qualified"
            }
            .into();
        }
        let source_chunk = file.path.to_string_lossy().to_string();
        let weight = transcript_human_messages(&transcript_entries);
        if let Some(row) = selection.iter_mut().find(|row| {
            row.agent == file.agent && row.session_id == file.session_id && row.path == source_chunk
        }) {
            row.human_messages = weight;
        }

        // oś 3: stamp records with the chunk's canonical bucket (file.project),
        // not the query filter (config.project) — empty/aliased filters must not
        // leak into record provenance.
        let (signal_candidates, signal_tasks) =
            extract_signal_candidates(&file, &file.project, &source_chunk, &signal_lines);
        dropped_candidates += extend_with_cap(
            &mut candidates,
            signal_candidates,
            &mut cap_warned,
            "candidates",
        );
        dropped_task_events += extend_with_cap(
            &mut task_events,
            signal_tasks,
            &mut cap_warned,
            "task_events",
        );

        let (raw_candidates, raw_tasks) =
            extract_transcript_candidates(&file, &file.project, &source_chunk, &transcript_entries);
        dropped_candidates += extend_with_cap(
            &mut candidates,
            raw_candidates,
            &mut cap_warned,
            "candidates",
        );
        dropped_task_events +=
            extend_with_cap(&mut task_events, raw_tasks, &mut cap_warned, "task_events");
    }

    let mut records = dedup_candidates(
        candidates,
        config.strict,
        config.min_confidence,
        config.kind_filter,
    );
    drop_truncated_duplicate_records(&mut records);
    let mut task_records = finalize_tasks(
        task_events,
        config.strict,
        config.min_confidence,
        config.kind_filter,
    );
    records.append(&mut task_records);
    let path_heuristic_records = records
        .iter()
        .filter(|record| path_heuristic_sources.contains(&record.source_chunk))
        .count();

    reconcile_session_id_with_path(&mut records);

    sort_intent_records(&mut records);

    let stats = IntentExtractionStats {
        scanned_count,
        candidate_count: records.len(),
        source_paths_verified,
        source_errors,
        candidate_cap: MAX_CANDIDATES,
        dropped_candidates,
        dropped_task_events,
        matched_project_buckets,
        identity_source,
        path_heuristic_records,
        live_sessions,
        mixed_scope_sessions: mixed_scope.len(),
        unplaced_frames: unplaced_scope.iter().map(|session| session.frames).sum(),
    };

    Ok(IntentExtraction {
        selection,
        records,
        stats,
        mixed_scope,
        unplaced_scope,
    })
}

pub(crate) fn extract_intents_from_root_at_for_projects_with_stats(
    config: &IntentsConfig,
    projects: &[String],
    aicx_home: &Path,
    now: DateTime<Utc>,
) -> Result<IntentExtraction> {
    extract_intents_from_root_at_for_projects_with_stats_filtered(
        config,
        projects,
        &IntentSourceFilter::default(),
        aicx_home,
        now,
    )
}

pub(crate) fn extract_intents_from_root_at_for_projects_with_stats_filtered(
    config: &IntentsConfig,
    projects: &[String],
    source_filter: &IntentSourceFilter,
    aicx_home: &Path,
    now: DateTime<Utc>,
) -> Result<IntentExtraction> {
    if projects.is_empty() {
        return extract_intents_from_root_at_with_stats_filtered(
            config,
            source_filter,
            aicx_home,
            now,
        );
    }

    let mut selection: Vec<SourceSelection> = Vec::new();
    let mut records = Vec::new();
    let mut scanned_count = 0usize;
    let mut source_paths_verified = true;
    let mut source_errors = 0usize;
    let mut dropped_candidates = 0usize;
    let mut dropped_task_events = 0usize;
    let mut matched_project_buckets = BTreeSet::new();
    let mut identity_source = PERSISTED_IDENTITY_SOURCE.to_string();
    let mut path_heuristic_records = 0usize;
    let mut live_sessions = 0usize;
    let mut mixed_scope: Vec<MixedScopeSession> = Vec::new();
    let mut unplaced_scope: Vec<UnplacedScopeSession> = Vec::new();

    for project in projects {
        let mut scoped = config.clone();
        scoped.project = project.clone();
        let extraction = extract_intents_from_root_at_with_stats_filtered(
            &scoped,
            source_filter,
            aicx_home,
            now,
        )?;
        for receipt in extraction.selection {
            if let Some(previous) = selection.iter_mut().find(|s| {
                s.agent == receipt.agent
                    && s.session_id == receipt.session_id
                    && s.path == receipt.path
            }) {
                if receipt.status == "qualified" || previous.status == "project_excluded" {
                    *previous = receipt;
                }
            } else {
                selection.push(receipt);
            }
        }
        for session in extraction.mixed_scope {
            if !mixed_scope
                .iter()
                .any(|seen| seen.agent == session.agent && seen.session_id == session.session_id)
            {
                mixed_scope.push(session);
            }
        }
        for session in extraction.unplaced_scope {
            if !unplaced_scope
                .iter()
                .any(|seen| seen.agent == session.agent && seen.session_id == session.session_id)
            {
                unplaced_scope.push(session);
            }
        }
        scanned_count += extraction.stats.scanned_count;
        source_paths_verified &= extraction.stats.source_paths_verified;
        source_errors += extraction.stats.source_errors;
        dropped_candidates += extraction.stats.dropped_candidates;
        dropped_task_events += extraction.stats.dropped_task_events;
        matched_project_buckets.extend(extraction.stats.matched_project_buckets);
        path_heuristic_records += extraction.stats.path_heuristic_records;
        live_sessions += extraction.stats.live_sessions;
        if extraction.stats.identity_source == PATH_HEURISTIC_IDENTITY_SOURCE {
            identity_source = PATH_HEURISTIC_IDENTITY_SOURCE.to_string();
        } else if extraction.stats.identity_source == CATALOG_IDENTITY_SOURCE
            && identity_source != PATH_HEURISTIC_IDENTITY_SOURCE
        {
            identity_source = CATALOG_IDENTITY_SOURCE.to_string();
        } else if extraction.stats.identity_source == INDEX_IDENTITY_SOURCE
            && identity_source == PERSISTED_IDENTITY_SOURCE
        {
            identity_source = INDEX_IDENTITY_SOURCE.to_string();
        }
        records.extend(extraction.records);
    }

    dedup_intent_records(&mut records);
    sort_intent_records(&mut records);

    let stats = IntentExtractionStats {
        scanned_count,
        candidate_count: records.len(),
        source_paths_verified,
        source_errors,
        candidate_cap: MAX_CANDIDATES,
        dropped_candidates,
        dropped_task_events,
        matched_project_buckets: matched_project_buckets.into_iter().collect(),
        identity_source,
        path_heuristic_records,
        live_sessions,
        mixed_scope_sessions: mixed_scope.len(),
        unplaced_frames: unplaced_scope.iter().map(|session| session.frames).sum(),
    };

    Ok(IntentExtraction {
        selection,
        records,
        stats,
        mixed_scope,
        unplaced_scope,
    })
}

fn sort_intent_records(records: &mut [IntentRecord]) {
    records.sort_by(|left, right| {
        right
            .date
            .cmp(&left.date)
            .then_with(|| left.kind.sort_rank().cmp(&right.kind.sort_rank()))
            .then_with(|| {
                let left_is_voice = left.source.as_deref() == Some("voice_transcript");
                let right_is_voice = right.source.as_deref() == Some("voice_transcript");
                left_is_voice.cmp(&right_is_voice)
            })
            .then_with(|| right.source_chunk.cmp(&left.source_chunk))
            .then_with(|| left.summary.cmp(&right.summary))
    });
}

fn dedup_intent_records(records: &mut Vec<IntentRecord>) {
    let mut seen = HashSet::new();
    records.retain(|record| {
        // Normalize the summary so dedup catches near-duplicates that differ
        // only in whitespace, case, or invisible chars (zero-width / bidi).
        // Without this, "fix au\u{200B}th" sneaks past as a "new" record.
        // Session/chunk are provenance, not identity: identical normalized
        // facts re-ingested across sessions should surface once per project.
        seen.insert((
            record.kind,
            record.project.clone(),
            normalize_key(&record.summary),
        ))
    });
}

fn verify_stored_chunk_paths(files: &[StoredChunkFile]) -> bool {
    files.iter().all(|file| file.path.exists())
}

/// E.6: append `additions` into `target` until `target` reaches MAX_CANDIDATES.
/// Emits a single stderr diagnostic the first time a cap is hit per run.
fn transcript_human_messages(entries: &[TranscriptEntry]) -> usize {
    entries
        .iter()
        .filter(|entry| is_user_role(&entry.role))
        .count()
}

fn file_human_messages(file: &StoredChunkFile) -> usize {
    file.transcript_entries
        .as_deref()
        .map(transcript_human_messages)
        .unwrap_or(0)
}

/// Parse index bodies once, narrow them the way the candidate loop does, and
/// drop turns outside the window. Weight has to be known before the cap
/// decides which file is allowed to fill it.
fn materialize_transcripts_for_admission(
    files: &mut [StoredChunkFile],
    config: &IntentsConfig,
    source_filter: &IntentSourceFilter,
    now: Option<DateTime<Utc>>,
) {
    let wanted = config.effective_frame_kind();
    for file in files.iter_mut() {
        if file.transcript_entries.is_none()
            && let Some(body) = file.body.take()
        {
            let mut entries = parse_extract_document(&body);
            entries
                .retain(|entry| FrameKind::parse(&entry.role).is_some_and(|kind| kind == wanted));
            file.transcript_entries = Some(entries);
        }
        if let Some(now) = now
            && let Some(entries) = file.transcript_entries.as_mut()
        {
            let cutoff = window_cutoff(now, config.hours);
            entries.retain(|entry| {
                entry
                    .timestamp
                    .is_none_or(|time| time >= cutoff && time <= now)
            });
        }
        if (source_filter.date_lo.is_some() || source_filter.date_hi.is_some())
            && let Some(entries) = file.transcript_entries.as_mut()
        {
            entries.retain(|entry| {
                entry.timestamp.is_some_and(|timestamp| {
                    source_date_matches_filter(
                        &timestamp.format("%Y-%m-%d").to_string(),
                        source_filter,
                    )
                })
            });
        }
        if let Some(latest) = file
            .transcript_entries
            .as_deref()
            .into_iter()
            .flatten()
            .filter_map(|entry| entry.timestamp)
            .max()
        {
            file.timestamp = latest;
            file.date = latest.format("%Y-%m-%d").to_string();
        }
    }
}

/// Heavier human speech is admitted before a newer thin file. Equal weight
/// keeps the previous newest-first order, including its path tie-break.
fn order_files_for_admission(files: &mut [StoredChunkFile]) {
    files.sort_by(|left, right| {
        file_human_messages(right)
            .cmp(&file_human_messages(left))
            .then_with(|| right.timestamp.cmp(&left.timestamp))
            .then_with(|| right.sequence.cmp(&left.sequence))
            .then_with(|| right.path.cmp(&left.path))
    });
}

fn extend_with_cap<T>(
    target: &mut Vec<T>,
    additions: Vec<T>,
    warned: &mut bool,
    bucket_name: &'static str,
) -> usize {
    let room = MAX_CANDIDATES.saturating_sub(target.len());
    if additions.len() <= room {
        target.extend(additions);
        return 0;
    }
    let dropped = additions.len() - room;
    target.extend(additions.into_iter().take(room));
    if !*warned {
        eprintln!(
            "aicx intents: warning: {bucket_name} cap of {MAX_CANDIDATES} reached; dropped {dropped} entries"
        );
        *warned = true;
    }
    dropped
}

/// Identity provenance for records served from the committed lexical index.
pub const INDEX_IDENTITY_SOURCE: &str = "index-v1";

/// Every metadata key the index lane's scope decisions read.
#[cfg(feature = "app")]
const SCOPE_METADATA_CONTRACT: [&str; 3] = ["scope_conflict", "scope_unattributed", "session_kind"];

/// Does this chunk STATE its scope, or merely omit it?
///
/// A chunk written before these keys existed carries none of them, and every
/// reader below takes absence for a clean answer: not mixed, not
/// unattributed, not a guardian. The Tantivy schema version does not move for
/// a metadata addition, so `open_current_adapter_at` admits such a generation
/// and it keeps serving foreign frames and control-plane prompts until
/// someone happens to re-run `aicx index`.
///
/// Presence is the contract, not truth: the builder writes all three keys on
/// every chunk it publishes, `null` included. A chunk that cannot state its
/// scope is re-sourced through the census lane, exactly like one that states
/// a bad scope.
#[cfg(feature = "app")]
fn chunk_states_scope(metadata: &serde_json::Value) -> bool {
    SCOPE_METADATA_CONTRACT
        .iter()
        .all(|key| metadata.get(key).is_some())
}

#[cfg(feature = "app")]
fn source_agent_matches(agent: &str, filter: &IntentSourceFilter) -> bool {
    let Some(want) = filter.agent.as_deref() else {
        return true;
    };
    let want = aicx_parser::engine::AgentKind::parse(want)
        .map(|kind| kind.as_str())
        .unwrap_or(want);
    agent == want
}

#[cfg(feature = "app")]
#[derive(Debug, PartialEq, Eq)]
struct CurrentMetadataFact {
    key: Option<(String, String)>,
    flagged_for_project: bool,
}

#[cfg(feature = "app")]
fn current_metadata_fact(
    metadata: &serde_json::Value,
    project: &str,
    source_filter: &IntentSourceFilter,
) -> Option<CurrentMetadataFact> {
    let agent = metadata.get("agent").and_then(|value| value.as_str());
    if source_filter.agent.is_some()
        && !agent.is_some_and(|agent| source_agent_matches(agent, source_filter))
    {
        return None;
    }
    let key = agent
        .zip(metadata.get("session_id").and_then(|value| value.as_str()))
        .map(|(agent, session_id)| (agent.to_string(), session_id.to_string()));
    let project_matches = entry_matches_project(
        metadata.get("project").and_then(|value| value.as_str()),
        project,
    );
    let guardian = crate::sessions::is_guardian_session_kind(
        metadata
            .get("session_kind")
            .and_then(|value| value.as_str()),
    );
    let scope_flagged = !chunk_states_scope(metadata)
        || metadata
            .get("scope_conflict")
            .and_then(|value| value.as_bool())
            == Some(true)
        || metadata
            .get("scope_unattributed")
            .and_then(|value| value.as_bool())
            == Some(true);
    Some(CurrentMetadataFact {
        key,
        flagged_for_project: project_matches && !guardian && scope_flagged,
    })
}

/// Match the date grammar used by the display filter, but on the utterance's
/// date. Callers pass normalized YYYY-MM-DD bounds, so an upper bound remains
/// inclusive for the whole named day instead of turning into midnight-only.
fn source_date_matches_filter(date: &str, filter: &IntentSourceFilter) -> bool {
    let within_bound = |bound: &str, keep_low: bool| -> bool {
        match (parse_flexible_utc(date), parse_flexible_utc(bound)) {
            (Some(date), Some(bound)) => {
                if keep_low {
                    date >= bound
                } else {
                    date <= bound
                }
            }
            _ => {
                if keep_low {
                    date >= bound
                } else {
                    date <= bound
                }
            }
        }
    };
    filter
        .date_lo
        .as_deref()
        .is_none_or(|bound| within_bound(bound, true))
        && filter
            .date_hi
            .as_deref()
            .is_none_or(|bound| within_bound(bound, false))
}

/// Serve chunk documents from the committed lexical index.
///
/// The index already stores, per session, the canonical extract verbatim plus
/// the resolved identity (project, agent, date, session id, cwd) — the exact
/// inputs this module used to rebuild by re-parsing every original transcript
/// on every call. Reading them back is the same work `aicx search` does in
/// milliseconds.
///
/// CURRENT supplies membership while the source parse ledger proves each body
/// against live fingerprints, ignore policy, extract checksum and scope layout.
/// Rows that are changed, new, missing, or otherwise unproven are re-sourced
/// through the validated conversation cache; live-unadmitted rows are joined
/// afterward. A full-history request (`hours == 0`) remains the durable-identity join
///   `overlay` performs. Overlay freezes `intent1:` evidence refs, so its input
///   set must stay the census: the index is signal-filtered at write time and
///   covers only what was committed, and swapping the source under it would
///   move revisions that are meant to be stable.
#[cfg(feature = "app")]
fn collect_intent_files_from_index(
    aicx_home: &Path,
    config: &IntentsConfig,
    cutoff: DateTime<Utc>,
    source_filter: &IntentSourceFilter,
    notes: &mut ScopeNotes,
) -> Option<(Vec<StoredChunkFile>, usize, usize)> {
    if config.hours == 0 {
        return None;
    }
    let project = &config.project;
    let frame_kind = config.effective_frame_kind();
    let live = config.live;
    let adapter = crate::steer_index::open_current_adapter_at(aicx_home).ok()?;
    let current_metadata = adapter
        .scan_metadata(adapter.doc_count, |metadata| {
            current_metadata_fact(metadata, project, source_filter).is_some()
        })
        .ok()?;
    let mut current_ids = BTreeSet::new();
    let mut current_flagged_for_project = BTreeSet::new();
    let mut unidentified_flagged = 0usize;
    for metadata in current_metadata {
        let fact = current_metadata_fact(&metadata, project, source_filter)
            .expect("scan predicate admitted this metadata");
        if let Some(key) = fact.key {
            current_ids.insert(key.clone());
            if fact.flagged_for_project {
                current_flagged_for_project.insert(key);
            }
        } else if fact.flagged_for_project {
            unidentified_flagged += 1;
            crate::diagnostics::log_describe(
                "intents_index_metadata_unidentified scope_flagged=true",
            );
        }
    }

    let entries = crate::catalog::read_entries_at(aicx_home).ok()?;
    if entries.is_empty() {
        return None;
    }
    let catalog_ids = entries
        .iter()
        .map(|entry| (entry.agent.clone(), entry.session_id.clone()))
        .collect::<BTreeSet<_>>();
    let unmatched_flagged = current_flagged_for_project
        .difference(&catalog_ids)
        .cloned()
        .collect::<Vec<_>>();
    for (agent, session_id) in &unmatched_flagged {
        crate::diagnostics::log_describe(&format!(
            "intents_index_resource_unmatched agent={agent} session_id={session_id}"
        ));
    }
    let mut source_errors = unmatched_flagged.len() + unidentified_flagged;
    let mut selected = Vec::new();
    for original_entry in entries {
        if !source_agent_matches(&original_entry.agent, source_filter) {
            notes
                .selection
                .push(source_receipt(&original_entry, true, "agent_excluded"));
            continue;
        }
        let entry = match crate::catalog::recover_catalog_scope_at(aicx_home, &original_entry) {
            Ok(entries) => entries,
            Err(error) => {
                source_errors += 1;
                notes
                    .selection
                    .push(source_receipt(&original_entry, true, "source_error"));
                crate::diagnostics::log_describe(&format!(
                    "intents_index_scope_recovery_skip agent={} session_id={} error={error:#}",
                    original_entry.agent, original_entry.session_id
                ));
                continue;
            }
        };
        if !entry_matches_project(entry.project.as_deref(), project) {
            notes
                .selection
                .push(source_receipt(&entry, true, "project_excluded"));
            continue;
        }
        if crate::sessions::is_guardian_session_kind(entry.session_kind.as_deref()) {
            notes
                .selection
                .push(source_receipt(&entry, true, "control_plane"));
            continue;
        }
        selected.push(entry);
    }

    let candidates = selected
        .iter()
        .filter(|entry| current_ids.contains(&(entry.agent.clone(), entry.session_id.clone())))
        .cloned()
        .collect::<Vec<_>>();
    let mut validated =
        crate::source_index::validated_cached_extracts_at(aicx_home, &candidates).ok()?;
    let mut files = Vec::with_capacity(selected.len());
    let mut live_sessions = 0usize;
    let mut seen_sessions = BTreeSet::new();

    for entry in selected {
        let key = (entry.agent.clone(), entry.session_id.clone());
        seen_sessions.insert(key.clone());
        let reusable = validated.remove(&key).filter(|validated| {
            chunk_states_scope(&validated.chunk.metadata)
                && !validated
                    .chunk
                    .metadata
                    .get("scope_unattributed")
                    .and_then(|value| value.as_bool())
                    .unwrap_or(true)
                && validated
                    .scope
                    .as_ref()
                    .is_none_or(|scope| !scope.scope_foreign_to(entry.cwd.as_deref()))
                && !crate::sessions::is_guardian_session_kind(
                    validated
                        .chunk
                        .metadata
                        .get("session_kind")
                        .and_then(|value| value.as_str()),
                )
        });

        if let Some(validated) = reusable {
            if !matches!(
                &validated.coverage,
                crate::source_index::ConversationCoverage::CompleteVisible
            ) {
                source_errors += 1;
                crate::diagnostics::log_describe(&format!(
                    "intents_partial_index_source agent={} session_id={} coverage={:?}",
                    entry.agent, entry.session_id, validated.coverage
                ));
            }
            notes
                .coverage
                .insert(key, validated.coverage.receipt_label());
            if let Some(file) = indexed_extract_to_intent_file(
                &entry,
                validated,
                cutoff,
                frame_kind,
                live,
                source_filter,
                notes,
            ) {
                if live {
                    live_sessions += 1;
                }
                files.push(file);
            }
            continue;
        }

        let (source_path, frames, scope) = match read_intent_source(
            aicx_home,
            &entry,
            frame_kind,
            &mut source_errors,
            notes,
        ) {
            Ok(result) => result,
            Err(error) => {
                source_errors += 1;
                notes
                    .selection
                    .push(source_receipt(&entry, true, "source_error"));
                crate::diagnostics::log_describe(&format!(
                    "intents_index_resource_skip agent={} session_id={} path={} error={error:#}",
                    entry.agent, entry.session_id, entry.source_path
                ));
                continue;
            }
        };
        let in_window = frames.iter().any(|frame| {
            frame_has_conversation_time(frame)
                && frame.timestamp >= cutoff
                && frame.timestamp <= notes.now.unwrap_or_else(Utc::now)
        });
        let live_row = session_is_hot_live(live, in_window);
        if let Some(file) = catalog_frames_to_intent_file(
            &entry,
            (source_path, frames, scope),
            cutoff,
            live_row,
            source_filter,
            notes,
        ) {
            if live_row {
                live_sessions += 1;
            }
            files.push(file);
        }
    }

    if live {
        live_sessions += collect_live_unadmitted_files(
            aicx_home,
            project,
            cutoff,
            frame_kind,
            &seen_sessions,
            &mut files,
            &mut source_errors,
            source_filter,
            notes,
        )
        .ok()?;
    }

    files.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.path.cmp(&right.path))
    });
    Some((files, source_errors, live_sessions))
}

#[cfg(feature = "app")]
fn indexed_extract_to_intent_file(
    entry: &crate::catalog::CatalogEntry,
    validated: crate::source_index::ValidatedSourceExtract,
    cutoff: DateTime<Utc>,
    frame_kind: FrameKind,
    live: bool,
    source_filter: &IntentSourceFilter,
    notes: &mut ScopeNotes,
) -> Option<StoredChunkFile> {
    let mut receipt = source_receipt(entry, true, "no_frames");
    receipt.path = validated.chunk.source_path.clone();
    receipt.parser_coverage = Some(validated.coverage.receipt_label());
    let Some(project) = entry.project.clone() else {
        receipt.status = "project_excluded".into();
        notes.selection.push(receipt);
        return None;
    };

    let mut entries = parse_extract_document(&validated.chunk.text);
    entries.retain(|item| FrameKind::parse(&item.role).is_some_and(|kind| kind == frame_kind));
    receipt.parsed_frames = entries.len();
    receipt.scoped_frames = entries.len();
    receipt.unknown_time_frames = entries
        .iter()
        .filter(|item| item.timestamp.is_none())
        .count();
    let now = notes.now.unwrap_or_else(Utc::now);
    entries.retain(|item| {
        item.timestamp.is_some_and(|timestamp| {
            timestamp >= cutoff
                && timestamp <= now
                && source_date_matches_filter(
                    &timestamp.format("%Y-%m-%d").to_string(),
                    source_filter,
                )
        })
    });
    receipt.qualified_frames = entries.len();
    receipt.outside_window_frames = receipt
        .parsed_frames
        .saturating_sub(receipt.qualified_frames + receipt.unknown_time_frames);
    receipt.latest_activity = entries
        .iter()
        .filter_map(|item| item.timestamp)
        .max()
        .map(|timestamp| timestamp.to_rfc3339());
    receipt.status = if entries.is_empty() {
        if receipt.parsed_frames == 0 {
            "no_frames"
        } else if receipt.unknown_time_frames == receipt.parsed_frames {
            "unknown_time"
        } else {
            "outside_window"
        }
    } else {
        "qualified"
    }
    .into();
    notes.selection.push(receipt);
    let timestamp = entries.iter().filter_map(|item| item.timestamp).max()?;

    Some(StoredChunkFile {
        agent: entry.agent.clone(),
        date: timestamp.format("%Y-%m-%d").to_string(),
        path: PathBuf::from(validated.chunk.source_path),
        project,
        identity_source: INDEX_IDENTITY_SOURCE.to_string(),
        sequence: 0,
        timestamp,
        session_id: entry.session_id.clone(),
        honesty: if live {
            crate::oracle::ClaimHonesty::live_open()
        } else {
            crate::oracle::ClaimHonesty::canonical()
        },
        scope: validated.scope,
        transcript_entries: Some(entries),
        body: None,
    })
}

/// Apply the caller's `-p` predicate to one stored project address.
///
/// The one place the filter lives: the index re-source lane, the catalog
/// census and the legacy chunk walk all decide membership the same way.
/// An empty filter matches everything; a row without a project never does.
fn entry_matches_project(entry_project: Option<&str>, project: &str) -> bool {
    if project.trim().is_empty() {
        return true;
    }
    let Some(entry_project) = entry_project else {
        return false;
    };
    let (organization, repository) = entry_project.split_once('/').unwrap_or(("", entry_project));
    legacy_archive::project_filter_matches(organization, repository, project)
}

#[cfg(feature = "app")]
fn collect_intent_files(
    aicx_home: &Path,
    config: &IntentsConfig,
    cutoff: DateTime<Utc>,
    source_filter: &IntentSourceFilter,
    notes: &mut ScopeNotes,
) -> Result<(Vec<StoredChunkFile>, usize, &'static str, usize)> {
    let project = &config.project;
    let frame_kind = config.effective_frame_kind();
    let live = config.live;
    // Prefer the committed index: same documents, no transcript re-parse.
    if let Some((files, source_errors, live_sessions)) =
        collect_intent_files_from_index(aicx_home, config, cutoff, source_filter, notes)
    {
        return Ok((files, source_errors, INDEX_IDENTITY_SOURCE, live_sessions));
    }

    let entries = crate::catalog::read_entries_at(aicx_home)?;
    if entries.is_empty() {
        return Ok((
            collect_legacy_chunk_files(aicx_home, project, cutoff, frame_kind, source_filter)?,
            0,
            PERSISTED_IDENTITY_SOURCE,
            0,
        ));
    }

    let mut files = Vec::new();
    let mut source_errors = 0usize;
    let mut live_sessions = 0usize;
    let mut seen_sessions: BTreeSet<(String, String)> = BTreeSet::new();
    for original_entry in entries {
        if !source_agent_matches(&original_entry.agent, source_filter) {
            notes
                .selection
                .push(source_receipt(&original_entry, true, "agent_excluded"));
            continue;
        }
        let entry = match crate::catalog::recover_catalog_scope_at(aicx_home, &original_entry) {
            Ok(entry) => entry,
            Err(error) => {
                source_errors += 1;
                notes
                    .selection
                    .push(source_receipt(&original_entry, true, "source_error"));
                crate::diagnostics::log_describe(&format!(
                    "intents_scope_recovery_skip agent={} session_id={} error={error:#}",
                    original_entry.agent, original_entry.session_id
                ));
                continue;
            }
        };
        if !entry_matches_project(entry.project.as_deref(), project) {
            notes
                .selection
                .push(source_receipt(&entry, true, "project_excluded"));
            continue;
        }
        if crate::sessions::is_guardian_session_kind(entry.session_kind.as_deref()) {
            notes
                .selection
                .push(source_receipt(&entry, true, "control_plane"));
            continue;
        }
        let (source_path, frames, scope) =
            match read_intent_source(aicx_home, &entry, frame_kind, &mut source_errors, notes) {
                Ok(result) => result,
                Err(error) => {
                    source_errors += 1;
                    notes
                        .selection
                        .push(source_receipt(&entry, true, "source_error"));
                    crate::diagnostics::log_describe(&format!(
                        "intents_source_skip agent={} session_id={} path={} error={error:#}",
                        entry.agent, entry.session_id, entry.source_path
                    ));
                    continue;
                }
            };
        let in_window = frames.iter().any(|frame| {
            frame_has_conversation_time(frame)
                && frame.timestamp >= cutoff
                && frame.timestamp <= notes.now.unwrap_or_else(Utc::now)
        });
        let live_row = session_is_hot_live(live, in_window);
        let Some(file) = catalog_frames_to_intent_file(
            &entry,
            (source_path, frames, scope),
            cutoff,
            live_row,
            source_filter,
            notes,
        ) else {
            continue;
        };
        seen_sessions.insert((entry.agent.clone(), entry.session_id.clone()));
        if live_row {
            live_sessions += 1;
        }
        files.push(file);
    }

    if live {
        live_sessions += collect_live_unadmitted_files(
            aicx_home,
            project,
            cutoff,
            frame_kind,
            &seen_sessions,
            &mut files,
            &mut source_errors,
            source_filter,
            notes,
        )?;
    }

    files.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok((files, source_errors, CATALOG_IDENTITY_SOURCE, live_sessions))
}

#[cfg(feature = "app")]
fn catalog_frames_to_intent_file(
    entry: &crate::catalog::CatalogEntry,
    conversation: (
        PathBuf,
        Vec<TimelineEntry>,
        crate::extraction::conversation::ScopeReport,
    ),
    cutoff: DateTime<Utc>,
    live_row: bool,
    source_filter: &IntentSourceFilter,
    notes: &mut ScopeNotes,
) -> Option<StoredChunkFile> {
    // Scope is of the WHOLE session, computed before the frame-kind filter: a
    // per-role view can see at most half the evidence.
    let (source_path, mut frames, scope) = conversation;
    let mut receipt = source_receipt(entry, true, "no_frames");
    receipt.path = source_path.to_string_lossy().into_owned();
    receipt.parsed_frames = frames.len();
    receipt.parser_coverage = notes
        .coverage
        .get(&(entry.agent.clone(), entry.session_id.clone()))
        .cloned();
    // Guardian sessions are control-plane evidence: they never enter the
    // intent stream, and their scope never enters the mixed-scope telemetry
    // either, regardless of which lane called us.
    if is_guardian_session(entry, &source_path, &frames) {
        receipt.status = "control_plane".into();
        notes.selection.push(receipt);
        return None;
    }
    let Some(project) = entry.project.clone() else {
        // No bucket to filter into: only the session's own spread is news.
        note_mixed_scope(
            &mut notes.mixed,
            &entry.agent,
            &entry.session_id,
            &scope,
            scope.scope_mixed(),
        );
        receipt.status = "project_excluded".into();
        notes.selection.push(receipt);
        return None;
    };
    // Scope is judged on the whole session, before the project filter narrows
    // the frames to one bucket, and the telemetry is noted before the filter
    // runs: fail-closed must not silence mixed evidence even when it removes
    // every frame. One verdict serves both, so the telemetry names exactly the
    // sessions the filter could not serve whole. A session it can serve loses
    // only its unplaced frames, and those are noted too.
    let foreign = scope.scope_foreign_to(entry.cwd.as_deref());
    note_mixed_scope(
        &mut notes.mixed,
        &entry.agent,
        &entry.session_id,
        &scope,
        foreign,
    );
    if !foreign {
        note_unplaced_frames(
            &mut notes.unplaced,
            &entry.agent,
            &entry.session_id,
            &frames,
        );
    }
    retain_frames_for_project(&mut frames, &project, entry.cwd.as_deref(), foreign);
    receipt.scoped_frames = frames.len();
    receipt.unknown_time_frames = frames
        .iter()
        .filter(|f| !frame_has_conversation_time(f))
        .count();
    let now = notes.now.unwrap_or_else(Utc::now);
    let unbounded = cutoff == DateTime::UNIX_EPOCH;
    frames.retain(|frame| {
        let known_time = frame_has_conversation_time(frame);
        let inside_window = (known_time && frame.timestamp >= cutoff && frame.timestamp <= now)
            || (unbounded && !known_time);
        let inside_query_dates =
            if source_filter.date_lo.is_none() && source_filter.date_hi.is_none() {
                true
            } else {
                known_time
                    && source_date_matches_filter(
                        &frame.timestamp.format("%Y-%m-%d").to_string(),
                        source_filter,
                    )
            };
        inside_window && inside_query_dates
    });
    receipt.qualified_frames = frames
        .iter()
        .filter(|f| frame_has_conversation_time(f))
        .count();
    receipt.human_messages = frames.iter().filter(|f| is_user_role(&f.role)).count();
    receipt.scope_withheld_frames = receipt.parsed_frames.saturating_sub(receipt.scoped_frames);
    receipt.outside_window_frames = receipt
        .scoped_frames
        .saturating_sub(receipt.qualified_frames + receipt.unknown_time_frames);
    receipt.latest_activity = frames
        .iter()
        .filter(|f| frame_has_conversation_time(f))
        .map(|f| f.timestamp)
        .max()
        .map(|t| t.to_rfc3339());
    receipt.status = if frames.is_empty() {
        if receipt.parsed_frames == 0 {
            "no_frames"
        } else if receipt.scoped_frames == 0 {
            "scope_withheld"
        } else if receipt.unknown_time_frames == receipt.scoped_frames {
            "unknown_time"
        } else {
            "outside_window"
        }
    } else if receipt.qualified_frames == 0 {
        "unbounded_unknown_time"
    } else {
        "qualified"
    }
    .into();
    notes.selection.push(receipt);
    if frames.is_empty() {
        return None;
    }
    let timestamp = frames
        .iter()
        .filter(|frame| frame_has_conversation_time(frame))
        .map(|frame| frame.timestamp)
        .max()
        .unwrap_or(DateTime::UNIX_EPOCH);
    Some(StoredChunkFile {
        agent: entry.agent.clone(),
        date: if timestamp == DateTime::UNIX_EPOCH {
            "unknown".into()
        } else {
            timestamp.format("%Y-%m-%d").to_string()
        },
        path: source_path,
        project: project.clone(),
        identity_source: CATALOG_IDENTITY_SOURCE.to_string(),
        sequence: 0,
        timestamp,
        session_id: entry.session_id.clone(),
        honesty: if live_row {
            crate::oracle::ClaimHonesty::live_open()
        } else {
            crate::oracle::ClaimHonesty::canonical()
        },
        scope: Some(scope),
        transcript_entries: Some(
            frames
                .into_iter()
                .enumerate()
                .map(|(ordinal, frame)| TranscriptEntry {
                    timestamp: frame_has_conversation_time(&frame).then_some(frame.timestamp),
                    locator: Some(frame.source_line_span.map_or_else(
                        || format!("role-frame:{ordinal}"),
                        |(start, end)| format!("source-lines:{start}-{end}"),
                    )),
                    cwd: frame.cwd,
                    role: frame.role,
                    lines: frame.message.lines().map(str::to_string).collect(),
                })
                .collect(),
        ),
        body: None,
    })
}

/// Is this session a guardian — control-plane evidence that stays out of the
/// operator intent stream and out of its scope telemetry?
///
/// The frames are the authority: their provenance was resolved where the
/// source was opened ([`crate::sessions::resolve_session_kind`]), so this also
/// catches catalog rows cataloged before the column existed — the column-only
/// guards in the lanes are fast paths, not the contract. The frame-kind and
/// privacy filters can leave no frame to carry that provenance, and a guardian
/// must not reach the telemetry through an empty view, so an empty view asks
/// the source once more (one bounded header read).
#[cfg(feature = "app")]
fn is_guardian_session(
    entry: &crate::catalog::CatalogEntry,
    source_path: &Path,
    frames: &[TimelineEntry],
) -> bool {
    if crate::sessions::is_guardian_session_kind(entry.session_kind.as_deref()) {
        return true;
    }
    if frames.is_empty() {
        let resolved = crate::sessions::resolve_session_kind(
            &entry.agent,
            entry.session_kind.as_deref(),
            source_path,
        );
        return crate::sessions::is_guardian_session_kind(resolved.as_deref());
    }
    crate::sessions::is_guardian_session_kind(
        frames
            .iter()
            .find_map(|frame| frame.session_kind.as_deref()),
    )
}

/// Overlay's full-history census uses the same per-lane global caps, task
/// reconciliation and dedup as `intents`. Only the source-read boundary differs:
/// a cleaned conversation can be reused for both lanes, including from cache.
///
/// Each conversation carries the scope report of its WHOLE session, built by
/// the reader before the signal projection and the frame-kind filter, exactly
/// as the census lane receives it. A report rebuilt here from the cleaned
/// frames would miss the workdirs seen only on tool calls and the scopes
/// `.aicxignore` hid.
#[cfg(feature = "app")]
pub(crate) fn extract_overlay_intents_from_conversations(
    project: &str,
    conversations: &[(
        crate::catalog::CatalogEntry,
        PathBuf,
        Vec<TimelineEntry>,
        crate::extraction::conversation::ScopeReport,
    )],
) -> Result<Vec<IntentRecord>> {
    let cutoff = DateTime::<Utc>::from_timestamp(0, 0).expect("valid Unix epoch");
    let mut records = Vec::new();
    for kind in [FrameKind::UserMsg, FrameKind::AgentReply] {
        let config = IntentsConfig {
            project: project.to_owned(),
            hours: 0,
            strict: false,
            min_confidence: None,
            kind_filter: None,
            frame_kind: Some(kind),
            live: false,
        };
        let mut notes = ScopeNotes::default();
        let mut files = conversations
            .iter()
            .filter_map(|(entry, path, frames, scope)| {
                let frames = frames
                    .iter()
                    .filter(|frame| {
                        frame.frame_kind.unwrap_or(match frame.role.as_str() {
                            "user" => FrameKind::UserMsg,
                            "assistant" => FrameKind::AgentReply,
                            _ => FrameKind::SystemNote,
                        }) == kind
                    })
                    .cloned()
                    .collect();
                catalog_frames_to_intent_file(
                    entry,
                    (path.clone(), frames, scope.clone()),
                    cutoff,
                    false,
                    &IntentSourceFilter::default(),
                    &mut notes,
                )
            })
            .collect::<Vec<_>>();
        files.sort_by(|left, right| {
            left.timestamp
                .cmp(&right.timestamp)
                .then_with(|| left.path.cmp(&right.path))
        });
        let extraction = extract_intents_from_files_with_stats(
            &config,
            files,
            0,
            CATALOG_IDENTITY_SOURCE,
            0,
            &IntentSourceFilter::default(),
            notes,
        )?;
        records.extend(extraction.records);
    }
    Ok(records)
}

/// Admit sessions the durable catalog census does not know yet (P0 live
/// window): scan live source roots, keep entries whose conversation date
/// (or last frame, when undated) is inside the window, parse them through
/// the same catalog-source reader, and stamp records with the `open_session`
/// honesty frame. Source mtime is not a membership clock.
#[cfg(feature = "app")]
#[allow(clippy::too_many_arguments)]
fn collect_live_unadmitted_files(
    aicx_home: &Path,
    project: &str,
    cutoff: DateTime<Utc>,
    frame_kind: FrameKind,
    seen_sessions: &BTreeSet<(String, String)>,
    files: &mut Vec<StoredChunkFile>,
    source_errors: &mut usize,
    source_filter: &IntentSourceFilter,
    notes: &mut ScopeNotes,
) -> Result<usize> {
    let user_home = crate::os_user_home().unwrap_or_else(|| aicx_home.to_path_buf());
    let cutoff_unix_ns = cutoff
        .timestamp_nanos_opt()
        .map(|nanos| nanos.max(0) as u128)
        .unwrap_or(0);
    let delta = crate::catalog::live_delta(aicx_home, &user_home, cutoff_unix_ns)?;
    let mut admitted = 0usize;
    for entry in delta.unadmitted {
        if !source_agent_matches(&entry.agent, source_filter) {
            notes
                .selection
                .push(source_receipt(&entry, false, "agent_excluded"));
            continue;
        }
        if seen_sessions.contains(&(entry.agent.clone(), entry.session_id.clone())) {
            continue;
        }
        if !entry_matches_project(entry.project.as_deref(), project) {
            notes
                .selection
                .push(source_receipt(&entry, false, "project_excluded"));
            continue;
        }
        let (source_path, frames, scope) =
            match read_intent_source(aicx_home, &entry, frame_kind, source_errors, notes) {
                Ok(result) => result,
                Err(error) => {
                    *source_errors += 1;
                    notes
                        .selection
                        .push(source_receipt(&entry, false, "source_error"));
                    crate::diagnostics::log_describe(&format!(
                        "intents_live_source_skip agent={} session_id={} error={error:#}",
                        entry.agent, entry.session_id
                    ));
                    continue;
                }
            };
        let selection_before = notes.selection.len();
        if let Some(mut file) = catalog_frames_to_intent_file(
            &entry,
            (source_path, frames, scope),
            cutoff,
            true,
            source_filter,
            notes,
        ) {
            file.identity_source = LIVE_SCAN_IDENTITY_SOURCE.into();
            admitted += 1;
            files.push(file);
        }
        for receipt in &mut notes.selection[selection_before..] {
            receipt.admitted = false;
        }
    }
    Ok(admitted)
}

/// Keep per-frame checkout truth ahead of a session's start-directory label —
/// fail-closed inside mixed sessions.
///
/// Agents can `cd` into another repository without starting a new session.
/// Frames that carry an explicit cwd must therefore prove they still belong
/// to the requested canonical bucket. A frame whose turn-window workdir
/// evidence conflicted (`scope_conflict`) never proves membership and is
/// always dropped from a project-filtered query. A frame with NO cwd evidence
/// inherits the catalog attribution only while the session scope is
/// homogeneous; once the session is a mixed candidate, silence is not proof
/// and the frame stays unattributed instead of leaking into the project.
///
/// Membership has two proofs, either suffices:
/// 1. the frame cwd is the session checkout (or a subdirectory of it) —
///    the catalog identity was derived from that very checkout (git remote),
///    and a checkout path need not spell `org/repo` in adjacent segments
///    (suite dirs, renamed clones);
/// 2. the frame cwd spells the project as adjacent path segments — the
///    strict anti-leak matcher for frames that left the session checkout.
///    Spelling of an unresolved path is honest only when the session
///    checkout is equally unresolved. A resolved checkout whose baseline is
///    gone may stand in only when that checkout's own owner and repository
///    are the requested project. A shared leaf name is a different
///    repository, and an ancestor directory in the path is not the checkout.
///    A frame with no session checkout at all is dropped.
#[cfg(feature = "app")]
fn retain_frames_for_project(
    frames: &mut Vec<TimelineEntry>,
    project: &str,
    session_cwd: Option<&str>,
    session_mixed: bool,
) {
    let filters = [project.to_string()];
    let session_root = session_cwd
        .map(|cwd| cwd.trim_end_matches(['/', '\\']))
        .filter(|cwd| !cwd.is_empty());
    frames.retain(|frame| {
        // Durable "do not inherit" states: proven divergence and unresolved
        // foreign evidence never join a project bucket.
        if frame.scope_conflict || frame.scope_unattributed {
            return false;
        }
        match frame.cwd.as_deref() {
            Some(cwd) => {
                // Repo identity, never a path prefix: a nested checkout or
                // submodule lives lexically under the session checkout and is
                // a different repository. Accepting containment by string
                // shape is the cross-repo leak this filter exists to close.
                if session_root
                    .is_some_and(|root| aicx_parser::engine::workdir_within_scope(cwd, root))
                {
                    return true;
                }
                // The path-segment fallback below is a legacy heuristic over
                // path SPELLING, and a nested checkout at
                // `…/vista/vendor/fleet-bus` still spells `vista` — it
                // re-admits exactly what repo identity just rejected. Reaching
                // it is only honest when there was no identity to be had.
                match aicx_parser::engine::normalize_workdir(cwd, session_root) {
                    // Membership against a live session checkout already failed.
                    // A vanished baseline cannot prove identity. The frame's own
                    // git root may stand in only when its owner and repository
                    // are this project — not when another repo wears the same
                    // leaf, and not when an ancestor directory spells the name.
                    // `…/other/aicx` is not `Loctree/aicx`.
                    aicx_parser::engine::WorkdirIdentity::Resolved(root) => {
                        session_root.is_some_and(|baseline| {
                            matches!(
                                aicx_parser::engine::normalize_workdir(baseline, None),
                                aicx_parser::engine::WorkdirIdentity::Unresolved(_)
                            )
                        }) && resolved_owner_and_repo_are_the_project(&root, project)
                    }
                    // Spelling is evidence only where the session root is as
                    // unknowable as the frame. A root that resolves here has
                    // just refused a path it cannot prove — a removed nested
                    // checkout at `…/vista/vendor/fleet-bus` still spells
                    // `vista` — and with no root at all there is nothing the
                    // frame could belong to, which is how the whole-session
                    // lane (`ScopeReport::scope_foreign_to`) reads it too.
                    aicx_parser::engine::WorkdirIdentity::Unresolved(_) => {
                        // A directory that is gone, whose parent is still this
                        // checkout, is that checkout: `…/vibecrafted/labs` did
                        // not become another repository by being deleted. A
                        // path whose parent is gone too (`…/vista/vendor/fleet-bus`)
                        // is unprovable and must not be spelled back in.
                        missing_directory_inside_resolved_checkout(cwd, session_root)
                            || (session_root.is_some_and(|root| {
                                matches!(
                                    aicx_parser::engine::normalize_workdir(root, None),
                                    aicx_parser::engine::WorkdirIdentity::Unresolved(_)
                                )
                            }) && crate::extraction::project_filter_matches_path(cwd, &filters))
                    }
                }
            }
            None => !session_mixed,
        }
    });
}

/// A missing path whose parent still belongs to the session checkout.
///
/// The parent has to exist and resolve to the same repo root. A removed
/// nested tree (`vendor/fleet-bus` when `vendor` is gone) has no parent to
/// stand on, so it stays unprovable instead of inheriting the catalog project.
#[cfg(feature = "app")]
fn missing_directory_inside_resolved_checkout(cwd: &str, session_root: Option<&str>) -> bool {
    let Some(session_root) = session_root else {
        return false;
    };
    let path = Path::new(cwd);
    if path.exists() {
        return false;
    }
    let Some(parent) = path.parent().filter(|parent| parent.exists()) else {
        return false;
    };
    let Some(parent) = parent.to_str() else {
        return false;
    };
    match (
        aicx_parser::engine::normalize_workdir(parent, None),
        aicx_parser::engine::normalize_workdir(session_root, None),
    ) {
        (
            aicx_parser::engine::WorkdirIdentity::Resolved(parent_root),
            aicx_parser::engine::WorkdirIdentity::Resolved(session),
        ) => parent_root == session,
        _ => false,
    }
}

/// The frame's git root is this project, not a neighbor with the same leaf.
///
/// Only a strict `owner/repo` filter counts, and only against the checkout's
/// own last two segments. `/aicx` and bare `aicx` match every repository
/// named `aicx`. `…/vista/vendor/fleet-bus` still has `vista` above it and
/// is a different repository.
#[cfg(feature = "app")]
fn resolved_owner_and_repo_are_the_project(root: &Path, project: &str) -> bool {
    let Some((organization, repository)) = strict_owner_repo(project) else {
        return false;
    };
    let mut segments = root
        .components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>();
    let Some(root_repository) = segments.pop() else {
        return false;
    };
    let Some(root_organization) = segments.pop() else {
        return false;
    };
    root_organization.eq_ignore_ascii_case(organization)
        && root_repository.eq_ignore_ascii_case(repository)
}

/// `owner/repo` only. A leading slash, a trailing slash, or a bare name is a
/// leaf or wildcard, and two repositories can share that name.
#[cfg(feature = "app")]
fn strict_owner_repo(project: &str) -> Option<(&str, &str)> {
    let project = project.trim();
    if project.is_empty() || project.starts_with(['/', '\\']) || project.ends_with(['/', '\\']) {
        return None;
    }
    let split_at = project.find(['/', '\\'])?;
    let organization = &project[..split_at];
    let repository = &project[split_at + 1..];
    if organization.is_empty() || repository.is_empty() || repository.contains(['/', '\\']) {
        return None;
    }
    Some((organization, repository))
}

/// Catalog-admitted sessions stay live when conversation activity is inside
/// the requested window. Source mtime must not promote a stale session.
#[cfg(feature = "app")]
pub(crate) fn session_is_hot_live(live: bool, conversation_in_window: bool) -> bool {
    live && conversation_in_window
}

#[cfg(not(feature = "app"))]
fn collect_intent_files(
    aicx_home: &Path,
    config: &IntentsConfig,
    cutoff: DateTime<Utc>,
    source_filter: &IntentSourceFilter,
    _notes: &mut ScopeNotes,
) -> Result<(Vec<StoredChunkFile>, usize, &'static str, usize)> {
    // `loctree-consumer` is the legacy read-core profile: it deliberately
    // excludes app-only source discovery and catalog parsing — including the
    // lexical index, so the window/live/full-history distinctions have no
    // source to choose between here. Keep pure intent extraction available
    // over explicitly supplied legacy artifacts without pulling the full
    // CLI/index graph into the library feature.
    Ok((
        collect_legacy_chunk_files(
            aicx_home,
            &config.project,
            cutoff,
            config.effective_frame_kind(),
            source_filter,
        )?,
        0,
        PERSISTED_IDENTITY_SOURCE,
        0,
    ))
}

fn collect_legacy_chunk_files(
    aicx_home: &Path,
    project: &str,
    cutoff: DateTime<Utc>,
    frame_kind: FrameKind,
    source_filter: &IntentSourceFilter,
) -> Result<Vec<StoredChunkFile>> {
    let mut files = Vec::new();
    let scan_root = normalize_scan_root(aicx_home);

    for file in legacy_archive::scan_context_files_at(&scan_root)? {
        if file.path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        // Keep intents aligned with every other `-p` surface: canonical
        // owner/repo equality, explicit owner/repo wildcards, and bare-name
        // equality. Legacy ownerless buckets use the virtual `_/repo` address;
        // their physical store path remains unchanged.
        let persisted_project = persisted_project_from_card(&file.path)?;
        let (identity_project, identity_source) = persisted_project
            .map(|identity_project| (identity_project, PERSISTED_IDENTITY_SOURCE))
            .unwrap_or_else(|| (file.project.clone(), PATH_HEURISTIC_IDENTITY_SOURCE));
        if !entry_matches_project(Some(identity_project.as_str()), project) {
            continue;
        }
        let sidecar = legacy_archive::load_sidecar(&file.path);
        if sidecar.as_ref().is_some_and(|sidecar| {
            sidecar.artifact_family.as_deref() == Some(legacy_archive::LOCT_CONTEXT_PACK_FAMILY)
                || sidecar
                    .truth_status
                    .as_ref()
                    .is_some_and(|status| status.role == crate::chunker::TruthRole::Example)
        }) {
            continue;
        }
        // Claim-honesty frame travels from the v2 sidecar into every record
        // extracted from this chunk; pre-v2 sidecars leave it empty (rendered
        // as unknown, serialized as no keys).
        let honesty = sidecar
            .as_ref()
            .map(|sidecar| crate::oracle::ClaimHonesty {
                claim_scope: sidecar.claim_scope.clone(),
                freshness_contract: sidecar.freshness_contract.clone(),
                verification_state: sidecar.verification_state.clone(),
            })
            .unwrap_or_default();
        // Legacy chunks (no sidecar yet, or a pre-frame_kind sidecar) belong
        // to the default user_msg lane; requiring an explicit frame_kind here
        // silently emptied intents on stores written before the field existed.
        let chunk_frame = sidecar
            .and_then(|sidecar| sidecar.frame_kind)
            .unwrap_or_else(IntentsConfig::default_frame_kind);
        if chunk_frame != frame_kind {
            continue;
        }

        // Legacy card paths carry a canonical day but no independently trusted
        // agent identity. Date pruning is safe here; agent pruning remains a
        // display-stage fallback instead of guessing from the filename.
        if !source_date_matches_filter(&file.date_iso, source_filter) {
            continue;
        }

        // Recency is anchored to the canonical chunk date encoded in the store
        // layout. Filesystem mtime drifts during daily sync/migration and must
        // not make stale sessions look fresh.
        let canonical_date = NaiveDate::parse_from_str(&file.date_iso, "%Y-%m-%d").ok();
        let timestamp = canonical_date.and_then(|date| combine_date_time(date, "000000"));
        let Some(timestamp) = timestamp else {
            continue;
        };
        if let Some(date) = canonical_date {
            if date < cutoff.date_naive() {
                continue;
            }
        } else if timestamp < cutoff {
            continue;
        }

        files.push(StoredChunkFile {
            agent: file.agent,
            date: file.date_iso,
            path: file.path,
            project: identity_project,
            identity_source: identity_source.to_string(),
            sequence: file.chunk,
            timestamp,
            session_id: file.session_id,
            honesty,
            scope: None,
            transcript_entries: None,
            body: None,
        });
    }

    files.sort_by(|left, right| {
        left.timestamp
            .cmp(&right.timestamp)
            .then_with(|| left.sequence.cmp(&right.sequence))
            .then_with(|| left.path.cmp(&right.path))
    });

    Ok(files)
}

fn persisted_project_from_card(path: &Path) -> Result<Option<String>> {
    let file = sanitize::open_file_validated(path)
        .with_context(|| format!("Failed to open chunk header: {}", path.display()))?;
    let mut prefix = Vec::new();
    file.take(CARD_HEADER_READ_LIMIT)
        .read_to_end(&mut prefix)
        .with_context(|| format!("Failed to read chunk header: {}", path.display()))?;
    let prefix = String::from_utf8_lossy(&prefix);
    Ok(crate::card_header::parse_card_header(&prefix)
        .and_then(|header| header.project)
        .filter(|project| {
            project
                .split_once('/')
                .is_some_and(|(organization, repository)| {
                    !organization.trim().is_empty()
                        && !repository.trim().is_empty()
                        && !repository.contains('/')
                })
        }))
}

fn normalize_scan_root(aicx_home: &Path) -> PathBuf {
    if aicx_home
        .file_name()
        .is_some_and(|name| name == legacy_archive::LEGACY_CARDS_DIRNAME)
    {
        return aicx_home
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| aicx_home.to_path_buf());
    }

    aicx_home.to_path_buf()
}

fn combine_date_time(date: NaiveDate, time: &str) -> Option<DateTime<Utc>> {
    let time = NaiveTime::parse_from_str(time, "%H%M%S").ok()?;
    let datetime = NaiveDateTime::new(date, time);
    Some(DateTime::<Utc>::from_naive_utc_and_offset(datetime, Utc))
}

/// Parse a session extract as rendered into the lexical index.
///
/// `source_index::render_extract` emits a different shape than the card
/// documents `parse_chunk_document` handles: a `# AICX session extract`
/// preamble, then one `## <rfc3339> · <role>` heading per frame. Feeding it to
/// the card parser silently loses every role — the classifier then cannot tell
/// operator input from assistant prose, which is precisely the distinction
/// `--frame-kind` exists to make.
///
/// Fenced blocks are skipped, as in the card parser: pasted shell output,
/// diffs, dispatch briefs and repeated `/loop` directives are quoted
/// boilerplate, not fresh intent. That is the same call `is_signal_frame`
/// makes upstream when the extract is written.
fn parse_extract_document(content: &str) -> Vec<TranscriptEntry> {
    const ROLE_SEPARATOR: &str = " · ";

    let mut entries: Vec<TranscriptEntry> = Vec::new();
    let mut current: Option<TranscriptEntry> = None;
    let mut fenced = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        if let Some(heading) = trimmed.strip_prefix("## ")
            && let Some((timestamp, role)) = heading.rsplit_once(ROLE_SEPARATOR)
        {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(TranscriptEntry {
                timestamp: DateTime::parse_from_rfc3339(timestamp.trim())
                    .ok()
                    .map(|t| t.with_timezone(&Utc)),
                locator: Some(format!("extract-heading:{}", entries.len())),
                cwd: None,
                role: role.trim().to_string(),
                lines: Vec::new(),
            });
            continue;
        }
        if let Some(entry) = current.as_mut() {
            entry.lines.push(line.to_string());
        }
    }

    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    entries
}

fn parse_chunk_document(content: &str) -> (Vec<String>, Vec<TranscriptEntry>) {
    let mut in_signals = false;
    let mut fenced = false;
    let mut signal_lines = Vec::new();
    let mut transcript_lines = Vec::new();

    // Header-agnostic: strip the card header (bracket or frontmatter)
    // structurally so frontmatter meta lines never read as transcript.
    for line in crate::card_header::card_body(content).lines() {
        let trimmed = line.trim();
        if trimmed == "[signals]" {
            in_signals = true;
            continue;
        }
        if trimmed == "[/signals]" {
            in_signals = false;
            continue;
        }
        if in_signals {
            signal_lines.push(line.to_string());
            continue;
        }
        if crate::card_header::is_bracket_header_line(trimmed) {
            continue;
        }
        // Track triple-backtick fence in the transcript section. Lines inside a
        // fenced block (e.g. pasted code, shell output, JSON dumps) are quoted
        // material — classifying them as user intents or assistant decisions is
        // a category error (`let's encrypt` inside a code block is a tool name,
        // not an intent).
        if trimmed.starts_with("```") {
            fenced = !fenced;
            continue;
        }
        if fenced {
            continue;
        }
        transcript_lines.push(line.to_string());
    }

    (signal_lines, parse_transcript_entries(&transcript_lines))
}

fn parse_transcript_entries(lines: &[String]) -> Vec<TranscriptEntry> {
    let mut entries = Vec::new();
    let mut current: Option<TranscriptEntry> = None;

    for line in lines {
        if let Some((role, first_line)) = parse_transcript_header(line) {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            current = Some(TranscriptEntry {
                timestamp: None,
                locator: Some(format!("transcript-entry:{}", entries.len())),
                cwd: None,
                role,
                lines: vec![first_line],
            });
            continue;
        }

        if let Some(entry) = current.as_mut() {
            entry.lines.push(line.clone());
        }
    }

    if let Some(entry) = current {
        entries.push(entry);
    }

    entries
}

fn parse_transcript_header(line: &str) -> Option<(String, String)> {
    if !line.starts_with('[') {
        return None;
    }

    let close = line.find(']')?;
    let time = &line[1..close];
    if time.len() != 8
        || !time.bytes().enumerate().all(|(idx, byte)| match idx {
            2 | 5 => byte == b':',
            _ => byte.is_ascii_digit(),
        })
    {
        return None;
    }

    let rest = line.get(close + 1..)?.trim_start();
    let colon = rest.find(':')?;
    let role = rest[..colon].trim();
    if role.is_empty() {
        return None;
    }

    let message = rest[colon + 1..].trim_start().to_string();
    Some((role.to_string(), message))
}

fn extract_signal_candidates(
    file: &StoredChunkFile,
    project: &str,
    source_chunk: &str,
    signal_lines: &[String],
) -> (Vec<IntentCandidate>, Vec<TaskEvent>) {
    let mut candidates = Vec::new();
    let mut task_events = Vec::new();
    let mut section = SignalSection::None;
    let mut in_skill_banner = false;
    // E.8: track ``` fenced blocks within the signal section. Pasted markdown
    // snippets inside [signals] (e.g. example "- [ ] task" demonstrating
    // checklist syntax) must not be picked up as real tasks.
    let mut in_fence = false;

    for raw_line in signal_lines {
        let line = raw_line.trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            continue;
        }
        if line == "=== SKILL ENTER ===" {
            in_skill_banner = true;
            continue;
        }
        if line == "===================" {
            in_skill_banner = false;
            continue;
        }
        if in_skill_banner {
            continue;
        }

        match line {
            "Intent:" => {
                section = SignalSection::Intent;
                continue;
            }
            "Decision:" => {
                section = SignalSection::Decision;
                continue;
            }
            "Results:" => {
                section = SignalSection::Results;
                continue;
            }
            "Outcome:" => {
                section = SignalSection::Outcome;
                continue;
            }
            "Ultrathink:" | "Insight:" | "Plan mode:" | "Notes:" => {
                section = SignalSection::Ignore;
                continue;
            }
            _ => {}
        }

        if let Some((is_done, task)) = parse_checklist_task(line) {
            if let Some(event) = build_task_event(
                &task,
                None,
                file,
                project,
                source_chunk,
                !is_done,
                true,
                None,
            ) {
                task_events.push(event);
            }
            continue;
        }

        if line.starts_with("RED LIGHT: checklist detected")
            || line.starts_with("Checklist detected")
            || line.starts_with("... (+")
            || is_source_metadata_line(line)
            || is_reingested_charter_line(line)
        {
            continue;
        }

        let payload = strip_signal_bullet(line);
        if is_source_metadata_line(payload)
            || is_local_command_artifact_line(payload)
            || is_reingested_charter_line(payload)
            || is_code_fragment_line(payload)
        {
            continue;
        }
        let declared = match section {
            SignalSection::Intent => Some(IntentKind::Intent),
            SignalSection::Decision => Some(IntentKind::Decision),
            SignalSection::Results | SignalSection::Outcome => Some(IntentKind::Outcome),
            SignalSection::Ignore | SignalSection::None => None,
        };

        let built = match declared {
            // Section-tagged line: the [signals] header is a strong hint, but it
            // must pass the shared classifier before becoming a record (oś 1).
            Some(declared) => revalidate_signal_kind(declared, payload).and_then(|verdict| {
                build_candidate(
                    verdict.kind,
                    payload,
                    None,
                    file,
                    project,
                    source_chunk,
                    verdict.trusted,
                    verdict.source,
                )
            }),
            // No section header: classify the line, but still tag it as
            // signal-sourced (it came from the [signals] block).
            None => infer_kind_from_line(payload, false).and_then(|kind| {
                build_candidate(
                    kind,
                    payload,
                    None,
                    file,
                    project,
                    source_chunk,
                    true,
                    Some(SIGNAL_SOURCE.to_string()),
                )
            }),
        };

        if let Some(candidate) = built {
            candidates.push(candidate);
        }
    }

    (candidates, task_events)
}

/// Kind + provenance verdict from passing a `[signals]` section line through the
/// shared semantic classifier (Round II / oś 1).
struct SignalVerdict {
    kind: IntentKind,
    /// Provenance for `IntentRecord.source`: `None` when the section hint is
    /// honored as-is, `Some("signals:revalidated(<from>->,<to>)")` when the
    /// classifier overrode it.
    source: Option<String>,
    /// Whether to score this with full signal confidence. Honored hints stay
    /// trusted; overridden lines drop to normal classified confidence.
    trusted: bool,
}

/// Revalidate a `[signals]` section-declared kind against the shared classifier.
///
/// `[signals]` headers (`Intent:`/`Decision:`/`Results:`/`Outcome:`) are a
/// strong upstream hint, but they must not bypass the ontology — previously the
/// section header alone set the kind, so e.g. a question filed under `Results:`
/// became an outcome. The classifier now runs on the payload; on a confident
/// contrary reading it wins (operator decision 2026-06-21). Returns `None` when
/// the classifier confidently reads the line as a non-bucket role
/// (assumption/insight/argue/commitment), dropping the section's false positive.
/// Provenance tag for records derived from a `[signals]` block. Every
/// signal-sourced record carries at least this, so downstream can distinguish
/// signal-origin from raw-transcript-origin records.
const SIGNAL_SOURCE: &str = "signals";

fn revalidate_signal_kind(declared: IntentKind, payload: &str) -> Option<SignalVerdict> {
    match classify_line_entry_type(payload, false) {
        Some((entry_type, confidence)) if confidence >= CLASSIFIER_ABSTAIN_THRESHOLD => {
            match entry_type_to_timeline_kind(entry_type) {
                // classifier agrees with the section hint -> trusted signal
                Some(kind) if kind == declared => Some(SignalVerdict {
                    kind: declared,
                    source: Some(SIGNAL_SOURCE.to_string()),
                    trusted: true,
                }),
                // classifier disagrees -> classifier wins, record provenance
                Some(kind) => Some(SignalVerdict {
                    kind,
                    source: Some(format!(
                        "{SIGNAL_SOURCE}:revalidated({}->{})",
                        declared.heading().to_ascii_lowercase(),
                        kind.heading().to_ascii_lowercase()
                    )),
                    trusted: false,
                }),
                // classifier confidently reads a non-bucket role -> drop false positive
                None => None,
            }
        }
        // classifier abstains -> honor the section hint
        _ => Some(SignalVerdict {
            kind: declared,
            source: Some(SIGNAL_SOURCE.to_string()),
            trusted: true,
        }),
    }
}

/// The intent signal lane (W2-T13, Decision 9): `Human` + `EchoSeal` are the
/// operator's intention signal. `Inject` never enters distillation, and the
/// `InterAgent` lane never does either — it is not the human and it is not
/// the assistant. Intents own no speech reducer of their own: this spec is
/// asked, role strings are not compared here.
fn intent_signal_spec() -> ProjectionSpec {
    ProjectionSpec {
        roles: vec![ProjectionRole::Human],
        kinds: vec![ProjectionKind::Human, ProjectionKind::EchoSeal],
        // Delayed human speech (echo bus / queue) is still the human speaking.
        dialog: true,
        ..ProjectionSpec::default()
    }
}

/// Does the signal lane emit this transcript role?
fn is_intent_signal_role(spec: &ProjectionSpec, role: &str) -> bool {
    let role_ok =
        projection_role_for_role(role).is_some_and(|projected| spec.emits_role(projected));
    let kind_ok = projection_kind_for_role(role).is_some_and(|kind| spec.emits_kind(kind));
    role_ok && kind_ok
}

fn extract_transcript_candidates(
    file: &StoredChunkFile,
    project: &str,
    source_chunk: &str,
    transcript_entries: &[TranscriptEntry],
) -> (Vec<IntentCandidate>, Vec<TaskEvent>) {
    let mut candidates = Vec::new();
    let mut task_events = Vec::new();
    let signal_spec = intent_signal_spec();

    // Clone identity once, then discard the document. Per-frame cloning of
    // transcript_entries would make a long session quadratic in its bytes.
    let mut frame_file = file.clone();
    frame_file.transcript_entries = None;
    frame_file.body = None;
    for entry in transcript_entries {
        let unknown_frame_time = entry.timestamp.is_none()
            && entry
                .locator
                .as_deref()
                .is_some_and(|locator| !locator.starts_with("transcript-entry:"));
        frame_file.timestamp = if unknown_frame_time {
            DateTime::UNIX_EPOCH
        } else {
            entry.timestamp.unwrap_or(file.timestamp)
        };
        frame_file.date = entry.timestamp.map_or_else(
            || {
                if unknown_frame_time {
                    "unknown".into()
                } else {
                    file.date.clone()
                }
            },
            |time| time.format("%Y-%m-%d").to_string(),
        );
        let file = &frame_file;
        let is_user = is_intent_signal_role(&signal_spec, &entry.role);
        if is_user {
            let message = entry.lines.join("\n");
            if (is_harness_injected_noise(&entry.role, &message)
                && !is_local_command_artifact_line(message.lines().next().unwrap_or_default()))
                || is_reingested_charter_block(&message)
            {
                continue;
            }
        }
        let mut codescribe_parser = CodescribeParser::new();
        // Document-role awareness: a pasted run of commit/changelog lines is
        // historical reference, not operator intent (Round II / oś 2 cut 2).
        let commit_block = commit_block_indices(&entry.lines);

        let mut fenced = false;
        for (index, raw_line) in entry.lines.iter().enumerate() {
            if raw_line.trim_start().starts_with("```") {
                fenced = !fenced;
                continue;
            }
            if fenced || raw_line.trim_start().starts_with('>') {
                continue;
            }
            if commit_block.contains(&index) {
                continue;
            }
            let (cleaned, is_voice) = codescribe_parser.process(raw_line);
            let line = cleaned.trim();
            if line.is_empty() {
                continue;
            }
            if is_source_metadata_line(line) {
                continue;
            }
            if is_local_command_artifact_line(line) {
                continue;
            }
            if is_reingested_charter_line(line) {
                continue;
            }

            let context = surrounding_context(&entry.lines, index);
            let source_provenance = if is_voice {
                Some("voice_transcript".to_string())
            } else {
                None
            };

            if let Some((is_done, task)) = parse_checklist_task(line) {
                if let Some(mut event) = build_task_event(
                    &task,
                    context,
                    file,
                    project,
                    source_chunk,
                    !is_done,
                    false,
                    source_provenance,
                ) {
                    event.candidate.record.provenance =
                        Some(transcript_provenance(entry, index, is_user));
                    if entry.timestamp.is_none() && file.date == "unknown" {
                        event.candidate.record.timestamp = None;
                    }
                    task_events.push(event);
                }
                continue;
            }

            let Some(kind) = infer_kind_from_line(line, is_user) else {
                continue;
            };
            if role_suppresses_outcome_promotion(&entry.role) && kind == IntentKind::Outcome {
                continue;
            }

            if let Some(mut candidate) = build_candidate(
                kind,
                line,
                context,
                file,
                project,
                source_chunk,
                false,
                source_provenance,
            ) {
                candidate.record.provenance = Some(transcript_provenance(entry, index, is_user));
                if entry.timestamp.is_none() && file.date == "unknown" {
                    candidate.record.timestamp = None;
                }
                candidates.push(candidate);
            }
        }
    }

    (candidates, task_events)
}

fn transcript_provenance(entry: &TranscriptEntry, line: usize, is_user: bool) -> IntentProvenance {
    IntentProvenance {
        role: entry.role.clone(),
        locator: format!(
            "{}/message-line:{}",
            entry.locator.as_deref().unwrap_or("unknown-frame"),
            line + 1
        ),
        scope: entry.cwd.clone(),
        timestamp_basis: if entry.timestamp.is_some() {
            "utterance"
        } else if entry
            .locator
            .as_deref()
            .is_some_and(|locator| locator.starts_with("transcript-entry:"))
        {
            "document_date"
        } else {
            "unknown"
        }
        .into(),
        attribution: if is_user {
            "human_candidate"
        } else {
            "agent_claim"
        }
        .into(),
    }
}

fn is_reingested_charter_block(message: &str) -> bool {
    let head = message.trim_start();
    charter_head_markers()
        .iter()
        .any(|marker| starts_with_marker(head, marker))
}

fn role_suppresses_outcome_promotion(role: &str) -> bool {
    let role = role.trim().to_ascii_lowercase();
    role == "agent_reply" || role.starts_with("tool_") || role == "tool"
}

fn is_reingested_charter_line(line: &str) -> bool {
    let trimmed = strip_signal_bullet(line)
        .trim_start_matches('>')
        .trim_start();
    if charter_head_markers()
        .iter()
        .any(|marker| starts_with_marker(trimmed, marker))
    {
        return true;
    }

    let lower = trimmed.to_lowercase();
    [
        "done is a market condition",
        "code is not done because a narrow check turned green",
        "\"done\" means repo health",
        "runtime truth beats theoretical correctness",
        "product truth beats local elegance",
        "loctree gives **sight**",
        "aicx gives **insight**",
        "vibecrafted gives **hands**",
        "move fast, but with taste",
        "be radical when radical is cleaner",
        "finish the whole thing, not just the code",
        "we craft.",
        "we converge.",
        "we ship.",
    ]
    .iter()
    .any(|marker| lower.starts_with(marker))
}

fn starts_with_marker(text: &str, marker: &str) -> bool {
    text.get(..marker.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(marker))
}

fn charter_head_markers() -> &'static [&'static str] {
    &[
        "# AGENTS.md instructions",
        "# CLAUDE.md instructions",
        "<INSTRUCTIONS>",
        "<!-- loctree-doctrine",
        "<!-- loctree-advise",
        "# Vetcoders Global Agent Charter",
        "# Vetcoders Agent Operating Guide",
        "# Loctree + AICX + Vibecrafted Agent Operating Guide",
        "# The Vibecrafted Manifesto",
        "## **LOCTREE + AICX + VIBECRAFTED",
    ]
}

fn infer_kind_from_line(line: &str, is_user_line: bool) -> Option<IntentKind> {
    let modality = if is_user_line {
        intent_line_modality("user", line)
    } else {
        IntentLineModality::Other
    };
    if modality == IntentLineModality::PastedReference {
        return None;
    }

    if is_user_line
        && let Some((entry_type, confidence)) = classify_line_entry_type(line, true)
        && confidence >= CLASSIFIER_ABSTAIN_THRESHOLD
        && let Some(kind) = entry_type_to_timeline_kind(entry_type)
    {
        return Some(kind);
    }

    if is_decision_tag(line) {
        return Some(IntentKind::Decision);
    }
    if is_user_line && looks_like_operator_decision_line(line) {
        return Some(IntentKind::Decision);
    }
    if is_user_line && looks_like_operator_requirement_line(line) {
        return Some(IntentKind::Intent);
    }
    if is_outcome_line(line) {
        return Some(IntentKind::Outcome);
    }
    if modality == IntentLineModality::TypedDirective {
        return Some(IntentKind::Intent);
    }
    if is_user_line && looks_like_intent_line(line) {
        return Some(IntentKind::Intent);
    }
    None
}

fn entry_type_to_timeline_kind(entry_type: EntryType) -> Option<IntentKind> {
    match entry_type {
        EntryType::Decision => Some(IntentKind::Decision),
        EntryType::Task => Some(IntentKind::Task),
        EntryType::Intent | EntryType::Question | EntryType::Why => Some(IntentKind::Intent),
        EntryType::Outcome | EntryType::Result => Some(IntentKind::Outcome),
        EntryType::Argue | EntryType::Assumption | EntryType::Insight | EntryType::Commitment => {
            None
        }
    }
}

fn is_outcome_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    // E.10: bare-affirmation lines ("Zrobione", "Done", "Gotowe") carry no
    // information about WHAT was done; they're emotional ack from the
    // operator, not a reportable outcome. Allow them only with follow-on
    // context (colon + detail).
    if is_bare_affirmation(line) {
        return false;
    }
    is_outcome_tag(line)
        || is_result_line(line)
        || lower.contains("p0=0")
        || lower.contains("p1=0")
        || lower.contains("p2=0")
}

/// Returns true when the entire line is a single affirmation token with no
/// follow-on content. Once a colon + detail appears ("Zrobione: build green"),
/// the line stops being bare and counts again.
fn is_bare_affirmation(line: &str) -> bool {
    let bare = crate::parser::intent_phrases::phrases().bare_affirmation;
    let trimmed = line.trim().trim_end_matches(['.', '!', ',']);
    if trimmed.is_empty() || trimmed.contains(':') {
        return false;
    }
    let stripped = trimmed
        .trim_start_matches(['-', '*', '+', '>', ' ', '\t'])
        .to_lowercase();
    bare.iter().any(|word| stripped == *word)
}

/// Inline backtick code-span ranges within a single line, as byte offsets
/// `(start, end_exclusive)`. Used so keyword classifiers ignore matches that
/// fall inside `` `inline code` `` (e.g. `` `let's encrypt` `` is a tool name,
/// not an intent).
fn code_span_ranges(line: &str) -> Vec<(usize, usize)> {
    let bytes = line.as_bytes();
    let mut ranges = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'`' {
            if let Some(rel) = bytes[i + 1..].iter().position(|&b| b == b'`') {
                let close = i + 1 + rel;
                ranges.push((i, close + 1));
                i = close + 1;
            } else {
                break;
            }
        } else {
            i += 1;
        }
    }
    ranges
}

/// Word boundaries treat alphanumerics (Unicode) and `_` as "word" chars.
/// Diacritics are alphanumeric in Rust so `pomysłu` does NOT word-match
/// keyword `pomysł` — exactly the behavior we want for Polish suffixes.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// `true` if a negator sits immediately around the keyword position — close
/// enough that it inverts the keyword's polarity. Checked on the lower-cased
/// line. Two windows:
/// * pre (~24 chars before): English/Polish negator prefixes that flip the
///   following clause (`don't `, `nie `, `bez `, ...).
/// * post (~16 chars after): post-keyword negators that flip the keyword
///   itself (`let's not`, `chcę nie`, ...).
fn is_negated_keyword(lower_line: &str, kw_pos: usize, kw_len: usize) -> bool {
    let pre_negators = crate::parser::intent_phrases::phrases().negation_pre;
    let post_negators = crate::parser::intent_phrases::phrases().negation_post;

    let pre_window_start = lower_line[..kw_pos]
        .char_indices()
        .rev()
        .take(24)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let pre = &lower_line[pre_window_start..kw_pos];
    if pre_negators.iter().any(|n| pre.ends_with(n)) {
        return true;
    }

    let post_start = kw_pos + kw_len;
    if post_start < lower_line.len() {
        let post_end = lower_line[post_start..]
            .char_indices()
            .take(16)
            .map(|(i, c)| post_start + i + c.len_utf8())
            .last()
            .unwrap_or(lower_line.len())
            .min(lower_line.len());
        let post = &lower_line[post_start..post_end];
        if post_negators.iter().any(|n| post.starts_with(n)) {
            return true;
        }
    }

    false
}

/// Substring match for `keyword` in `line` that:
/// * is case-insensitive,
/// * requires a word boundary on both sides (so `pomysł` does not match
///   `pomysłu`, `let's` does not match `let'salutations`),
/// * rejects matches that fall inside an inline `` ` `` code span,
/// * rejects matches that are immediately negated (`let's not`, `nie chcę`).
fn matches_keyword_word_boundary(line: &str, keyword: &str) -> bool {
    let lower_line = line.to_lowercase();
    let lower_kw = keyword.to_lowercase();
    if lower_kw.is_empty() || lower_line.len() < lower_kw.len() {
        return false;
    }
    let spans = code_span_ranges(&lower_line);

    let mut start = 0;
    while let Some(rel) = lower_line[start..].find(&lower_kw) {
        let abs = start + rel;
        let end = abs + lower_kw.len();

        let prev_ok = if abs == 0 {
            true
        } else {
            let prev = lower_line[..abs].chars().next_back().unwrap();
            !is_word_char(prev) && prev != '-'
        };
        let next_ok = if end >= lower_line.len() {
            true
        } else {
            let next = lower_line[end..].chars().next().unwrap();
            !is_word_char(next)
        };

        if prev_ok && next_ok {
            let in_span = spans.iter().any(|&(s, e)| abs >= s && abs < e);
            if !in_span && !is_negated_keyword(&lower_line, abs, lower_kw.len()) {
                return true;
            }
        }

        start = abs + 1;
    }
    false
}

fn looks_like_intent_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    if lower.starts_with("intent:") || lower.starts_with("[intent]") {
        return true;
    }
    if severity_marker(line).is_some() {
        return true;
    }
    intent_keywords()
        .iter()
        .any(|kw| matches_keyword_word_boundary(line, kw))
}

fn severity_marker(line: &str) -> Option<&'static str> {
    let upper = line.to_ascii_uppercase();
    let has_marker = |marker: &str| {
        upper
            .split(|ch: char| !ch.is_ascii_alphanumeric())
            .any(|token| token == marker)
    };
    ["P0", "P1", "P2"]
        .into_iter()
        .find(|marker| has_marker(marker))
}

fn is_source_metadata_line(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    [
        "source:",
        "kind:",
        "source_file:",
        "severity:",
        "project:",
        "author:",
        "heading:",
        "input:",
        "output:",
        "\"output\":",
        "current topic:",
        "topic summary:",
        "successfully created and wrote to new file:",
        "base directory for this skill:",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn looks_like_operator_decision_line(line: &str) -> bool {
    let lower = line.to_lowercase();

    crate::parser::intent_phrases::phrases()
        .decision_policy
        .iter()
        .any(|marker| lower.contains(marker))
}

/// `true` when a line is a code/log fragment rather than prose — a bare
/// identifier (`DEFAULT_KEYWORDS_PATH`), an assignment to one
/// (`DEFAULT_KEYWORDS_PATH = "..."`), or a kwarg call
/// (`field(default_factory=list)`). These leak into the classifier through
/// substring policy markers ("default", "canonical") and must not become
/// intents/decisions/outcomes (Round II / oś 2).
fn is_code_fragment_line(line: &str) -> bool {
    let s = line.trim().trim_start_matches(['-', '*', '+']).trim();
    if s.is_empty() {
        return false;
    }

    // (a) keyword-arg / assignment inside a call: foo(bar=baz)
    if has_kwarg_call(s) {
        return true;
    }

    // (b) the line is essentially a CONSTANT_CASE identifier (optionally
    // assigned), with at most one trailing prose word.
    let mut has_constant_case = false;
    let mut prose_words = 0usize;
    for token in s.split_whitespace() {
        let core = token.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
        if core.is_empty() {
            continue; // pure punctuation (=, (), "", ...)
        }
        if is_constant_case_identifier(core) {
            has_constant_case = true;
            continue;
        }
        if core.chars().all(|c| c.is_ascii_digit()) {
            continue; // numbers / counts
        }
        if core.chars().all(|c| c.is_alphabetic()) && core.chars().any(|c| c.is_lowercase()) {
            prose_words += 1; // a natural lowercase word is prose
        }
    }
    has_constant_case && prose_words <= 1
}

/// `FOO_BAR`, `DEFAULT_KEYWORDS_PATH` — all-caps/digits with at least one
/// underscore-joined segment. Plain `OK`/`PASS` (no underscore) are not matched.
fn is_constant_case_identifier(token: &str) -> bool {
    if !token.contains('_') {
        return false;
    }
    let mut has_alpha = false;
    for c in token.chars() {
        if c.is_ascii_uppercase() {
            has_alpha = true;
        } else if c.is_ascii_digit() || c == '_' {
            // allowed
        } else {
            return false; // lowercase or other -> not constant case
        }
    }
    has_alpha
}

/// Detects `ident(... = ...)` — a bare `=` (not `==`/`<=`/`>=`/`!=`) between the
/// first `(` and the next `)`, i.e. a keyword argument / call assignment.
fn has_kwarg_call(s: &str) -> bool {
    let Some(open) = s.find('(') else {
        return false;
    };
    let rest = &s[open + 1..];
    let Some(close) = rest.find(')') else {
        return false;
    };
    let bytes = &rest.as_bytes()[..close];
    for (i, &b) in bytes.iter().enumerate() {
        if b == b'=' {
            let prev = if i > 0 { bytes[i - 1] } else { b' ' };
            let next = if i + 1 < bytes.len() {
                bytes[i + 1]
            } else {
                b' '
            };
            if prev != b'=' && prev != b'<' && prev != b'>' && prev != b'!' && next != b'=' {
                return true;
            }
        }
    }
    false
}

/// Capitalized English commit-subject verbs (git/changelog/conventional-commit
/// imperative mood). Case-sensitive: real commit subjects are Capitalized, so
/// lowercase prose ("add a retry") is not matched here.
const COMMIT_SUBJECT_VERBS: &[&str] = &[
    "Add",
    "Fix",
    "Update",
    "Remove",
    "Refactor",
    "Implement",
    "Create",
    "Delete",
    "Move",
    "Rename",
    "Drop",
    "Bump",
    "Split",
    "Harden",
    "Allow",
    "Stabilize",
    "Port",
    "Merge",
    "Revert",
    "Close",
    "Resolve",
    "Improve",
    "Switch",
    "Enable",
    "Disable",
    "Replace",
    "Extract",
    "Introduce",
    "Wire",
    "Guard",
    "Expose",
    "Prevent",
    "Support",
];

const CONVENTIONAL_COMMIT_TYPES: &[&str] = &[
    "feat", "fix", "chore", "docs", "refactor", "test", "perf", "build", "ci", "style", "revert",
    "ops", "polish", "release",
];

/// `type: subject` or `type(scope): subject` conventional-commit prefix.
fn is_conventional_commit_prefix(s: &str) -> bool {
    let Some((head, rest)) = s.split_once(": ") else {
        return false;
    };
    if rest.trim().is_empty() {
        return false;
    }
    let type_word = head.split_once('(').map(|(t, _)| t).unwrap_or(head);
    // scope form must actually close its paren
    if head.contains('(') && !head.ends_with(')') {
        return false;
    }
    CONVENTIONAL_COMMIT_TYPES.contains(&type_word)
}

/// `true` when a line reads like a git-log / changelog entry: a leading commit
/// hash, a conventional-commit prefix, or a capitalized commit-subject verb.
/// Used only as a per-line signal; a single such line is NOT enough to suppress
/// it (see [`commit_block_indices`]).
fn looks_like_commit_log_line(line: &str) -> bool {
    let s = line.trim().trim_start_matches(['-', '*', '+', '>']).trim();
    if s.is_empty() {
        return false;
    }
    if let Some((first, rest)) = s.split_once(char::is_whitespace)
        && !rest.trim().is_empty()
        && looks_like_commit_hash(first)
    {
        return true;
    }
    if is_conventional_commit_prefix(s) {
        return true;
    }
    if let Some((first, rest)) = s.split_once(char::is_whitespace)
        && !rest.trim().is_empty()
        && COMMIT_SUBJECT_VERBS.contains(&first)
    {
        return true;
    }
    false
}

/// Document-role awareness (Round II / oś 2): indices of lines that belong to a
/// pasted commit-list / changelog BLOCK — a run of >=2 consecutive
/// commit-log-like lines. A lone imperative is left alone (it may be a real
/// task); only a run is treated as historical reference.
fn commit_block_indices(lines: &[String]) -> HashSet<usize> {
    let flags: Vec<bool> = lines
        .iter()
        .map(|l| looks_like_commit_log_line(l))
        .collect();
    let mut block = HashSet::new();
    let mut i = 0;
    while i < flags.len() {
        if flags[i] {
            let start = i;
            while i < flags.len() && flags[i] {
                i += 1;
            }
            if i - start >= 2 {
                block.extend(start..i);
            }
        } else {
            i += 1;
        }
    }
    block
}

fn looks_like_operator_requirement_line(line: &str) -> bool {
    let lower = line.to_lowercase();

    crate::parser::intent_phrases::phrases()
        .requirement
        .iter()
        .any(|marker| lower.contains(marker))
}

fn is_garbled_transcription(summary: &str) -> bool {
    let lower = summary.to_lowercase();
    if lower.contains("arozet") || lower.contains("injust") {
        return true;
    }
    let fillers = &["yym", "ehem", "ten tego", "yyy", "eee", "hmmm"];
    for &f in fillers {
        if lower.contains(f) {
            return true;
        }
    }
    let word_count = summary.split_whitespace().count();
    let has_structure = summary.contains(',')
        || summary.contains('.')
        || summary.contains(';')
        || summary.contains('?')
        || summary.contains('!');
    if word_count > 15 && !has_structure {
        return true;
    }
    false
}

fn calculate_confidence(
    kind: IntentKind,
    summary: &str,
    has_context: bool,
    has_evidence: bool,
    is_signal: bool,
) -> u8 {
    let mut confidence = if is_signal {
        4
    } else {
        match kind {
            IntentKind::Intent => 2,
            _ => 3,
        }
    };

    if has_context {
        confidence += 1;
    }
    if has_evidence {
        confidence += 1;
    }
    if kind == IntentKind::Intent && severity_marker(summary).is_some() {
        confidence += 1;
    }

    confidence.min(5)
}

#[allow(clippy::too_many_arguments)]
fn build_candidate(
    kind: IntentKind,
    raw_summary: &str,
    context: Option<String>,
    file: &StoredChunkFile,
    project: &str,
    source_chunk: &str,
    is_signal: bool,
    source_provenance: Option<String>,
) -> Option<IntentCandidate> {
    let summary = normalize_display_text(&clean_summary(kind, raw_summary));
    if summary.is_empty() || is_metadata_only_summary(&summary) {
        return None;
    }

    let context = context
        .map(|value| normalize_display_text(&value))
        .filter(|value| !value.is_empty() && normalize_key(value) != normalize_key(&summary))
        .filter(|value| !is_section_heading_noise(value))
        .map(|value| truncate_signal_line(&value));

    let mut evidence = extract_evidence(&summary);
    if let Some(extra) = context.as_deref() {
        merge_evidence(&mut evidence, extract_evidence(extra));
    }
    // W2-R1: a record distilled from a mixed-workstream candidate says so
    // — the reader must not take it for one homogeneous history.
    if let Some(scope) = file.scope.as_ref()
        && scope.status == aicx_parser::engine::ScopeStatus::MixedCandidate
    {
        evidence.push(format!(
            "scope_status=mixed_candidate cwds={} branches={}",
            scope.cwds.join(","),
            scope.branches.join(",")
        ));
    }

    // Anti-bełkot sanity gate:
    if kind == IntentKind::Intent
        && context.is_none()
        && evidence.is_empty()
        && is_garbled_transcription(&summary)
    {
        return None; // Degrade to candidate (exclude from final intents)
    }

    let confidence = calculate_confidence(
        kind,
        &summary,
        context.is_some(),
        !evidence.is_empty(),
        is_signal,
    );

    Some(IntentCandidate {
        record: IntentRecord {
            provenance: None,
            kind,
            summary: truncate_summary_for_display(&summary),
            context,
            evidence,
            project: project.to_string(),
            agent: file.agent.clone(),
            date: file.date.clone(),
            session_id: file.session_id.clone(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: source_chunk.to_string(),
            timestamp: Some(file.timestamp.to_rfc3339()),
            source: source_provenance,
            honesty: file.honesty.clone(),
        },
        confidence,
        timestamp: file.timestamp,
    })
}

#[allow(clippy::too_many_arguments)]
fn build_task_event(
    task: &str,
    context: Option<String>,
    file: &StoredChunkFile,
    project: &str,
    source_chunk: &str,
    is_open: bool,
    is_signal: bool,
    source_provenance: Option<String>,
) -> Option<TaskEvent> {
    let candidate = build_candidate(
        IntentKind::Task,
        task,
        context,
        file,
        project,
        source_chunk,
        is_signal,
        source_provenance,
    )?;

    Some(TaskEvent {
        key: normalize_key(&candidate.record.summary),
        candidate,
        is_open,
    })
}

fn clean_summary(kind: IntentKind, raw: &str) -> String {
    let mut text = strip_signal_bullet(raw).trim();

    match kind {
        IntentKind::Decision => {
            text = strip_case_insensitive_prefix(text, "[decision]");
            text = strip_case_insensitive_prefix(text, "decision:");
        }
        IntentKind::Outcome => {
            text = strip_case_insensitive_prefix(text, "[skill_outcome]");
            text = strip_case_insensitive_prefix(text, "outcome:");
            text = strip_case_insensitive_prefix(text, "validation:");
        }
        IntentKind::Intent => {
            text = strip_case_insensitive_prefix(text, "[intent]");
            text = strip_case_insensitive_prefix(text, "intent:");
            text = strip_case_insensitive_prefix(text, "question:");
            text = strip_case_insensitive_prefix(text, "why:");
        }
        IntentKind::Task => {}
    }

    normalize_display_text(text)
}

fn strip_signal_bullet(line: &str) -> &str {
    line.trim().strip_prefix("- ").unwrap_or(line.trim())
}

fn strip_case_insensitive_prefix<'a>(text: &'a str, prefix: &str) -> &'a str {
    if text.len() < prefix.len() {
        return text;
    }

    let Some(candidate) = text.get(..prefix.len()) else {
        return text;
    };

    if candidate.eq_ignore_ascii_case(prefix) {
        text.get(prefix.len()..)
            .unwrap_or("")
            .trim_start_matches([' ', '-', ':'])
            .trim_start()
    } else {
        text
    }
}

fn normalize_display_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn surrounding_context(lines: &[String], index: usize) -> Option<String> {
    let mut parts = Vec::new();
    let mut push_part = |line: &str| {
        let line = line.trim();
        if is_source_metadata_line(line) || is_local_command_artifact_line(line) {
            return;
        }
        let part = normalize_display_text(line);
        if !part.is_empty() {
            parts.push(part);
        }
    };

    if let Some(prev) = index.checked_sub(1).and_then(|idx| lines.get(idx)) {
        push_part(prev);
    }

    if let Some(next) = lines.get(index + 1) {
        push_part(next);
    }

    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" | "))
    }
}

fn extract_evidence(text: &str) -> Vec<String> {
    let mut evidence = Vec::new();

    for token in text.split_whitespace() {
        let cleaned = token.trim_matches(|ch: char| {
            matches!(
                ch,
                ',' | '.' | ';' | ':' | '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\''
            )
        });
        if cleaned.is_empty() {
            continue;
        }

        if looks_like_file_ref(cleaned)
            || looks_like_commit_hash(cleaned)
            || looks_like_score(cleaned)
        {
            push_unique(&mut evidence, cleaned.to_string());
        }
    }

    evidence
}

fn looks_like_file_ref(token: &str) -> bool {
    let lower = token.to_lowercase();
    const EXTENSIONS: &[&str] = &[
        ".rs", ".md", ".json", ".jsonl", ".toml", ".yaml", ".yml", ".ts", ".tsx", ".js", ".jsx",
        ".py", ".sh", ".txt",
    ];

    EXTENSIONS.iter().any(|ext| {
        lower.contains(ext)
            && (token.contains('/')
                || token.contains('\\')
                || token.contains(':')
                || token.starts_with("src."))
    })
}

fn looks_like_commit_hash(token: &str) -> bool {
    (7..=40).contains(&token.len()) && token.chars().all(|ch| ch.is_ascii_hexdigit())
}

fn looks_like_score(token: &str) -> bool {
    let lower = token.to_lowercase();
    lower == "p0=0"
        || lower == "p1=0"
        || lower == "p2=0"
        || lower.ends_with("/10")
        || lower.starts_with("score=")
        || lower.starts_with("score:")
}

/// Drops candidates whose entire summary is a numeric list-item heading like
/// "1 Wierność źródłu" or "4 Deterministyczność transformacji". These slip
/// through `looks_like_operator_decision_line` because numbered headings
/// inside long Polish reflective passages look like "musi"-bearing imperatives
/// to the heuristic, but carry no decision content on their own.
fn is_metadata_only_summary(text: &str) -> bool {
    let trimmed = text.trim();
    let residue = trimmed
        .trim_matches(|c: char| c.is_whitespace() || matches!(c, '.' | '`' | '-' | '_' | '*'));
    if residue.is_empty() {
        return true;
    }

    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() {
        return true;
    }
    if words.len() <= 4
        && words[0]
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ')')
        && !words[0].is_empty()
        && words[0].chars().any(|c| c.is_ascii_digit())
    {
        return true;
    }
    if trimmed.ends_with(':') && words.len() <= 4 && !trimmed.contains("://") {
        return true;
    }
    if is_numbered_reference_item(trimmed) {
        return true;
    }
    false
}

fn is_numbered_reference_item(text: &str) -> bool {
    let Some(first) = text.split_whitespace().next() else {
        return false;
    };
    let marker = first.trim_end_matches(['.', ')']);
    if marker.is_empty() || !marker.chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let lower = text.to_ascii_lowercase();
    text.contains('`')
        || lower.contains(" when the ")
        || lower.contains("must ")
        || lower.contains("should ")
}

/// Detects the "1 Foo | 2 Bar" pattern that appears as `context` when the
/// surrounding-context window (lines ±1) lands on numbered section headings
/// inside a long paragraph. Such context offers no reasoning value and only
/// adds noise to the human-readable output.
fn is_section_heading_noise(text: &str) -> bool {
    let parts: Vec<&str> = text.split('|').map(str::trim).collect();
    if parts.is_empty() || parts.iter().any(|p| p.is_empty()) {
        return false;
    }
    parts.iter().all(|part| {
        let words: Vec<&str> = part.split_whitespace().collect();
        if words.is_empty() || words.len() > 4 {
            return false;
        }
        words[0]
            .chars()
            .all(|c| c.is_ascii_digit() || c == '.' || c == ')')
            && words[0].chars().any(|c| c.is_ascii_digit())
    })
}

/// Sentence-aware truncation for human-readable summaries. The chunker's
/// generic `truncate_signal_line` cuts mid-word at 240 bytes and appends
/// "...[truncated]"; for an intent summary that destroys readability and
/// often discards the verb that carries the decision. We allow up to 480
/// bytes and prefer the last sentence terminator (or comma/space) within
/// the trailing window so the output ends on a natural break.
fn truncate_summary_for_display(text: &str) -> String {
    const MAX_BYTES: usize = 480;
    const TAIL_LOOKBACK: usize = 80;

    if text.len() <= MAX_BYTES {
        return text.to_string();
    }

    let mut cutoff = MAX_BYTES;
    while cutoff > 0 && !text.is_char_boundary(cutoff) {
        cutoff -= 1;
    }

    let look_start = cutoff.saturating_sub(TAIL_LOOKBACK);
    let look_start = (0..=look_start)
        .rev()
        .find(|i| text.is_char_boundary(*i))
        .unwrap_or(0);
    let tail = &text[look_start..cutoff];

    if let Some(rel) = tail.rfind(['.', '!', '?']) {
        let abs_start = look_start + rel;
        if let Some(ch) = text[abs_start..].chars().next() {
            let abs_end = abs_start + ch.len_utf8();
            return text[..abs_end].trim_end().to_string();
        }
    }

    if let Some(rel) = tail.rfind([',', ';', ':']) {
        let abs = look_start + rel;
        let mut out = text[..abs].trim_end().to_string();
        out.push_str(" …");
        return out;
    }

    if let Some(rel) = tail.rfind(char::is_whitespace) {
        let abs = look_start + rel;
        let mut out = text[..abs].trim_end().to_string();
        out.push_str(" …");
        return out;
    }

    let mut out = text[..cutoff].to_string();
    out.push_str(" …");
    out
}

/// Walks the dedup output and replaces `session_id` with the value parsed from
/// the source_chunk filename when the two disagree. Filenames are produced by
/// `legacy_archive::session_basename` and treated as ground truth — that file actually
/// exists and was read. A mismatched `session_id` claim is a provenance lie
/// (it tells the operator "this is from session X" while citing a file that
/// belongs to session Y).
fn reconcile_session_id_with_path(records: &mut [IntentRecord]) {
    for record in records.iter_mut() {
        let path = std::path::Path::new(&record.source_chunk);
        let Some(stem) = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .map(str::to_string)
        else {
            continue;
        };
        // basename layout: <YYYY_MMDD>_<agent>_<session-id>_<chunk>
        // Strip trailing _NNN chunk suffix, then strip the leading
        // <date>_<agent>_ prefix to recover the truncated session_id.
        let Some((without_chunk, chunk_part)) = stem.rsplit_once('_') else {
            continue;
        };
        if chunk_part.len() != 3 || !chunk_part.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        // Skip <date> tokens (YYYY_MMDD), find first non-numeric segment as agent
        let segments: Vec<&str> = without_chunk.split('_').collect();
        if segments.len() < 4 {
            continue;
        }
        // segments[0]=YYYY, [1]=MMDD, [2]=agent, [3..]=session_id pieces
        let session_id_from_path = segments[3..].join("_");
        if session_id_from_path.is_empty() {
            continue;
        }
        if record.session_id != session_id_from_path {
            record.session_id = session_id_from_path;
        }
    }
}

/// Drop truncated-prefix duplicates: records whose summary ends in
/// `...[truncated]` AND whose pre-truncation prefix is also the literal prefix
/// of a longer non-truncated sibling in the same `(kind, session_id,
/// source_chunk)` group.
///
/// Indexed: O(N) build of a per-group index of non-truncated record indices,
/// then O(N) decision pass that only scans the small same-group set. The
/// previous shape was O(N²) — a 10k record session ran 100M comparisons.
/// Real groups stay small (one chunk holds at most a handful of records of a
/// single kind), so the inner scan is effectively constant.
fn drop_truncated_duplicate_records(records: &mut Vec<IntentRecord>) {
    const TRUNC_MARKER: &str = "...[truncated]";

    type GroupKey = (IntentKind, String, String);

    // Pass 1: bucket non-truncated record indices by (kind, session, chunk).
    // Truncated records cannot be "the fuller version" of another, so they
    // never need to live in the index.
    let mut groups: HashMap<GroupKey, Vec<usize>> = HashMap::new();
    for (idx, record) in records.iter().enumerate() {
        if record.summary.contains(TRUNC_MARKER) {
            continue;
        }
        groups
            .entry((
                record.kind,
                record.session_id.clone(),
                record.source_chunk.clone(),
            ))
            .or_default()
            .push(idx);
    }

    // Pass 2: each truncated record looks up its (kind, session, chunk)
    // bucket once and scans the small list of non-truncated siblings.
    let keep: Vec<bool> = records
        .iter()
        .enumerate()
        .map(|(idx, record)| {
            if !record.summary.contains(TRUNC_MARKER) {
                return true;
            }
            let Some(raw_prefix) = record.summary.split(TRUNC_MARKER).next() else {
                return true;
            };
            let prefix = raw_prefix.trim_end();
            if prefix.is_empty() {
                return true;
            }
            let key = (
                record.kind,
                record.session_id.clone(),
                record.source_chunk.clone(),
            );
            let Some(siblings) = groups.get(&key) else {
                return true;
            };
            let has_fuller = siblings.iter().any(|&other_idx| {
                if other_idx == idx {
                    return false;
                }
                let other = &records[other_idx];
                other.summary.len() > record.summary.len() && other.summary.starts_with(prefix)
            });
            !has_fuller
        })
        .collect();

    let mut index = 0;
    records.retain(|_| {
        let should_keep = keep[index];
        index += 1;
        should_keep
    });
}

fn dedup_candidates(
    candidates: Vec<IntentCandidate>,
    strict: bool,
    min_confidence: Option<u8>,
    kind_filter: Option<IntentKind>,
) -> Vec<IntentRecord> {
    let mut map: HashMap<(IntentKind, String, String), CandidateAccumulator> = HashMap::new();

    let target_confidence = if let Some(mc) = min_confidence {
        mc
    } else if strict {
        4
    } else {
        1
    };

    for candidate in candidates {
        if kind_filter.is_some() && kind_filter != Some(candidate.record.kind) {
            continue;
        }
        if candidate.confidence < target_confidence {
            continue;
        }

        // Same normalized fact, same project, same kind: surface once. The
        // merge path below carries the winning provenance together, so the
        // selected source_chunk and session_id stay consistent.
        let key = (
            candidate.record.kind,
            candidate.record.project.clone(),
            normalize_key(&candidate.record.summary),
        );

        if let Some(existing) = map.get_mut(&key) {
            merge_candidate(existing, candidate);
        } else {
            map.insert(key, CandidateAccumulator { candidate });
        }
    }

    let mut values: Vec<CandidateAccumulator> = map.into_values().collect();
    values.sort_by(|left, right| {
        right
            .candidate
            .timestamp
            .cmp(&left.candidate.timestamp)
            .then_with(|| {
                left.candidate
                    .record
                    .kind
                    .sort_rank()
                    .cmp(&right.candidate.record.kind.sort_rank())
            })
            .then_with(|| {
                right
                    .candidate
                    .record
                    .source_chunk
                    .cmp(&left.candidate.record.source_chunk)
            })
    });

    values
        .into_iter()
        .map(|item| item.candidate.record)
        .collect()
}

fn finalize_tasks(
    task_events: Vec<TaskEvent>,
    strict: bool,
    min_confidence: Option<u8>,
    kind_filter: Option<IntentKind>,
) -> Vec<IntentRecord> {
    if kind_filter.is_some() && kind_filter != Some(IntentKind::Task) {
        return Vec::new();
    }

    let mut map: HashMap<String, TaskAccumulator> = HashMap::new();
    let mut events = task_events;
    events.sort_by(|left, right| {
        left.candidate
            .timestamp
            .cmp(&right.candidate.timestamp)
            .then_with(|| {
                left.candidate
                    .record
                    .source_chunk
                    .cmp(&right.candidate.record.source_chunk)
            })
    });

    let target_confidence = if let Some(mc) = min_confidence {
        mc
    } else if strict {
        4
    } else {
        1
    };

    for event in events {
        if event.candidate.confidence < target_confidence {
            continue;
        }

        if let Some(existing) = map.get_mut(&event.key) {
            merge_task(existing, event);
        } else {
            map.insert(
                event.key,
                TaskAccumulator {
                    candidate: event.candidate,
                    is_open: event.is_open,
                },
            );
        }
    }

    let mut tasks: Vec<TaskAccumulator> = map.into_values().filter(|acc| acc.is_open).collect();

    tasks.sort_by(|left, right| {
        right
            .candidate
            .timestamp
            .cmp(&left.candidate.timestamp)
            .then_with(|| {
                right
                    .candidate
                    .record
                    .source_chunk
                    .cmp(&left.candidate.record.source_chunk)
            })
    });

    tasks
        .into_iter()
        .map(|task| task.candidate.record)
        .collect()
}

fn merge_candidate(existing: &mut CandidateAccumulator, incoming: IntentCandidate) {
    merge_evidence(
        &mut existing.candidate.record.evidence,
        incoming.record.evidence.clone(),
    );

    if should_replace_context(
        existing.candidate.record.context.as_deref(),
        incoming.record.context.as_deref(),
    ) {
        existing.candidate.record.context = incoming.record.context.clone();
    }

    existing.candidate.record.summary =
        prefer_summary(&existing.candidate.record.summary, &incoming.record.summary);
    existing.candidate.confidence = existing.candidate.confidence.max(incoming.confidence);

    let should_replace_record = incoming.timestamp > existing.candidate.timestamp
        || (incoming.timestamp == existing.candidate.timestamp
            && incoming.confidence >= existing.candidate.confidence);

    if should_replace_record {
        existing.candidate.timestamp = incoming.timestamp;
        existing.candidate.record.project = incoming.record.project.clone();
        existing.candidate.record.agent = incoming.record.agent.clone();
        existing.candidate.record.date = incoming.record.date.clone();
        existing.candidate.record.session_id = incoming.record.session_id.clone();
        existing.candidate.record.source_chunk = incoming.record.source_chunk.clone();
        existing.candidate.record.timestamp = incoming.record.timestamp.clone();
        existing.candidate.record.provenance = incoming.record.provenance.clone();
        existing.candidate.record.source = incoming.record.source.clone();
        existing.candidate.record.honesty = incoming.record.honesty.clone();
    }
}

fn merge_task(existing: &mut TaskAccumulator, incoming: TaskEvent) {
    merge_evidence(
        &mut existing.candidate.record.evidence,
        incoming.candidate.record.evidence.clone(),
    );

    if should_replace_context(
        existing.candidate.record.context.as_deref(),
        incoming.candidate.record.context.as_deref(),
    ) {
        existing.candidate.record.context = incoming.candidate.record.context.clone();
    }

    existing.candidate.record.summary = prefer_summary(
        &existing.candidate.record.summary,
        &incoming.candidate.record.summary,
    );
    existing.candidate.confidence = existing
        .candidate
        .confidence
        .max(incoming.candidate.confidence);

    let should_replace_record = incoming.candidate.timestamp > existing.candidate.timestamp
        || (incoming.candidate.timestamp == existing.candidate.timestamp
            && incoming.candidate.confidence >= existing.candidate.confidence);

    if should_replace_record {
        existing.candidate.timestamp = incoming.candidate.timestamp;
        existing.candidate.record.project = incoming.candidate.record.project.clone();
        existing.candidate.record.agent = incoming.candidate.record.agent.clone();
        existing.candidate.record.date = incoming.candidate.record.date.clone();
        existing.candidate.record.source_chunk = incoming.candidate.record.source_chunk.clone();
        existing.candidate.record.timestamp = incoming.candidate.record.timestamp.clone();
        existing.candidate.record.session_id = incoming.candidate.record.session_id.clone();
        existing.candidate.record.provenance = incoming.candidate.record.provenance.clone();
        existing.candidate.record.source = incoming.candidate.record.source.clone();
        existing.candidate.record.honesty = incoming.candidate.record.honesty.clone();
        existing.is_open = incoming.is_open;
    }
}

fn should_replace_context(existing: Option<&str>, incoming: Option<&str>) -> bool {
    let existing_len = existing.map(str::len).unwrap_or(0);
    let incoming_len = incoming.map(str::len).unwrap_or(0);
    incoming_len > existing_len
}

fn prefer_summary(existing: &str, incoming: &str) -> String {
    if incoming.len() > existing.len() {
        incoming.to_string()
    } else {
        existing.to_string()
    }
}

fn merge_evidence(existing: &mut Vec<String>, additions: Vec<String>) {
    // E.4: build the seen-set once per merge instead of rebuilding it on
    // every `push_unique` call. The previous shape rebuilt the HashSet per
    // insert, making evidence appends O(N^2) over long accumulators.
    let mut seen: HashSet<String> = existing.iter().map(|item| normalize_key(item)).collect();
    for item in additions {
        let key = normalize_key(&item);
        if seen.insert(key) {
            existing.push(item);
        }
    }
}

fn push_unique(target: &mut Vec<String>, value: String) {
    let key = normalize_key(&value);
    if target.iter().any(|item| normalize_key(item) == key) {
        return;
    }
    target.push(value);
}

// ── 11-type intent entry classifier ─────────────────────────────────

const CLASSIFIER_ABSTAIN_THRESHOLD: f32 = 0.5;

/// A line "has result shape" when it carries a concrete reporting signal:
/// a digit (test count, error count, percentage), a PASS/FAIL token, or a
/// known status word. Without one, soft markers like "tests" or "error:" are
/// almost certainly meta-discussion, not actual outcomes.
fn line_has_result_shape(lower_line: &str) -> bool {
    if lower_line.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    crate::parser::intent_phrases::phrases()
        .result_shape
        .iter()
        .any(|t| lower_line.contains(t))
}

fn looks_like_task_directive_line(line: &str) -> bool {
    let head = line.trim_start();
    crate::parser::intent_phrases::phrases()
        .task_directive
        .iter()
        .any(|marker| {
            head.get(..marker.len())
                .is_some_and(|prefix| prefix.eq_ignore_ascii_case(marker))
        })
}

fn looks_like_bare_checkbox_task(line: &str) -> bool {
    let head = line.trim_start();
    head.starts_with("[ ] ") || head.starts_with("[x] ") || head.starts_with("[X] ")
}

fn looks_like_actionable_task_line(line: &str) -> bool {
    let head = line
        .trim_start()
        .trim_start_matches(['-', '*', '+'])
        .trim_start();
    let word_count = head.split_whitespace().take(4).count();
    if word_count < 3 {
        return false;
    }

    let lower = head.to_lowercase();
    crate::parser::intent_phrases::phrases()
        .task_action_heads
        .iter()
        .any(|marker| {
            lower == *marker
                || lower
                    .strip_prefix(marker)
                    .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with(':'))
        })
}

fn looks_like_completion_outcome_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    if !crate::parser::intent_phrases::phrases()
        .completion
        .iter()
        .any(|marker| lower.contains(marker))
    {
        return false;
    }

    lower.starts_with("plik ")
        || lower.starts_with("file ")
        || lower.starts_with("docs/")
        || lower.contains(".md")
        || lower.contains(".rs")
        || lower.contains('/')
}

fn looks_like_observed_count_outcome_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    if !lower.chars().any(|c| c.is_ascii_digit()) {
        return false;
    }
    let has_observed_counts =
        lower.contains(" records") || lower.contains(" rekord") || lower.contains(" wynik");
    let has_result_verb = lower.contains(" dal ")
        || lower.contains(" dał ")
        || lower.contains("dala ")
        || lower.contains(" dała ")
        || lower.contains("yielded")
        || lower.contains("produced")
        || lower.contains("gave");

    has_observed_counts && has_result_verb
}

fn looks_like_commitment_line(line: &str) -> bool {
    let head = line.trim_start().to_lowercase();
    if head.starts_with("commitment:")
        || head.starts_with("promise:")
        || head.starts_with("obietnica:")
    {
        return true;
    }

    crate::parser::intent_phrases::phrases()
        .commitment_heads
        .iter()
        .any(|marker| head.starts_with(marker))
}

pub fn classify_line_entry_type(line: &str, is_user: bool) -> Option<(EntryType, f32)> {
    let lower = line.to_lowercase();
    let trimmed = lower.trim();
    let modality = if is_user {
        intent_line_modality("user", line)
    } else {
        IntentLineModality::Other
    };

    if modality == IntentLineModality::PastedReference {
        return None;
    }

    if trimmed.starts_with("decision:") || trimmed.contains("[decision]") {
        return Some((EntryType::Decision, 0.95));
    }
    if looks_like_task_directive_line(line) {
        return Some((EntryType::Task, 0.95));
    }
    if is_user && looks_like_bare_checkbox_task(line) {
        return Some((EntryType::Task, 0.85));
    }
    if is_user && looks_like_actionable_task_line(line) {
        return Some((EntryType::Task, 0.75));
    }
    if looks_like_commitment_line(line) {
        return Some((EntryType::Commitment, 0.72));
    }

    if trimmed.starts_with("question:") || trimmed.ends_with('?') && trimmed.len() > 15 {
        let conf = if trimmed.starts_with("question:") {
            0.95
        } else {
            0.7
        };
        if crate::parser::intent_phrases::phrases()
            .question
            .iter()
            .any(|m| lower.contains(m))
            || trimmed.ends_with('?')
        {
            return Some((EntryType::Question, conf));
        }
    }

    if is_user && looks_like_operator_decision_line(line) {
        return Some((EntryType::Decision, 0.75));
    }
    if is_user && looks_like_operator_requirement_line(line) {
        return Some((EntryType::Intent, 0.7));
    }

    if trimmed.starts_with("assumption:")
        || trimmed.starts_with("hypothesis:")
        || trimmed.starts_with("zakładam")
        || trimmed.starts_with("zakladam")
        || trimmed.starts_with("założenie:")
        || trimmed.starts_with("zalozenie:")
        || trimmed.starts_with("hipoteza:")
    {
        return Some((EntryType::Assumption, 0.9));
    }
    if crate::parser::intent_phrases::phrases()
        .assumption
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some((EntryType::Assumption, 0.65));
    }

    if trimmed.starts_with("insight:")
        || trimmed.starts_with("odkrycie:")
        || trimmed.starts_with("wniosek:")
        || trimmed.starts_with("kluczowe:")
        || trimmed.contains("★ insight")
    {
        return Some((EntryType::Insight, 0.9));
    }
    if crate::parser::intent_phrases::phrases()
        .insight
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some((EntryType::Insight, 0.65));
    }

    if is_outcome_tag(line) || trimmed.starts_with("[skill_outcome]") {
        return Some((EntryType::Outcome, 0.9));
    }
    if looks_like_completion_outcome_line(line) {
        return Some((EntryType::Outcome, 0.72));
    }
    if looks_like_observed_count_outcome_line(line) {
        return Some((EntryType::Outcome, 0.72));
    }

    if trimmed.starts_with("result:") || trimmed.starts_with("wynik:") {
        return Some((EntryType::Result, 0.95));
    }
    if is_result_line(line)
        || crate::parser::intent_phrases::phrases()
            .result_strict
            .iter()
            .any(|m| lower.contains(m))
    {
        return Some((EntryType::Result, 0.75));
    }
    if crate::parser::intent_phrases::phrases()
        .result_soft
        .iter()
        .any(|m| lower.contains(m))
        && line_has_result_shape(&lower)
    {
        return Some((EntryType::Result, 0.6));
    }

    if crate::parser::intent_phrases::phrases()
        .argue
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some((EntryType::Argue, 0.6));
    }

    if crate::parser::intent_phrases::phrases()
        .why
        .iter()
        .any(|m| lower.contains(m))
    {
        return Some((EntryType::Why, 0.7));
    }

    if modality == IntentLineModality::TypedDirective {
        return Some((EntryType::Intent, 0.8));
    }
    if is_user
        && intent_keywords()
            .iter()
            .any(|kw| matches_keyword_word_boundary(line, kw))
    {
        return Some((EntryType::Intent, 0.7));
    }
    if is_decision_tag(line) {
        return Some((EntryType::Decision, 0.9));
    }

    None
}

pub fn classify_chunk_entries(
    content: &str,
    source_chunk: &str,
    project: Option<&str>,
    agent: Option<&str>,
    session_id: Option<&str>,
    date: &str,
) -> Vec<IntentEntry> {
    let mut entries = Vec::new();
    let mut byte_offset = 0usize;

    let (signal_lines, transcript_entries) = parse_chunk_document(content);

    for line in &signal_lines {
        let trimmed = line.trim();
        if trimmed.is_empty()
            || trimmed == "[signals]"
            || trimmed == "[/signals]"
            || trimmed == "=== SKILL ENTER ==="
            || trimmed == "==================="
        {
            byte_offset += line.len() + 1;
            continue;
        }

        if let Some((entry_type, conf)) = classify_signal_line(trimmed)
            && conf >= CLASSIFIER_ABSTAIN_THRESHOLD
        {
            let title = clean_entry_title(entry_type, trimmed);
            if !title.is_empty() {
                let id = IntentEntry::stable_id(source_chunk, byte_offset, entry_type);
                let evidence = extract_evidence(&title);
                let tags = infer_tags(&title);
                entries.push(IntentEntry {
                    id,
                    entry_type,
                    state: initial_state(entry_type),
                    title: truncate_signal_line(&title),
                    body: None,
                    evidence,
                    links: Vec::new(),
                    superseded_by: None,
                    confidence: conf,
                    tags,
                    project: project.map(String::from),
                    agent: agent.map(String::from),
                    session_id: session_id.map(String::from),
                    timestamp: None,
                    date: date.to_string(),
                    source_chunk: source_chunk.to_string(),
                });
            }
        }
        byte_offset += line.len() + 1;
    }

    let signal_spec = intent_signal_spec();
    for entry in &transcript_entries {
        let is_user = is_intent_signal_role(&signal_spec, &entry.role);
        for raw_line in &entry.lines {
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                byte_offset += raw_line.len() + 1;
                continue;
            }

            if let Some((entry_type, conf)) = classify_line_entry_type(trimmed, is_user)
                && conf >= CLASSIFIER_ABSTAIN_THRESHOLD
            {
                if role_suppresses_outcome_promotion(&entry.role)
                    && matches!(entry_type, EntryType::Outcome | EntryType::Result)
                {
                    continue;
                }
                let title = clean_entry_title(entry_type, trimmed);
                if !title.is_empty() {
                    let id = IntentEntry::stable_id(source_chunk, byte_offset, entry_type);
                    let evidence = extract_evidence(&title);
                    let tags = infer_tags(&title);
                    entries.push(IntentEntry {
                        id,
                        entry_type,
                        state: initial_state(entry_type),
                        title: truncate_signal_line(&title),
                        body: None,
                        evidence,
                        links: Vec::new(),
                        superseded_by: None,
                        confidence: conf,
                        tags,
                        project: project.map(String::from),
                        agent: agent.map(String::from),
                        session_id: session_id.map(String::from),
                        timestamp: None,
                        date: date.to_string(),
                        source_chunk: source_chunk.to_string(),
                    });
                }
            }
            byte_offset += raw_line.len() + 1;
        }
    }

    entries
}

fn classify_signal_line(line: &str) -> Option<(EntryType, f32)> {
    let lower = line.to_lowercase();
    let trimmed = lower.trim();
    let payload = strip_signal_bullet(line);

    if trimmed == "intent:"
        || trimmed == "decision:"
        || trimmed == "results:"
        || trimmed == "outcome:"
        || trimmed == "ultrathink:"
        || trimmed == "insight:"
        || trimmed == "plan mode:"
        || trimmed == "notes:"
    {
        return None;
    }

    if let Some(result) = classify_line_entry_type(payload, false) {
        return Some(result);
    }

    if is_decision_tag(line) {
        return Some((EntryType::Decision, 0.9));
    }
    if is_outcome_tag(line) || is_result_line(line) {
        return Some((EntryType::Outcome, 0.75));
    }

    None
}

fn initial_state(entry_type: EntryType) -> EntryState {
    match entry_type {
        EntryType::Intent
        | EntryType::Task
        | EntryType::Commitment
        | EntryType::Question
        | EntryType::Assumption => EntryState::Proposed,
        EntryType::Decision | EntryType::Insight => EntryState::Active,
        EntryType::Outcome | EntryType::Result => EntryState::Done,
        EntryType::Why | EntryType::Argue => EntryState::Active,
    }
}

fn clean_entry_title(entry_type: EntryType, raw: &str) -> String {
    let text = strip_signal_bullet(raw);
    let stripped = match entry_type {
        EntryType::Decision => {
            let t = strip_case_insensitive_prefix(text, "[decision]");
            strip_case_insensitive_prefix(t, "decision:")
        }
        EntryType::Outcome => {
            let t = strip_case_insensitive_prefix(text, "[skill_outcome]");
            let t = strip_case_insensitive_prefix(t, "outcome:");
            strip_case_insensitive_prefix(t, "validation:")
        }
        EntryType::Result => strip_case_insensitive_prefix(text, "result:"),
        EntryType::Task => {
            let t = strip_case_insensitive_prefix(text, "task:");
            let t = strip_case_insensitive_prefix(t, "todo:");
            let t = strip_case_insensitive_prefix(t, "zadanie:");
            let t = strip_case_insensitive_prefix(t, "[ ]");
            let t = strip_case_insensitive_prefix(t, "[x]");
            strip_case_insensitive_prefix(t, "[X]")
        }
        EntryType::Commitment => {
            let t = strip_case_insensitive_prefix(text, "commitment:");
            let t = strip_case_insensitive_prefix(t, "promise:");
            strip_case_insensitive_prefix(t, "obietnica:")
        }
        EntryType::Question => strip_case_insensitive_prefix(text, "question:"),
        EntryType::Assumption => {
            let t = strip_case_insensitive_prefix(text, "assumption:");
            strip_case_insensitive_prefix(t, "hypothesis:")
        }
        EntryType::Insight => {
            let t = strip_case_insensitive_prefix(text, "insight:");
            strip_case_insensitive_prefix(t, "★ insight")
        }
        EntryType::Why => {
            let t = strip_case_insensitive_prefix(text, "because ");
            strip_case_insensitive_prefix(t, "why:")
        }
        EntryType::Intent | EntryType::Argue => text,
    };
    normalize_display_text(stripped)
}

fn infer_tags(title: &str) -> Vec<String> {
    let lower = title.to_lowercase();
    let mut tags = Vec::new();

    let tag_map: &[(&[&str], &str)] = &[
        (
            &["auth", "login", "session", "token", "jwt", "oauth"],
            "auth",
        ),
        (
            &[
                "database",
                "sql",
                "migration",
                "schema",
                "table",
                "query",
                "db",
            ],
            "db",
        ),
        (
            &[
                "ui",
                "frontend",
                "component",
                "css",
                "tailwind",
                "react",
                "button",
            ],
            "ui",
        ),
        (
            &["api", "endpoint", "route", "handler", "rest", "graphql"],
            "api",
        ),
        (
            &["test", "spec", "assert", "fixture", "coverage"],
            "testing",
        ),
        (
            &["deploy", "ci", "cd", "pipeline", "docker", "release"],
            "devops",
        ),
        (&["license", "licensing", "busl", "copyright"], "licensing"),
        (&["brand", "rebrand", "naming", "identity"], "brand"),
        (
            &["perf", "latency", "performance", "cache", "optimize"],
            "performance",
        ),
    ];

    for (keywords, tag) in tag_map {
        if keywords.iter().any(|kw| lower.contains(kw)) {
            tags.push((*tag).to_string());
            if tags.len() >= 5 {
                break;
            }
        }
    }

    tags
}

// ── Session-level post-processing ───────────────────────────────────

pub fn postprocess_session_entries(entries: &mut [IntentEntry], age_days: Option<i64>) {
    detect_unresolved(entries, age_days.unwrap_or(7));
    detect_supersedes(entries);
    detect_contradicted_assumptions(entries);
    link_insights_to_sources(entries);
}

fn detect_unresolved(entries: &mut [IntentEntry], threshold_days: i64) {
    let outcome_keys: HashSet<String> = entries
        .iter()
        .filter(|e| {
            matches!(
                e.entry_type,
                EntryType::Outcome | EntryType::Result | EntryType::Decision
            )
        })
        .filter_map(|e| e.session_id.clone())
        .collect();

    let has_outcome_for_session = |session_id: &Option<String>| -> bool {
        session_id
            .as_ref()
            .is_some_and(|sid| outcome_keys.contains(sid))
    };

    let today = chrono::Utc::now().date_naive();

    for entry in entries.iter_mut() {
        if entry.entry_type == EntryType::Intent && entry.state == EntryState::Proposed {
            let is_old = NaiveDate::parse_from_str(&entry.date, "%Y-%m-%d")
                .ok()
                .is_some_and(|d| (today - d).num_days() >= threshold_days);

            if is_old && !has_outcome_for_session(&entry.session_id) {
                entry.tags.push("unresolved".to_string());
                entry.tags.dedup();
            }
        }
    }
}

/// Parse a date string that may be either a bare `YYYY-MM-DD` day or a full
/// RFC3339 timestamp with any offset. Full timestamps are normalized to UTC;
/// bare dates map to midnight UTC so day-only and timestamped values compare
/// on a single typed axis instead of lexicographically (P3-09).
pub(crate) fn parse_flexible_utc(s: &str) -> Option<DateTime<Utc>> {
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Some(dt.with_timezone(&Utc));
    }
    NaiveDate::parse_from_str(s, "%Y-%m-%d")
        .ok()
        .map(|d| DateTime::<Utc>::from_naive_utc_and_offset(d.and_time(NaiveTime::MIN), Utc))
}

/// Compare two flexible date strings on the typed UTC axis when both parse;
/// fall back to the legacy lexicographic comparison for unparsable input so
/// garbage keeps its historical ordering.
pub(crate) fn cmp_dates_flexible(a: &str, b: &str) -> std::cmp::Ordering {
    match (parse_flexible_utc(a), parse_flexible_utc(b)) {
        (Some(da), Some(db)) => da.cmp(&db),
        _ => a.cmp(b),
    }
}

fn detect_supersedes(entries: &mut [IntentEntry]) {
    // Group supersession candidates by topic, then resolve each topic as a
    // date-ordered chain. Recomputing the whole chain (instead of applying
    // pairwise actions against a running "latest") makes the final states
    // independent of input order: an entry that supersedes an older sibling
    // while itself being superseded by a newer one always ends Superseded,
    // never Active (P2-01).
    let mut topics: HashMap<String, Vec<usize>> = HashMap::new();

    for (idx, entry) in entries.iter().enumerate() {
        if !matches!(
            entry.entry_type,
            EntryType::Intent | EntryType::Decision | EntryType::Insight
        ) {
            continue;
        }
        let topic_key = format!(
            "{}:{}:{}",
            entry.project.as_deref().unwrap_or(""),
            entry.entry_type.as_str(),
            normalize_key(&entry.title)
                .split_whitespace()
                .take(5)
                .collect::<Vec<_>>()
                .join(" ")
        );
        topics.entry(topic_key).or_default().push(idx);
    }

    for mut chain in topics.into_values() {
        if chain.len() < 2 {
            continue;
        }
        // Oldest first. Ties on date are broken by confidence (higher wins,
        // so it sorts later in the chain), then by input order (first-seen
        // wins), mirroring the previous pairwise rules.
        chain.sort_by(|&a, &b| {
            cmp_dates_flexible(&entries[a].date, &entries[b].date)
                .then_with(|| {
                    entries[a]
                        .confidence
                        .partial_cmp(&entries[b].confidence)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
                .then_with(|| b.cmp(&a))
        });

        for pair in chain.windows(2) {
            let (older_idx, newer_idx) = (pair[0], pair[1]);
            let older_id = entries[older_idx].id.clone();
            let newer_id = entries[newer_idx].id.clone();
            entries[older_idx].state = EntryState::Superseded;
            entries[older_idx].superseded_by = Some(newer_id);
            let already = entries[newer_idx]
                .links
                .iter()
                .any(|l| l.relation == LinkType::Supersedes && l.target == older_id);
            if !already {
                entries[newer_idx].links.push(Link {
                    relation: LinkType::Supersedes,
                    target: older_id,
                    confidence: Some(0.7),
                });
            }
        }

        // Only the chain head is promoted; every other member was just
        // marked Superseded above and must stay that way.
        let winner_idx = *chain.last().expect("chain has at least two members");
        entries[winner_idx].state = EntryState::Active;
    }
}

fn detect_contradicted_assumptions(entries: &mut [IntentEntry]) {
    let contradiction_words = ["fail", "broken", "wrong", "error", "invalid", "rejected"];

    // E.5: precompute token sets once per entry, and pre-filter Results to
    // those that actually carry a contradiction keyword. Then group those
    // Results by session_id so each Assumption only scans peers in its own
    // session (cross-session contradictions are not meaningful).
    struct Bucket {
        idx: usize,
        words: HashSet<String>,
    }

    let mut assumptions: Vec<(Option<String>, Bucket)> = Vec::new();
    let mut results_by_session: HashMap<Option<String>, Vec<Bucket>> = HashMap::new();

    for (idx, entry) in entries.iter().enumerate() {
        match entry.entry_type {
            EntryType::Assumption => {
                let key = normalize_key(&entry.title);
                let words: HashSet<String> =
                    key.split_whitespace().map(|w| w.to_string()).collect();
                assumptions.push((entry.session_id.clone(), Bucket { idx, words }));
            }
            EntryType::Result => {
                let title_lower = entry.title.to_lowercase();
                if !contradiction_words.iter().any(|w| title_lower.contains(w)) {
                    continue;
                }
                let words: HashSet<String> = title_lower
                    .split_whitespace()
                    .map(|w| w.to_string())
                    .collect();
                results_by_session
                    .entry(entry.session_id.clone())
                    .or_default()
                    .push(Bucket { idx, words });
            }
            _ => {}
        }
    }

    if assumptions.is_empty() || results_by_session.is_empty() {
        return;
    }

    for (session_id, a) in &assumptions {
        let Some(bucket) = results_by_session.get(session_id) else {
            continue;
        };
        for r in bucket {
            let overlap = a.words.intersection(&r.words).count();
            if overlap >= 2 {
                entries[a.idx].state = EntryState::Contradicted;
                let r_id = entries[r.idx].id.clone();
                entries[a.idx].links.push(Link {
                    relation: LinkType::Contradicts,
                    target: r_id,
                    confidence: Some(0.6),
                });
            }
        }
    }
}

fn link_insights_to_sources(entries: &mut [IntentEntry]) {
    let source_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            matches!(
                e.entry_type,
                EntryType::Result | EntryType::Outcome | EntryType::Why
            )
        })
        .map(|(i, _)| i)
        .collect();

    let insight_indices: Vec<usize> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.entry_type == EntryType::Insight)
        .map(|(i, _)| i)
        .collect();

    for &i_idx in &insight_indices {
        let insight_session = entries[i_idx].session_id.clone();
        let mut linked_count = 0;
        for &s_idx in &source_indices {
            if linked_count >= 3 {
                break;
            }
            if entries[s_idx].session_id == insight_session {
                let target_id = entries[s_idx].id.clone();
                let already = entries[i_idx].links.iter().any(|l| l.target == target_id);
                if !already {
                    entries[i_idx].links.push(Link {
                        relation: LinkType::DerivedFrom,
                        target: target_id,
                        confidence: Some(0.65),
                    });
                    linked_count += 1;
                }
            }
        }
    }
}

// ── Migration support ───────────────────────────────────────────────

pub fn migrate_intent_schema_dry_run(project_filter: Option<&str>) -> Result<MigrationReport> {
    migrate_intent_schema_dry_run_at(&crate::aicx_home::ensure()?, project_filter)
}

pub fn migrate_intent_schema_dry_run_at(
    aicx_home: &Path,
    project_filter: Option<&str>,
) -> Result<MigrationReport> {
    let inferred_project = project_filter.map(str::to_string);
    let files = collect_legacy_chunk_files(
        aicx_home,
        project_filter.unwrap_or(""),
        DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2020, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            Utc,
        ),
        IntentsConfig::default_frame_kind(),
        &IntentSourceFilter::default(),
    )?;

    let mut all_entries = Vec::new();
    let total_chunks = files.len();

    for file in &files {
        let content = sanitize::read_to_string_validated(&file.path)
            .with_context(|| format!("Failed to read chunk file: {}", file.path.display()))?;
        let source_chunk = file.path.to_string_lossy().to_string();
        let project_label = inferred_project
            .clone()
            .unwrap_or_else(|| normalize_migration_project_label(&file.project));
        let mut chunk_entries = classify_chunk_entries(
            &content,
            &source_chunk,
            Some(project_label.as_str()),
            Some(&file.agent),
            Some(&file.session_id),
            &file.date,
        );
        for e in &mut chunk_entries {
            e.timestamp = Some(file.timestamp.to_rfc3339());
            e.project = Some(project_label.clone());
        }
        all_entries.extend(chunk_entries);
    }

    postprocess_session_entries(&mut all_entries, Some(7));

    let mut per_type: HashMap<String, usize> = HashMap::new();
    let mut per_project: HashMap<String, usize> = HashMap::new();
    let mut unresolved_count = 0;

    for entry in &all_entries {
        *per_type
            .entry(entry.entry_type.as_str().to_string())
            .or_default() += 1;
        if let Some(ref proj) = entry.project {
            *per_project.entry(proj.clone()).or_default() += 1;
        }
        if entry.tags.contains(&"unresolved".to_string()) {
            unresolved_count += 1;
        }
    }

    Ok(MigrationReport {
        total_chunks,
        entries_found: all_entries.len(),
        per_type,
        per_project,
        unresolved_count,
    })
}

fn normalize_migration_project_label(project: &str) -> String {
    project
        .strip_prefix("local/")
        .unwrap_or(project)
        .to_string()
}

#[cfg(test)]
mod tests;

#[cfg(all(test, feature = "app"))]
#[path = "intents/continuity_contract_tests.rs"]
mod continuity_contract_tests;
