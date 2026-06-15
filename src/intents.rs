//! Intention Engine for ai-contexters.
//!
//! Elevates stored chunk `[signals]` metadata and matching raw conversation
//! lines into first-class, queryable intent records.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::chunker::{
    INTENT_KEYWORDS, is_decision_tag, is_outcome_tag, is_result_line, normalize_key,
    parse_checklist_task, truncate_signal_line,
};
use crate::sanitize;
use crate::sources::shared::{IntentLineModality, intent_line_modality};
use crate::store;
use crate::timeline::FrameKind;
use crate::types::{EntryState, EntryType, IntentEntry, Link, LinkType};

mod display;
mod schema;
mod types;

pub use self::display::{
    IntentDisplayFilters, IntentSortOrder, UnresolvedMode, apply_display_filters,
    format_intents_json, format_intents_markdown, format_intents_oracle_json,
};
use self::types::{
    CandidateAccumulator, IntentCandidate, SignalSection, StoredChunkFile, TaskAccumulator,
    TaskEvent, TranscriptEntry,
};
pub use self::types::{
    IntentExtraction, IntentExtractionStats, IntentKind, IntentRecord, IntentsConfig,
    MigrationReport,
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

pub fn extract_intents(config: &IntentsConfig) -> Result<Vec<IntentRecord>> {
    Ok(extract_intents_with_stats(config)?.records)
}

pub fn extract_intents_with_stats(config: &IntentsConfig) -> Result<IntentExtraction> {
    let store_root = store::store_base_dir()?;
    extract_intents_from_root_at_with_stats(config, &store_root, Utc::now())
}

pub fn extract_intents_with_stats_for_projects(
    config: &IntentsConfig,
    projects: &[String],
) -> Result<IntentExtraction> {
    let store_root = store::store_base_dir()?;
    extract_intents_from_root_at_for_projects_with_stats(config, projects, &store_root, Utc::now())
}

#[cfg(test)]
fn extract_intents_from_root_at(
    config: &IntentsConfig,
    store_root: &Path,
    now: DateTime<Utc>,
) -> Result<Vec<IntentRecord>> {
    Ok(extract_intents_from_root_at_with_stats(config, store_root, now)?.records)
}

pub(crate) fn extract_intents_from_root_at_with_stats(
    config: &IntentsConfig,
    store_root: &Path,
    now: DateTime<Utc>,
) -> Result<IntentExtraction> {
    let cutoff = if config.hours == 0 {
        DateTime::<Utc>::from_timestamp(0, 0).expect("Unix epoch timestamp is valid")
    } else {
        let cutoff_hours = config.hours.min(i64::MAX as u64) as i64;
        now - Duration::hours(cutoff_hours)
    };
    let files = collect_chunk_files(
        store_root,
        &config.project,
        cutoff,
        config.effective_frame_kind(),
    )?;
    let scanned_count = files.len();
    let source_paths_verified = verify_stored_chunk_paths(&files);

    let mut candidates = Vec::new();
    let mut task_events = Vec::new();
    let mut cap_warned = false;

    for file in files {
        let content = sanitize::read_to_string_validated(&file.path)
            .with_context(|| format!("Failed to read chunk file: {}", file.path.display()))?;

        let (signal_lines, transcript_entries) = parse_chunk_document(&content);
        let source_chunk = file.path.to_string_lossy().to_string();

        let (signal_candidates, signal_tasks) =
            extract_signal_candidates(&file, &config.project, &source_chunk, &signal_lines);
        extend_with_cap(
            &mut candidates,
            signal_candidates,
            &mut cap_warned,
            "candidates",
        );
        extend_with_cap(
            &mut task_events,
            signal_tasks,
            &mut cap_warned,
            "task_events",
        );

        let (raw_candidates, raw_tasks) = extract_transcript_candidates(
            &file,
            &config.project,
            &source_chunk,
            &transcript_entries,
        );
        extend_with_cap(
            &mut candidates,
            raw_candidates,
            &mut cap_warned,
            "candidates",
        );
        extend_with_cap(&mut task_events, raw_tasks, &mut cap_warned, "task_events");

        if candidates.len() >= MAX_CANDIDATES && task_events.len() >= MAX_CANDIDATES {
            // Both buckets saturated — further files cannot add anything.
            break;
        }
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

    reconcile_session_id_with_path(&mut records);

    sort_intent_records(&mut records);

    let stats = IntentExtractionStats {
        scanned_count,
        candidate_count: records.len(),
        source_paths_verified,
    };

    Ok(IntentExtraction { records, stats })
}

pub(crate) fn extract_intents_from_root_at_for_projects_with_stats(
    config: &IntentsConfig,
    projects: &[String],
    store_root: &Path,
    now: DateTime<Utc>,
) -> Result<IntentExtraction> {
    if projects.is_empty() {
        return extract_intents_from_root_at_with_stats(config, store_root, now);
    }

    let mut records = Vec::new();
    let mut scanned_count = 0usize;
    let mut source_paths_verified = true;

    for project in projects {
        let mut scoped = config.clone();
        scoped.project = project.clone();
        let extraction = extract_intents_from_root_at_with_stats(&scoped, store_root, now)?;
        scanned_count += extraction.stats.scanned_count;
        source_paths_verified &= extraction.stats.source_paths_verified;
        records.extend(extraction.records);
    }

    dedup_intent_records(&mut records);
    sort_intent_records(&mut records);

    let stats = IntentExtractionStats {
        scanned_count,
        candidate_count: records.len(),
        source_paths_verified,
    };

    Ok(IntentExtraction { records, stats })
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
        seen.insert((
            record.kind,
            normalize_key(&record.summary),
            record.session_id.clone(),
            record.source_chunk.clone(),
        ))
    });
}

fn verify_stored_chunk_paths(files: &[StoredChunkFile]) -> bool {
    files.iter().all(|file| file.path.exists())
}

/// E.6: append `additions` into `target` until `target` reaches MAX_CANDIDATES.
/// Emits a single stderr diagnostic the first time a cap is hit per run.
fn extend_with_cap<T>(
    target: &mut Vec<T>,
    additions: Vec<T>,
    warned: &mut bool,
    bucket_name: &'static str,
) {
    let room = MAX_CANDIDATES.saturating_sub(target.len());
    if additions.len() <= room {
        target.extend(additions);
        return;
    }
    let dropped = additions.len() - room;
    target.extend(additions.into_iter().take(room));
    if !*warned {
        eprintln!(
            "aicx intents: warning: {bucket_name} cap of {MAX_CANDIDATES} reached; dropped {dropped} entries"
        );
        *warned = true;
    }
}

fn collect_chunk_files(
    store_root: &Path,
    project: &str,
    cutoff: DateTime<Utc>,
    frame_kind: FrameKind,
) -> Result<Vec<StoredChunkFile>> {
    let mut files = Vec::new();
    let scan_root = normalize_scan_root(store_root);

    for file in store::scan_context_files_at(&scan_root)? {
        if file.path.extension().and_then(|ext| ext.to_str()) != Some("md") {
            continue;
        }
        if !file
            .project
            .to_ascii_lowercase()
            .contains(&project.to_ascii_lowercase())
        {
            continue;
        }
        let sidecar = store::load_sidecar(&file.path);
        if sidecar.as_ref().is_some_and(|sidecar| {
            sidecar.artifact_family.as_deref() == Some(store::LOCT_CONTEXT_PACK_FAMILY)
                || sidecar
                    .truth_status
                    .as_ref()
                    .is_some_and(|status| status.role == crate::chunker::TruthRole::Example)
        }) {
            continue;
        }
        // Legacy chunks (no sidecar yet, or a pre-frame_kind sidecar) belong
        // to the default user_msg lane; requiring an explicit frame_kind here
        // silently emptied intents on stores written before the field existed.
        let chunk_frame = sidecar
            .and_then(|sidecar| sidecar.frame_kind)
            .unwrap_or_else(IntentsConfig::default_frame_kind);
        if chunk_frame != frame_kind {
            continue;
        }

        // Recency is anchored to the canonical chunk date encoded in the store
        // layout. Filesystem mtime drifts during daily sync/migration and must
        // not make stale sessions look fresh.
        let canonical_date = NaiveDate::parse_from_str(&file.date_iso, "%Y-%m-%d").ok();
        let timestamp = canonical_date
            .and_then(|date| combine_date_time(date, "000000"))
            .or_else(|| {
                file.path
                    .metadata()
                    .ok()
                    .and_then(|metadata| metadata.modified().ok())
                    .map(DateTime::<Utc>::from)
            });
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
            project: file.project,
            sequence: file.chunk,
            timestamp,
            session_id: file.session_id,
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

fn normalize_scan_root(store_root: &Path) -> PathBuf {
    if store_root
        .file_name()
        .is_some_and(|name| name == store::CANONICAL_STORE_DIRNAME)
    {
        return store_root
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| store_root.to_path_buf());
    }

    store_root.to_path_buf()
}

fn combine_date_time(date: NaiveDate, time: &str) -> Option<DateTime<Utc>> {
    let time = NaiveTime::parse_from_str(time, "%H%M%S").ok()?;
    let datetime = NaiveDateTime::new(date, time);
    Some(DateTime::<Utc>::from_naive_utc_and_offset(datetime, Utc))
}

fn parse_chunk_document(content: &str) -> (Vec<String>, Vec<TranscriptEntry>) {
    let mut in_signals = false;
    let mut fenced = false;
    let mut signal_lines = Vec::new();
    let mut transcript_lines = Vec::new();

    for line in content.lines() {
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
        if trimmed.starts_with("[project:") {
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
        {
            continue;
        }

        let payload = strip_signal_bullet(line);
        if is_source_metadata_line(payload) || is_local_command_artifact_line(payload) {
            continue;
        }
        let kind = match section {
            SignalSection::Intent => Some(IntentKind::Intent),
            SignalSection::Decision => Some(IntentKind::Decision),
            SignalSection::Results | SignalSection::Outcome => Some(IntentKind::Outcome),
            SignalSection::Ignore | SignalSection::None => infer_kind_from_line(payload, false),
        };

        if let Some(kind) = kind
            && let Some(candidate) =
                build_candidate(kind, payload, None, file, project, source_chunk, true, None)
        {
            candidates.push(candidate);
        }
    }

    (candidates, task_events)
}

fn extract_transcript_candidates(
    file: &StoredChunkFile,
    project: &str,
    source_chunk: &str,
    transcript_entries: &[TranscriptEntry],
) -> (Vec<IntentCandidate>, Vec<TaskEvent>) {
    let mut candidates = Vec::new();
    let mut task_events = Vec::new();

    for entry in transcript_entries {
        let is_user = entry.role.eq_ignore_ascii_case("user");
        let mut codescribe_parser = CodescribeParser::new();

        for (index, raw_line) in entry.lines.iter().enumerate() {
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

            let context = surrounding_context(&entry.lines, index);
            let source_provenance = if is_voice {
                Some("voice_transcript".to_string())
            } else {
                None
            };

            if let Some((is_done, task)) = parse_checklist_task(line) {
                if let Some(event) = build_task_event(
                    &task,
                    context,
                    file,
                    project,
                    source_chunk,
                    !is_done,
                    false,
                    source_provenance,
                ) {
                    task_events.push(event);
                }
                continue;
            }

            let Some(kind) = infer_kind_from_line(line, is_user) else {
                continue;
            };

            if let Some(candidate) = build_candidate(
                kind,
                line,
                context,
                file,
                project,
                source_chunk,
                false,
                source_provenance,
            ) {
                candidates.push(candidate);
            }
        }
    }

    (candidates, task_events)
}

fn is_local_command_artifact_line(line: &str) -> bool {
    let trimmed = strip_signal_bullet(line).trim();
    let lower = trimmed.to_ascii_lowercase();
    if lower.starts_with("<local-command-caveat")
        || lower.starts_with("</local-command-caveat")
        || lower.starts_with("<bash-stdout")
        || lower.starts_with("</bash-stdout")
        || lower.starts_with("<bash-stderr")
        || lower.starts_with("</bash-stderr")
    {
        return true;
    }

    let shell_line = trimmed.trim_start_matches(['*', '>', '<']).trim_start();
    let shell_lower = shell_line.to_ascii_lowercase();
    shell_lower.starts_with("issuer:")
        || shell_lower.starts_with("subject:")
        || shell_lower.starts_with("subjectaltname:")
        || shell_lower.starts_with("ssl certificate")
        || shell_lower.starts_with("alpn:")
        || shell_lower.starts_with("http/")
        || shell_lower.starts_with("server:")
        || shell_lower.starts_with("content-type:")
        || shell_lower.starts_with("x-ratelimit-")
        || shell_lower.starts_with("x-request-id:")
        || shell_lower.starts_with("strict-transport-security:")
        || shell_lower.starts_with("content-security-policy:")
        || shell_lower.starts_with("referrer-policy:")
        || shell_lower.starts_with("permissions-policy:")
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
        EntryType::Intent | EntryType::Question | EntryType::Why => Some(IntentKind::Intent),
        EntryType::Outcome | EntryType::Result => Some(IntentKind::Outcome),
        EntryType::Argue | EntryType::Assumption | EntryType::Insight => None,
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
    const BARE: &[&str] = &[
        "zrobione",
        "dowiezione",
        "gotowe",
        "dziala",
        "działa",
        "done",
        "completed",
    ];
    let trimmed = line.trim().trim_end_matches(['.', '!', ',']);
    if trimmed.is_empty() || trimmed.contains(':') {
        return false;
    }
    let stripped = trimmed
        .trim_start_matches(['-', '*', '+', '>', ' ', '\t'])
        .to_lowercase();
    BARE.iter().any(|word| stripped == *word)
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
    const PRE_NEGATORS: &[&str] = &[
        // Polish
        "nie ",
        "bez ",
        // English
        "don't ",
        "do not ",
        "won't ",
        "will not ",
        "shouldn't ",
        "should not ",
        "wouldn't ",
        "would not ",
        "isn't ",
        "aren't ",
        "doesn't ",
        "didn't ",
    ];
    const POST_NEGATORS: &[&str] = &[" not ", " not,", " not.", " nie ", " nie,", " nie."];

    let pre_window_start = lower_line[..kw_pos]
        .char_indices()
        .rev()
        .take(24)
        .last()
        .map(|(i, _)| i)
        .unwrap_or(0);
    let pre = &lower_line[pre_window_start..kw_pos];
    if PRE_NEGATORS.iter().any(|n| pre.ends_with(n)) {
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
        if POST_NEGATORS.iter().any(|n| post.starts_with(n)) {
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
    INTENT_KEYWORDS
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
    [
        "nie może być",
        "nie moze byc",
        "ma być",
        "ma byc",
        "musi ",
        "musimy ",
        "trzeba ",
        "zrób to testowalne",
        "zrob to testowalne",
        "pełny ownership",
        "pelny ownership",
        "teraz wypuszuj",
    ]
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
/// `store::session_basename` and treated as ground truth — that file actually
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

        // Session-scoped key. Cross-session merges silently swap source_chunk
        // while keeping the wrong session_id, lying about provenance. Keep
        // identical text in different sessions as separate records.
        let key = (
            candidate.record.kind,
            normalize_key(&candidate.record.summary),
            candidate.record.session_id.clone(),
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
        existing.candidate.record.source_chunk = incoming.record.source_chunk.clone();
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

// ── 9-type intent entry classifier ──────────────────────────────────

const CLASSIFIER_ABSTAIN_THRESHOLD: f32 = 0.5;

const QUESTION_MARKERS: &[&str] = &[
    "how do",
    "how does",
    "how to",
    "what is",
    "what are",
    "why does",
    "why is",
    "can we",
    "should we",
    "is it possible",
    "does it",
    "do we",
    "jak ",
    "dlaczego ",
    "czy ",
    "co to ",
    "co jest",
    "w jaki sposób",
];

const ASSUMPTION_MARKERS: &[&str] = &[
    "i assume",
    "assuming",
    "i believe",
    "we assume",
    "hypothesis:",
    "zakładam",
    "założenie:",
    "hipoteza:",
    "przypuszczam",
];

const WHY_MARKERS: &[&str] = &[
    "why:",
    "because",
    "the reason",
    "this is needed",
    "motivated by",
    "driven by",
    "root cause",
    "underlying issue",
    "bo ",
    "ponieważ",
    "dlatego że",
    "przyczyna:",
    "powód:",
];

const ARGUE_MARKERS: &[&str] = &[
    "on the other hand",
    "alternatively",
    "disagree",
    "counterpoint",
    "trade-off",
    "tradeoff",
    "but if we",
    "however,",
    "z drugiej strony",
    "alternatywnie",
    "spór:",
    "kontrargument",
];

const INSIGHT_MARKERS: &[&str] = &[
    "insight:",
    "realization:",
    "key finding:",
    "★ insight",
    "the real issue is",
    "fundamentally,",
    "odkrycie:",
    "wniosek:",
    "kluczowe:",
];

/// Markers whose presence alone is enough to call a line a Result line. Each
/// carries result-shape on its own (PASS/FAIL outcome, score readout, P-level
/// count, command name that only appears in result-reporting contexts).
const RESULT_STRICT_MARKERS: &[&str] = &[
    "passed",
    "failed",
    "score=",
    "score:",
    "latency",
    "p0=",
    "p1=",
    "p2=",
    "/10",
    "clippy",
    "cargo test",
    "✓",
    "✗",
    "0 warnings",
    "0 errors",
];

/// Markers that look result-y but appear too often in meta-discussion (e.g.
/// "we need to write tests for X", "this throws an error: should we…").
/// These classify a line as Result only when [`line_has_result_shape`] matches.
const RESULT_SOFT_MARKERS: &[&str] = &["tests ", "error:"];

/// A line "has result shape" when it carries a concrete reporting signal:
/// a digit (test count, error count, percentage), a PASS/FAIL token, or a
/// known status word. Without one, soft markers like "tests" or "error:" are
/// almost certainly meta-discussion, not actual outcomes.
fn line_has_result_shape(lower_line: &str) -> bool {
    if lower_line.chars().any(|c| c.is_ascii_digit()) {
        return true;
    }
    const SHAPE_TOKENS: &[&str] = &[
        "pass", "fail", " ok", "ok.", "done", "skipped", "ignored", "timeout", "panicked",
        "panic:", "✓", "✗",
    ];
    SHAPE_TOKENS.iter().any(|t| lower_line.contains(t))
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
    if is_user && looks_like_operator_decision_line(line) {
        return Some((EntryType::Decision, 0.75));
    }

    if trimmed.starts_with("question:") || trimmed.ends_with('?') && trimmed.len() > 15 {
        let conf = if trimmed.starts_with("question:") {
            0.95
        } else {
            0.7
        };
        if QUESTION_MARKERS.iter().any(|m| lower.contains(m)) || trimmed.ends_with('?') {
            return Some((EntryType::Question, conf));
        }
    }

    if trimmed.starts_with("assumption:")
        || trimmed.starts_with("hypothesis:")
        || trimmed.starts_with("zakładam")
        || trimmed.starts_with("założenie:")
        || trimmed.starts_with("hipoteza:")
    {
        return Some((EntryType::Assumption, 0.9));
    }
    if ASSUMPTION_MARKERS.iter().any(|m| lower.contains(m)) {
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
    if INSIGHT_MARKERS.iter().any(|m| lower.contains(m)) {
        return Some((EntryType::Insight, 0.65));
    }

    if is_outcome_tag(line) || trimmed.starts_with("[skill_outcome]") {
        return Some((EntryType::Outcome, 0.9));
    }

    if trimmed.starts_with("result:") || trimmed.starts_with("wynik:") {
        return Some((EntryType::Result, 0.95));
    }
    if is_result_line(line) || RESULT_STRICT_MARKERS.iter().any(|m| lower.contains(m)) {
        return Some((EntryType::Result, 0.75));
    }
    if RESULT_SOFT_MARKERS.iter().any(|m| lower.contains(m)) && line_has_result_shape(&lower) {
        return Some((EntryType::Result, 0.6));
    }

    if ARGUE_MARKERS.iter().any(|m| lower.contains(m)) {
        return Some((EntryType::Argue, 0.6));
    }

    if WHY_MARKERS.iter().any(|m| lower.contains(m)) {
        return Some((EntryType::Why, 0.7));
    }

    if modality == IntentLineModality::TypedDirective {
        return Some((EntryType::Intent, 0.8));
    }
    if is_user
        && INTENT_KEYWORDS
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

    for entry in &transcript_entries {
        let is_user = entry.role.eq_ignore_ascii_case("user");
        for raw_line in &entry.lines {
            let trimmed = raw_line.trim();
            if trimmed.is_empty() {
                byte_offset += raw_line.len() + 1;
                continue;
            }

            if let Some((entry_type, conf)) = classify_line_entry_type(trimmed, is_user)
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
        EntryType::Intent | EntryType::Question | EntryType::Assumption => EntryState::Proposed,
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
    migrate_intent_schema_dry_run_at(&store::store_base_dir()?, project_filter)
}

pub fn migrate_intent_schema_dry_run_at(
    store_root: &Path,
    project_filter: Option<&str>,
) -> Result<MigrationReport> {
    let inferred_project = project_filter.map(str::to_string);
    let files = collect_chunk_files(
        store_root,
        project_filter.unwrap_or(""),
        DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2020, 1, 1)
                .unwrap()
                .and_hms_opt(0, 0, 0)
                .unwrap(),
            Utc,
        ),
        IntentsConfig::default_frame_kind(),
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
