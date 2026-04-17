//! Intention Engine for ai-contexters.
//!
//! Elevates stored chunk `[signals]` metadata and matching raw conversation
//! lines into first-class, queryable intent records.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::{Context, Result};
use chrono::{DateTime, Duration, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::chunker::{
    INTENT_KEYWORDS, is_decision_tag, is_outcome_tag, is_result_line, normalize_key,
    parse_checklist_task, truncate_signal_line,
};
use crate::sanitize;
use crate::store;
use crate::types::{EntryState, EntryType, FrameKind, IntentEntry, Link, LinkType};

const STRICT_CONFIDENCE: u8 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum IntentKind {
    Decision,
    Intent,
    Outcome,
    Task,
}

impl IntentKind {
    pub fn heading(self) -> &'static str {
        match self {
            Self::Decision => "DECISION",
            Self::Intent => "INTENT",
            Self::Outcome => "OUTCOME",
            Self::Task => "TASK",
        }
    }

    fn sort_rank(self) -> u8 {
        match self {
            Self::Decision => 0,
            Self::Intent => 1,
            Self::Outcome => 2,
            Self::Task => 3,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntentRecord {
    pub kind: IntentKind,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    pub evidence: Vec<String>,
    pub project: String,
    pub agent: String,
    pub date: String,
    pub timestamp: Option<String>,
    pub session_id: String,
    pub count: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_chunk: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_chunk: Option<String>,
    pub source_chunk: String,
}
#[derive(Debug, Clone)]
pub struct IntentsConfig {
    pub project: String,
    pub hours: u64,
    pub strict: bool,
    pub kind_filter: Option<IntentKind>,
    pub frame_kind: Option<crate::types::FrameKind>,
}

#[derive(Debug, Clone)]
struct StoredChunkFile {
    agent: String,
    date: String,
    path: PathBuf,
    project: String,
    sequence: u32,
    timestamp: DateTime<Utc>,
    session_id: String,
}

#[derive(Debug, Clone)]
struct TranscriptEntry {
    role: String,
    lines: Vec<String>,
}

#[derive(Debug, Clone)]
struct IntentCandidate {
    record: IntentRecord,
    confidence: u8,
    timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
struct TaskEvent {
    key: String,
    candidate: IntentCandidate,
    is_open: bool,
}

#[derive(Debug, Clone)]
struct CandidateAccumulator {
    candidate: IntentCandidate,
}

#[derive(Debug, Clone)]
struct TaskAccumulator {
    candidate: IntentCandidate,
    is_open: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SignalSection {
    None,
    Intent,
    Decision,
    Results,
    Outcome,
    Ignore,
}

pub fn extract_intents(config: &IntentsConfig) -> Result<Vec<IntentRecord>> {
    let store_root = store::store_base_dir()?;
    extract_intents_from_root_at(config, &store_root, Utc::now())
}

pub fn format_intents_markdown(records: &[IntentRecord]) -> String {
    if records.is_empty() {
        return String::new();
    }

    let mut out = String::from("# Intent Timeline\n\n");
    let mut last_date: Option<&str> = None;

    for record in records {
        if last_date != Some(record.date.as_str()) {
            if last_date.is_some() {
                out.push('\n');
            }
            out.push_str(&format!("## {}\n\n", record.date));
            last_date = Some(record.date.as_str());
        }

        out.push_str(&format!(
            "### {} | {}\n",
            record.kind.heading(),
            record.agent
        ));
        out.push_str(&format!("{}: {}\n", record.kind.heading(), record.summary));
        out.push_str(&format!(
            "WHY: {}\n",
            record.context.as_deref().unwrap_or("not captured")
        ));
        out.push_str("EVIDENCE:\n");
        out.push_str(&format!("- source_chunk: {}\n", record.source_chunk));
        for evidence in &record.evidence {
            out.push_str(&format!("- {}\n", evidence));
        }
        out.push('\n');
    }

    out
}

pub fn format_intents_json(records: &[IntentRecord]) -> Result<String> {
    serde_json::to_string_pretty(records).context("Failed to serialize intents to JSON")
}

fn extract_intents_from_root_at(
    config: &IntentsConfig,
    store_root: &Path,
    now: DateTime<Utc>,
) -> Result<Vec<IntentRecord>> {
    let cutoff_hours = config.hours.min(i64::MAX as u64) as i64;
    let cutoff = now - Duration::hours(cutoff_hours);
    let files = collect_chunk_files(store_root, &config.project, cutoff, config.frame_kind)?;

    let mut candidates = Vec::new();
    let mut task_events = Vec::new();

    for file in files {
        let content = sanitize::read_to_string_validated(&file.path)
            .with_context(|| format!("Failed to read chunk file: {}", file.path.display()))?;

        let (signal_lines, transcript_entries) = parse_chunk_document(&content);
        let source_chunk = file.path.to_string_lossy().to_string();

        let (signal_candidates, signal_tasks) =
            extract_signal_candidates(&file, &config.project, &source_chunk, &signal_lines);
        candidates.extend(signal_candidates);
        task_events.extend(signal_tasks);

        let (raw_candidates, raw_tasks) = extract_transcript_candidates(
            &file,
            &config.project,
            &source_chunk,
            &transcript_entries,
        );
        candidates.extend(raw_candidates);
        task_events.extend(raw_tasks);
    }

    let mut records = dedup_candidates(candidates, config.strict, config.kind_filter);
    let mut task_records = finalize_tasks(task_events, config.strict, config.kind_filter);
    records.append(&mut task_records);

    records.sort_by(|left, right| {
        right
            .date
            .cmp(&left.date)
            .then_with(|| left.kind.sort_rank().cmp(&right.kind.sort_rank()))
            .then_with(|| right.source_chunk.cmp(&left.source_chunk))
            .then_with(|| left.summary.cmp(&right.summary))
    });

    Ok(records)
}

fn collect_chunk_files(
    store_root: &Path,
    project: &str,
    cutoff: DateTime<Utc>,
    frame_kind: Option<FrameKind>,
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
        if let Some(expected) = frame_kind {
            let matches_frame = store::load_sidecar(&file.path)
                .and_then(|sidecar| sidecar.frame_kind)
                .is_some_and(|kind| kind == expected);
            if !matches_frame {
                continue;
            }
        }

        let timestamp = file
            .path
            .metadata()
            .ok()
            .and_then(|metadata| metadata.modified().ok())
            .map(DateTime::<Utc>::from)
            .or_else(|| {
                NaiveDate::parse_from_str(&file.date_iso, "%Y-%m-%d")
                    .ok()
                    .and_then(|date| combine_date_time(date, "000000"))
            });
        let Some(timestamp) = timestamp else {
            continue;
        };
        if timestamp < cutoff {
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

    for raw_line in signal_lines {
        let line = raw_line.trim();
        if line.is_empty() {
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
                STRICT_CONFIDENCE,
            ) {
                task_events.push(event);
            }
            continue;
        }

        if line.starts_with("RED LIGHT: checklist detected")
            || line.starts_with("Checklist detected")
            || line.starts_with("... (+")
        {
            continue;
        }

        let payload = strip_signal_bullet(line);
        let kind = match section {
            SignalSection::Intent => Some(IntentKind::Intent),
            SignalSection::Decision => Some(IntentKind::Decision),
            SignalSection::Results | SignalSection::Outcome => Some(IntentKind::Outcome),
            SignalSection::Ignore | SignalSection::None => infer_kind_from_line(payload, false),
        };

        if let Some(kind) = kind
            && let Some(candidate) = build_candidate(
                kind,
                payload,
                None,
                file,
                project,
                source_chunk,
                STRICT_CONFIDENCE,
            )
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

        for (index, raw_line) in entry.lines.iter().enumerate() {
            let line = raw_line.trim();
            if line.is_empty() {
                continue;
            }

            let context = surrounding_context(&entry.lines, index);

            if let Some((is_done, task)) = parse_checklist_task(line) {
                if let Some(event) = build_task_event(
                    &task,
                    context,
                    file,
                    project,
                    source_chunk,
                    !is_done,
                    STRICT_CONFIDENCE,
                ) {
                    task_events.push(event);
                }
                continue;
            }

            let Some(kind) = infer_kind_from_line(line, is_user) else {
                continue;
            };
            let confidence = match kind {
                IntentKind::Intent => 2,
                _ => STRICT_CONFIDENCE,
            };

            if let Some(candidate) =
                build_candidate(kind, line, context, file, project, source_chunk, confidence)
            {
                candidates.push(candidate);
            }
        }
    }

    (candidates, task_events)
}

fn infer_kind_from_line(line: &str, is_user_line: bool) -> Option<IntentKind> {
    if is_decision_tag(line) {
        return Some(IntentKind::Decision);
    }
    if is_outcome_line(line) {
        return Some(IntentKind::Outcome);
    }
    if is_user_line && looks_like_intent_line(line) {
        return Some(IntentKind::Intent);
    }
    None
}

fn is_outcome_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    is_outcome_tag(line)
        || is_result_line(line)
        || lower.contains("p0=0")
        || lower.contains("p1=0")
        || lower.contains("p2=0")
}

fn looks_like_intent_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    INTENT_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

fn build_candidate(
    kind: IntentKind,
    raw_summary: &str,
    context: Option<String>,
    file: &StoredChunkFile,
    project: &str,
    source_chunk: &str,
    confidence: u8,
) -> Option<IntentCandidate> {
    let summary = normalize_display_text(&clean_summary(kind, raw_summary));
    if summary.is_empty() {
        return None;
    }

    let context = context
        .map(|value| normalize_display_text(&value))
        .filter(|value| !value.is_empty() && normalize_key(value) != normalize_key(&summary))
        .map(|value| truncate_signal_line(&value));

    let mut evidence = extract_evidence(&summary);
    if let Some(extra) = context.as_deref() {
        merge_evidence(&mut evidence, extract_evidence(extra));
    }

    Some(IntentCandidate {
        record: IntentRecord {
            kind,
            summary: truncate_signal_line(&summary),
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
        },
        confidence,
        timestamp: file.timestamp,
    })
}

fn build_task_event(
    task: &str,
    context: Option<String>,
    file: &StoredChunkFile,
    project: &str,
    source_chunk: &str,
    is_open: bool,
    confidence: u8,
) -> Option<TaskEvent> {
    let candidate = build_candidate(
        IntentKind::Task,
        task,
        context,
        file,
        project,
        source_chunk,
        confidence,
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
        IntentKind::Intent | IntentKind::Task => {}
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

    if let Some(prev) = index.checked_sub(1).and_then(|idx| lines.get(idx)) {
        let prev = normalize_display_text(prev);
        if !prev.is_empty() {
            parts.push(prev);
        }
    }

    if let Some(next) = lines.get(index + 1) {
        let next = normalize_display_text(next);
        if !next.is_empty() {
            parts.push(next);
        }
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

fn dedup_candidates(
    candidates: Vec<IntentCandidate>,
    strict: bool,
    kind_filter: Option<IntentKind>,
) -> Vec<IntentRecord> {
    let mut map: HashMap<(IntentKind, String), CandidateAccumulator> = HashMap::new();

    for candidate in candidates {
        if kind_filter.is_some() && kind_filter != Some(candidate.record.kind) {
            continue;
        }
        if strict && candidate.confidence < STRICT_CONFIDENCE {
            continue;
        }

        let key = (
            candidate.record.kind,
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

    for event in events {
        if strict && event.candidate.confidence < STRICT_CONFIDENCE {
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
    for item in additions {
        push_unique(existing, item);
    }
}

fn push_unique(target: &mut Vec<String>, value: String) {
    let key = normalize_key(&value);
    let mut seen = HashSet::new();
    for item in target.iter() {
        seen.insert(normalize_key(item));
    }
    if !seen.contains(&key) {
        target.push(value);
    }
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

const RESULT_STRICT_MARKERS: &[&str] = &[
    "passed",
    "failed",
    "error:",
    "score=",
    "score:",
    "latency",
    "p0=",
    "p1=",
    "p2=",
    "/10",
    "tests ",
    "clippy",
    "cargo test",
    "✓",
    "✗",
    "0 warnings",
    "0 errors",
];

pub fn classify_line_entry_type(line: &str, is_user: bool) -> Option<(EntryType, f32)> {
    let lower = line.to_lowercase();
    let trimmed = lower.trim();

    if trimmed.starts_with("decision:") || trimmed.contains("[decision]") {
        return Some((EntryType::Decision, 0.95));
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

    if ARGUE_MARKERS.iter().any(|m| lower.contains(m)) {
        return Some((EntryType::Argue, 0.6));
    }

    if WHY_MARKERS.iter().any(|m| lower.contains(m)) {
        return Some((EntryType::Why, 0.7));
    }

    if is_user && INTENT_KEYWORDS.iter().any(|kw| lower.contains(kw)) {
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

fn detect_supersedes(entries: &mut [IntentEntry]) {
    struct SupersedesAction {
        superseded_idx: usize,
        newer_idx: usize,
        superseded_id: String,
    }

    let mut topic_latest: HashMap<String, (usize, String, String)> = HashMap::new();
    let mut actions: Vec<SupersedesAction> = Vec::new();

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

        if let Some((prev_idx, prev_id, prev_date)) = topic_latest.get(&topic_key) {
            if entry.date > *prev_date
                || (entry.date == *prev_date && entry.confidence > entries[*prev_idx].confidence)
            {
                let old_idx = *prev_idx;
                let old_id = prev_id.clone();
                actions.push(SupersedesAction {
                    superseded_idx: old_idx,
                    newer_idx: idx,
                    superseded_id: old_id,
                });
                topic_latest.insert(topic_key, (idx, entry.id.clone(), entry.date.clone()));
            } else {
                actions.push(SupersedesAction {
                    superseded_idx: idx,
                    newer_idx: *prev_idx,
                    superseded_id: entry.id.clone(),
                });
            }
        } else {
            topic_latest.insert(topic_key, (idx, entry.id.clone(), entry.date.clone()));
        }
    }

    for action in actions {
        entries[action.superseded_idx].state = EntryState::Superseded;
        let already = entries[action.newer_idx]
            .links
            .iter()
            .any(|l| l.target == action.superseded_id);
        if !already {
            entries[action.newer_idx].links.push(Link {
                relation: LinkType::Supersedes,
                target: action.superseded_id,
                confidence: Some(0.7),
            });
        }
    }
}

fn detect_contradicted_assumptions(entries: &mut [IntentEntry]) {
    let assumptions: Vec<(usize, String)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.entry_type == EntryType::Assumption)
        .map(|(i, e)| (i, normalize_key(&e.title)))
        .collect();

    let results: Vec<(usize, String)> = entries
        .iter()
        .enumerate()
        .filter(|(_, e)| e.entry_type == EntryType::Result)
        .map(|(i, e)| (i, e.title.to_lowercase()))
        .collect();

    let contradiction_words = ["fail", "broken", "wrong", "error", "invalid", "rejected"];

    for (a_idx, a_key) in &assumptions {
        let a_words: HashSet<&str> = a_key.split_whitespace().collect();
        for (r_idx, r_title) in &results {
            if !contradiction_words.iter().any(|w| r_title.contains(w)) {
                continue;
            }
            let r_words: HashSet<&str> = r_title.split_whitespace().collect();
            let overlap = a_words.intersection(&r_words).count();
            if overlap >= 2 {
                entries[*a_idx].state = EntryState::Contradicted;
                let r_id = entries[*r_idx].id.clone();
                entries[*a_idx].links.push(Link {
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

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub total_chunks: usize,
    pub entries_found: usize,
    pub per_type: HashMap<String, usize>,
    pub per_project: HashMap<String, usize>,
    pub unresolved_count: usize,
}

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
        None,
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
mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    fn chunk_path(root: &Path, project: &str, date: &str, name: &str) -> PathBuf {
        let date_compact = crate::store::compact_date(date);
        let agent = if name.contains("_claude") || name.contains("claude") {
            "claude"
        } else if name.contains("_gemini") || name.contains("gemini") {
            "gemini"
        } else {
            "codex"
        };
        let sequence = name
            .trim_end_matches(".md")
            .rsplit_once('-')
            .and_then(|(_, tail)| tail.parse::<u32>().ok())
            .unwrap_or(1);
        let basename = crate::store::session_basename(date, agent, "intentstest01", sequence);
        let dir = root
            .join("store")
            .join("local")
            .join(project)
            .join(date_compact)
            .join("conversations")
            .join(agent);
        fs::create_dir_all(&dir).expect("create chunk dir");
        dir.join(basename)
    }

    fn write_chunk(root: &Path, project: &str, date: &str, name: &str, body: &str) {
        let path = chunk_path(root, project, date, name);
        fs::write(path, body).expect("write chunk");
    }

    fn migration_test_root(label: &str) -> PathBuf {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("unix time")
            .as_nanos();
        std::env::temp_dir().join(format!("aicx-intents-{label}-{nanos}"))
    }

    #[test]
    fn migrate_intent_schema_dry_run_at_scans_all_projects_without_filter() {
        let root = migration_test_root("intent-migration-all-projects");
        write_chunk(
            &root,
            "alpha",
            "2026-04-14",
            "093000_claude-001.md",
            "[09:30:00] assistant: result: alpha migration passed\n",
        );
        write_chunk(
            &root,
            "beta",
            "2026-04-15",
            "101500_codex-001.md",
            "[10:15:00] user: question: should beta keep legacy links?\n",
        );

        let report = migrate_intent_schema_dry_run_at(&root.join("store"), None)
            .expect("global migration dry run should work");

        assert_eq!(report.total_chunks, 2);
        assert_eq!(report.entries_found, 2);
        assert_eq!(report.per_project.get("alpha"), Some(&1));
        assert_eq!(report.per_project.get("beta"), Some(&1));

        let _ = fs::remove_dir_all(root);
    }

    fn write_chunk_with_sidecar(
        root: &Path,
        project: &str,
        date: &str,
        name: &str,
        body: &str,
        frame_kind: Option<FrameKind>,
    ) {
        let path = chunk_path(root, project, date, name);
        fs::write(&path, body).expect("write chunk");
        let sidecar = crate::chunker::ChunkMetadataSidecar {
            id: path
                .file_stem()
                .expect("chunk file stem")
                .to_string_lossy()
                .to_string(),
            project: format!("local/{project}"),
            agent: if name.contains("_claude") || name.contains("claude") {
                "claude".to_string()
            } else if name.contains("_gemini") || name.contains("gemini") {
                "gemini".to_string()
            } else {
                "codex".to_string()
            },
            date: date.to_string(),
            session_id: "intentstest01".to_string(),
            cwd: None,
            kind: crate::store::Kind::Conversations,
            run_id: None,
            prompt_id: None,
            frame_kind,
            agent_model: None,
            started_at: None,
            completed_at: None,
            token_usage: None,
            findings_count: None,
            workflow_phase: None,
            mode: None,
            skill_code: None,
            framework_version: None,
            intent_entries: Vec::new(),
        };
        fs::write(
            path.with_extension("meta.json"),
            serde_json::to_vec_pretty(&sidecar).expect("serialize sidecar"),
        )
        .expect("write sidecar");
    }

    #[test]
    fn extracts_and_dedups_signal_records() {
        let tmp = std::env::temp_dir().join(format!(
            "ai-contexters-intents-{}-signals",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);

        let chunk_one = r#"[project: demo | agent: codex | date: 2026-03-15]

[signals]
Decision:
- [decision] Reuse normalize_key from src/chunker.rs:508 for overlap dedup
Intent:
- Let's ship the intention engine this week
Outcome:
- [skill_outcome] p0=0 after cargo test
RED LIGHT: checklist detected (open: 1, done: 0)
- [ ] wire CLI
[/signals]

[12:00:00] user: Let's ship the intention engine this week
[12:01:00] assistant: [decision] Reuse normalize_key from src/chunker.rs:508 for overlap dedup
[12:02:00] assistant: [skill_outcome] p0=0 after cargo test
"#;

        let chunk_two = r#"[project: demo | agent: codex | date: 2026-03-15]

[signals]
Decision:
- [decision] Reuse normalize_key from src/chunker.rs:508 for overlap dedup
Outcome:
- outcome: p0=0 after cargo test
RED LIGHT: checklist detected (open: 0, done: 1)
- [x] wire CLI
[/signals]

[12:05:00] assistant: outcome: p0=0 after cargo test
"#;

        write_chunk(&tmp, "demo", "2026-03-15", "120000_codex-001.md", chunk_one);
        write_chunk(&tmp, "demo", "2026-03-15", "120500_codex-002.md", chunk_two);

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 24,
            strict: false,
            kind_filter: None,
            frame_kind: None,
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 3, 15)
                .expect("date")
                .and_hms_opt(13, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

        assert_eq!(records.len(), 3);
        assert!(records.iter().any(|record| {
            record.kind == IntentKind::Decision
                && record.summary.contains("Reuse normalize_key")
                && record
                    .evidence
                    .iter()
                    .any(|item| item == "src/chunker.rs:508")
        }));
        assert!(records.iter().any(|record| {
            record.kind == IntentKind::Intent
                && record.summary == "Let's ship the intention engine this week"
        }));
        assert!(records.iter().any(|record| {
            record.kind == IntentKind::Outcome && record.summary.contains("p0=0")
        }));
        assert!(!records.iter().any(|record| record.kind == IntentKind::Task));
    }

    #[test]
    fn extracts_raw_lines_and_keeps_surviving_open_tasks() {
        let tmp =
            std::env::temp_dir().join(format!("ai-contexters-intents-{}-raw", std::process::id()));
        let _ = fs::remove_dir_all(&tmp);

        let chunk = r#"[project: demo | agent: claude | date: 2026-03-14]

[11:00:00] user: Proponuję uprościć parser chunków
Bo overlap robi bałagan.
[11:01:00] assistant: decision: keep parser flat around src/intents.rs:1
commit abcdef1 proves the old path was wrong.
[11:02:00] assistant: validation: p1=0 and score=9 after checks
[11:03:00] assistant: - [ ] add CLI polish
"#;

        write_chunk(&tmp, "demo", "2026-03-14", "110000_claude-001.md", chunk);

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 48,
            strict: false,
            kind_filter: None,
            frame_kind: None,
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 3, 15)
                .expect("date")
                .and_hms_opt(9, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");

        assert!(records.iter().any(|record| {
            record.kind == IntentKind::Intent
                && record.summary == "Proponuję uprościć parser chunków"
                && record
                    .context
                    .as_deref()
                    .is_some_and(|ctx| ctx.contains("Bo overlap robi bałagan"))
        }));
        assert!(records.iter().any(|record| {
            record.kind == IntentKind::Decision
                && record
                    .evidence
                    .iter()
                    .any(|item| item == "src/intents.rs:1")
                && record.evidence.iter().any(|item| item == "abcdef1")
        }));
        assert!(records.iter().any(|record| {
            record.kind == IntentKind::Outcome
                && record.evidence.iter().any(|item| item == "p1=0")
                && record.evidence.iter().any(|item| item == "score=9")
        }));
        assert!(records.iter().any(|record| {
            record.kind == IntentKind::Task && record.summary == "add CLI polish"
        }));
    }

    #[test]
    fn strict_mode_filters_heuristic_only_intents() {
        let tmp = std::env::temp_dir().join(format!(
            "ai-contexters-intents-{}-strict",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);

        let chunk = r#"[project: demo | agent: codex | date: 2026-03-15]

[12:00:00] user: Let's keep only the sharp path.
"#;

        write_chunk(&tmp, "demo", "2026-03-15", "120000_codex-001.md", chunk);

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 24,
            strict: true,
            kind_filter: None,
            frame_kind: None,
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 3, 15)
                .expect("date")
                .and_hms_opt(13, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
        assert!(records.is_empty());
    }

    #[test]
    fn frame_kind_filter_keeps_only_matching_chunks() {
        let tmp = std::env::temp_dir().join(format!(
            "ai-contexters-intents-{}-frame-kind",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&tmp);

        write_chunk_with_sidecar(
            &tmp,
            "demo",
            "2026-03-15",
            "120000_codex-001.md",
            "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:00:00] user: Let's keep only user intent truth.\n",
            Some(FrameKind::UserMsg),
        );
        write_chunk_with_sidecar(
            &tmp,
            "demo",
            "2026-03-15",
            "120100_codex-002.md",
            "[project: demo | agent: codex | date: 2026-03-15]\n\n[12:01:00] assistant: decision: assistant-only steering\n",
            Some(FrameKind::AgentReply),
        );

        let config = IntentsConfig {
            project: "demo".to_string(),
            hours: 24,
            strict: false,
            kind_filter: None,
            frame_kind: Some(FrameKind::UserMsg),
        };
        let now = DateTime::<Utc>::from_naive_utc_and_offset(
            NaiveDate::from_ymd_opt(2026, 3, 15)
                .expect("date")
                .and_hms_opt(13, 0, 0)
                .expect("time"),
            Utc,
        );

        let records = extract_intents_from_root_at(&config, &tmp, now).expect("extract intents");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].kind, IntentKind::Intent);
        assert_eq!(records[0].summary, "Let's keep only user intent truth.");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn formats_markdown_with_required_sections() {
        let records = vec![IntentRecord {
            kind: IntentKind::Decision,
            summary: "Keep the parser flat".to_string(),
            context: Some("It removes overlap bugs.".to_string()),
            evidence: vec!["src/intents.rs:42".to_string()],
            project: "demo".to_string(),
            agent: "codex".to_string(),
            date: "2026-03-15".to_string(),
            timestamp: None,
            session_id: "test".to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: "/tmp/demo/2026-03-15/120000_codex-001.md".to_string(),
        }];

        let markdown = format_intents_markdown(&records);
        assert!(markdown.contains("DECISION: Keep the parser flat"));
        assert!(markdown.contains("WHY: It removes overlap bugs."));
        assert!(markdown.contains("EVIDENCE:"));
        assert!(markdown.contains("source_chunk: /tmp/demo/2026-03-15/120000_codex-001.md"));
    }

    #[test]
    fn formats_json_with_same_fields() {
        let records = vec![IntentRecord {
            kind: IntentKind::Outcome,
            summary: "p0=0 after validation".to_string(),
            context: None,
            evidence: vec!["p0=0".to_string()],
            project: "demo".to_string(),
            agent: "claude".to_string(),
            date: "2026-03-15".to_string(),
            timestamp: None,
            session_id: "test".to_string(),
            count: None,
            first_chunk: None,
            last_chunk: None,
            source_chunk: "/tmp/demo/2026-03-15/120500_claude-002.md".to_string(),
        }];

        let json = format_intents_json(&records).expect("serialize intents");
        assert!(json.contains("\"kind\": \"outcome\""));
        assert!(json.contains("\"summary\": \"p0=0 after validation\""));
        assert!(json.contains("\"source_chunk\": \"/tmp/demo/2026-03-15/120500_claude-002.md\""));
    }

    #[test]
    fn strip_case_prefix_is_utf8_safe() {
        let text = "Działa pięknie — pełny artifact pack z Rust flow...";
        assert_eq!(strip_case_insensitive_prefix(text, "validation:"), text);
    }

    // ── classifier tests ────────────────────────────────────────────

    mod classifier {
        use super::*;
        use crate::types::{EntryState, EntryType};

        #[test]
        fn classifies_decision_marker() {
            let result = classify_line_entry_type("[decision] Use flat parser", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Decision));
        }

        #[test]
        fn classifies_decision_prefix() {
            let result = classify_line_entry_type("decision: keep normalize_key", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Decision));
        }

        #[test]
        fn classifies_question_mark() {
            let result =
                classify_line_entry_type("How does the auth middleware handle sessions?", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Question));
        }

        #[test]
        fn classifies_question_prefix() {
            let result = classify_line_entry_type("question: can we reuse normalize_key?", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Question));
        }

        #[test]
        fn classifies_assumption() {
            let result =
                classify_line_entry_type("assumption: the store root is always ~/.aicx", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Assumption));
        }

        #[test]
        fn classifies_polish_assumption() {
            let result = classify_line_entry_type("zakładam że ścieżka zawsze istnieje", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Assumption));
        }

        #[test]
        fn classifies_insight_marker() {
            let result = classify_line_entry_type(
                "insight: aicx is an intention engine not a formatter",
                false,
            );
            assert_eq!(result.map(|r| r.0), Some(EntryType::Insight));
        }

        #[test]
        fn classifies_result_marker() {
            let result = classify_line_entry_type("result: latency 450ms p99", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Result));
        }

        #[test]
        fn classifies_result_from_keywords() {
            let result = classify_line_entry_type("tests 276/276 passed, 0 warnings", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Result));
        }

        #[test]
        fn classifies_outcome_tag() {
            let result = classify_line_entry_type("[skill_outcome] p0=0 after cargo test", false);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Outcome));
        }

        #[test]
        fn classifies_why_marker() {
            let result = classify_line_entry_type(
                "because the old auth middleware stores tokens wrong",
                false,
            );
            assert_eq!(result.map(|r| r.0), Some(EntryType::Why));
        }

        #[test]
        fn classifies_argue_marker() {
            let result = classify_line_entry_type(
                "on the other hand, rewriting is cheaper than patching",
                false,
            );
            assert_eq!(result.map(|r| r.0), Some(EntryType::Argue));
        }

        #[test]
        fn classifies_user_intent() {
            let result =
                classify_line_entry_type("Let's ship the intention engine this week", true);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Intent));
        }

        #[test]
        fn classifies_polish_user_intent() {
            let result = classify_line_entry_type("Proponuję uprościć parser chunków", true);
            assert_eq!(result.map(|r| r.0), Some(EntryType::Intent));
        }

        #[test]
        fn abstains_on_ambiguous_line() {
            let result = classify_line_entry_type("some random code comment", false);
            assert!(result.is_none());
        }

        #[test]
        fn abstains_on_short_question() {
            let result = classify_line_entry_type("what?", false);
            assert!(result.is_none());
        }

        #[test]
        fn classify_chunk_all_nine_types() {
            let content = r#"[project: demo | agent: claude | date: 2026-04-15]

[signals]
Decision:
- [decision] Use 9-type taxonomy
Intent:
- Let's ship intent engine
Outcome:
- outcome: migration completed successfully
[/signals]

[12:00:00] user: Proponuję dodać link graph
[12:01:00] assistant: assumption: store root always at ~/.aicx
[12:02:00] assistant: insight: aicx is an intention retrieval engine
[12:03:00] assistant: result: tests 276/276 passed
[12:04:00] user: How does the chunker handle overlap?
[12:05:00] assistant: because the old approach created duplicates
[12:06:00] assistant: on the other hand, flat parsing is simpler
"#;

            let entries = classify_chunk_entries(
                content,
                "/tmp/test/chunk-001.md",
                Some("demo"),
                Some("claude"),
                Some("sess-01"),
                "2026-04-15",
            );

            let types: HashSet<EntryType> = entries.iter().map(|e| e.entry_type).collect();
            assert!(types.contains(&EntryType::Decision), "missing Decision");
            assert!(types.contains(&EntryType::Outcome), "missing Outcome");
            assert!(types.contains(&EntryType::Intent), "missing Intent");
            assert!(types.contains(&EntryType::Assumption), "missing Assumption");
            assert!(types.contains(&EntryType::Insight), "missing Insight");
            assert!(types.contains(&EntryType::Result), "missing Result");
            assert!(types.contains(&EntryType::Question), "missing Question");
            assert!(types.contains(&EntryType::Why), "missing Why");
            assert!(types.contains(&EntryType::Argue), "missing Argue");

            for entry in &entries {
                assert!(!entry.id.is_empty());
                assert!(!entry.title.is_empty());
                assert!(entry.confidence >= CLASSIFIER_ABSTAIN_THRESHOLD);
                assert_eq!(entry.date, "2026-04-15");
                assert_eq!(entry.source_chunk, "/tmp/test/chunk-001.md");
            }
        }

        #[test]
        fn stable_ids_are_deterministic() {
            let content = "[12:00:00] user: Let's ship intent engine\n";
            let a = classify_chunk_entries(content, "/chunk.md", None, None, None, "2026-04-15");
            let b = classify_chunk_entries(content, "/chunk.md", None, None, None, "2026-04-15");
            assert_eq!(a.len(), b.len());
            for (x, y) in a.iter().zip(b.iter()) {
                assert_eq!(x.id, y.id);
            }
        }

        #[test]
        fn tags_are_inferred() {
            let content = "[12:00:00] assistant: result: auth login tests passed\n";
            let entries = classify_chunk_entries(content, "/c.md", None, None, None, "2026-04-15");
            let result_entry = entries.iter().find(|e| e.entry_type == EntryType::Result);
            assert!(result_entry.is_some());
            let tags = &result_entry.unwrap().tags;
            assert!(tags.contains(&"auth".to_string()) || tags.contains(&"testing".to_string()));
        }

        #[test]
        fn initial_state_mapping() {
            assert_eq!(initial_state(EntryType::Intent), EntryState::Proposed);
            assert_eq!(initial_state(EntryType::Question), EntryState::Proposed);
            assert_eq!(initial_state(EntryType::Assumption), EntryState::Proposed);
            assert_eq!(initial_state(EntryType::Decision), EntryState::Active);
            assert_eq!(initial_state(EntryType::Insight), EntryState::Active);
            assert_eq!(initial_state(EntryType::Outcome), EntryState::Done);
            assert_eq!(initial_state(EntryType::Result), EntryState::Done);
            assert_eq!(initial_state(EntryType::Why), EntryState::Active);
            assert_eq!(initial_state(EntryType::Argue), EntryState::Active);
        }
    }

    mod session_level {
        use super::*;
        use crate::types::{EntryState, EntryType};

        fn make_entry(
            entry_type: EntryType,
            title: &str,
            date: &str,
            session_id: &str,
            project: &str,
        ) -> IntentEntry {
            IntentEntry {
                id: IntentEntry::stable_id(title, 0, entry_type),
                entry_type,
                state: initial_state(entry_type),
                title: title.to_string(),
                body: None,
                evidence: Vec::new(),
                links: Vec::new(),
                confidence: 0.9,
                tags: Vec::new(),
                project: Some(project.to_string()),
                agent: Some("claude".to_string()),
                session_id: Some(session_id.to_string()),
                timestamp: None,
                date: date.to_string(),
                source_chunk: "/test/chunk.md".to_string(),
            }
        }

        #[test]
        fn supersedes_marks_older_entry() {
            let mut entries = vec![
                make_entry(
                    EntryType::Intent,
                    "ship the new intent engine soon with basic features",
                    "2026-04-10",
                    "s1",
                    "demo",
                ),
                make_entry(
                    EntryType::Intent,
                    "ship the new intent engine soon with full taxonomy",
                    "2026-04-15",
                    "s2",
                    "demo",
                ),
            ];

            postprocess_session_entries(&mut entries, Some(30));

            assert_eq!(entries[0].state, EntryState::Superseded);
            assert_eq!(entries[1].state, EntryState::Proposed);
            assert!(
                entries[1]
                    .links
                    .iter()
                    .any(|l| l.relation == LinkType::Supersedes)
            );
        }

        #[test]
        fn contradicted_assumption() {
            let mut entries = vec![
                make_entry(
                    EntryType::Assumption,
                    "store root always exists",
                    "2026-04-10",
                    "s1",
                    "demo",
                ),
                make_entry(
                    EntryType::Result,
                    "store root failed validation error",
                    "2026-04-11",
                    "s1",
                    "demo",
                ),
            ];

            postprocess_session_entries(&mut entries, Some(30));

            assert_eq!(entries[0].state, EntryState::Contradicted);
            assert!(
                entries[0]
                    .links
                    .iter()
                    .any(|l| l.relation == LinkType::Contradicts)
            );
        }

        #[test]
        fn insight_links_to_sources() {
            let mut entries = vec![
                make_entry(
                    EntryType::Result,
                    "tests 276/276 passed",
                    "2026-04-15",
                    "s1",
                    "demo",
                ),
                make_entry(
                    EntryType::Outcome,
                    "migration complete",
                    "2026-04-15",
                    "s1",
                    "demo",
                ),
                make_entry(
                    EntryType::Insight,
                    "aicx is an intention engine",
                    "2026-04-15",
                    "s1",
                    "demo",
                ),
            ];

            postprocess_session_entries(&mut entries, Some(30));

            let insight = &entries[2];
            assert!(!insight.links.is_empty());
            assert!(
                insight
                    .links
                    .iter()
                    .all(|l| l.relation == LinkType::DerivedFrom)
            );
        }

        #[test]
        fn unresolved_intent_tagged_after_threshold() {
            let old_date = (chrono::Utc::now().date_naive() - chrono::Duration::days(10))
                .format("%Y-%m-%d")
                .to_string();
            let mut entries = vec![make_entry(
                EntryType::Intent,
                "implement dark mode",
                &old_date,
                "s-old",
                "demo",
            )];

            postprocess_session_entries(&mut entries, Some(7));

            assert!(entries[0].tags.contains(&"unresolved".to_string()));
        }

        #[test]
        fn recent_intent_not_tagged_unresolved() {
            let today = chrono::Utc::now()
                .date_naive()
                .format("%Y-%m-%d")
                .to_string();
            let mut entries = vec![make_entry(
                EntryType::Intent,
                "implement dark mode",
                &today,
                "s-today",
                "demo",
            )];

            postprocess_session_entries(&mut entries, Some(7));

            assert!(!entries[0].tags.contains(&"unresolved".to_string()));
        }
    }
}
