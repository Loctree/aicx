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
use crate::types::FrameKind;

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

    for file in store::scan_context_files_at(store_root)? {
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
}
