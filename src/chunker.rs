//! Semantic windowing chunker for RAG indexing.
//!
//! Splits timeline entries into overlapping windows of ~1.5k tokens,
//! suitable for vector embedding and semantic search via memex.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::output::TimelineEntry;

// ============================================================================
// Types
// ============================================================================

/// A single chunk ready for vector indexing.
#[derive(Debug, Clone)]
pub struct Chunk {
    /// Unique ID: `{project}_{agent}_{date}_{seq:03}`
    pub id: String,
    pub project: String,
    pub agent: String,
    /// Date string (YYYY-MM-DD)
    pub date: String,
    /// Session ID from first message in chunk
    pub session_id: String,
    /// Working directory from the first message in the chunk window
    pub cwd: Option<String>,
    /// Classified kind for this chunk's content
    pub kind: crate::store::Kind,
    /// Optional correlation ID for the originating run
    pub run_id: Option<String>,
    /// Optional prompt or task identity for the originating run
    pub prompt_id: Option<String>,
    /// Optional agent model reported by the source frontmatter
    pub agent_model: Option<String>,
    /// Optional run start timestamp reported by the source frontmatter
    pub started_at: Option<String>,
    /// Optional run completion timestamp reported by the source frontmatter
    pub completed_at: Option<String>,
    /// Optional token usage reported by the source frontmatter
    pub token_usage: Option<u64>,
    /// Optional findings count reported by the source frontmatter
    pub findings_count: Option<u32>,
    /// Optional workflow phase reported by the source frontmatter
    pub workflow_phase: Option<String>,
    /// Optional routing mode reported by the source frontmatter
    pub mode: Option<String>,
    /// Optional framework skill code reported by the source frontmatter
    pub skill_code: Option<String>,
    /// Optional steering schema/framework version reported by the source frontmatter
    pub framework_version: Option<String>,
    /// Index range in original day's entries (start, end exclusive)
    pub msg_range: (usize, usize),
    /// Formatted chunk text with header
    pub text: String,
    /// Estimated token count (~chars/4)
    pub token_estimate: usize,
    /// Decision/plan highlights extracted from the chunk
    pub highlights: Vec<String>,
}

/// Structured metadata sidecar persisted alongside each memex chunk file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkMetadataSidecar {
    pub id: String,
    pub project: String,
    pub agent: String,
    pub date: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub kind: crate::store::Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub findings_count: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workflow_phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skill_code: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub framework_version: Option<String>,
}

impl From<&Chunk> for ChunkMetadataSidecar {
    fn from(chunk: &Chunk) -> Self {
        Self {
            id: chunk.id.clone(),
            project: chunk.project.clone(),
            agent: chunk.agent.clone(),
            date: chunk.date.clone(),
            session_id: chunk.session_id.clone(),
            cwd: chunk.cwd.clone(),
            kind: chunk.kind,
            run_id: chunk.run_id.clone(),
            prompt_id: chunk.prompt_id.clone(),
            agent_model: chunk.agent_model.clone(),
            started_at: chunk.started_at.clone(),
            completed_at: chunk.completed_at.clone(),
            token_usage: chunk.token_usage,
            findings_count: chunk.findings_count,
            workflow_phase: chunk.workflow_phase.clone(),
            mode: chunk.mode.clone(),
            skill_code: chunk.skill_code.clone(),
            framework_version: chunk.framework_version.clone(),
        }
    }
}

/// Configuration for the chunker.
#[derive(Debug, Clone)]
pub struct ChunkerConfig {
    /// Target tokens per chunk (default: 1500)
    pub target_tokens: usize,
    /// Minimum tokens — don't create tiny chunks unless it's the last window (default: 500)
    pub min_tokens: usize,
    /// Maximum tokens — force split if exceeded (default: 2500)
    pub max_tokens: usize,
    /// Number of messages to overlap between consecutive windows (default: 2)
    pub overlap_messages: usize,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            target_tokens: 1500,
            min_tokens: 500,
            max_tokens: 2500,
            overlap_messages: 2,
        }
    }
}

// ============================================================================
// Token estimation
// ============================================================================

/// Estimate token count from text length.
///
/// Uses the simple heuristic: 1 token ≈ 4 characters.
/// Rounds up to avoid underestimation.
pub fn estimate_tokens(text: &str) -> usize {
    text.len().div_ceil(4)
}

fn prepare_entries_for_chunking<'a>(
    entries: &'a [TimelineEntry],
) -> (
    Option<crate::frontmatter::ReportFrontmatter>,
    Cow<'a, [TimelineEntry]>,
) {
    let Some(first) = entries.first() else {
        return (None, Cow::Borrowed(entries));
    };

    if !first.message.trim_start().starts_with("---") {
        return (None, Cow::Borrowed(entries));
    }

    let (frontmatter, body) = crate::frontmatter::parse(&first.message);
    if body == first.message {
        return (None, Cow::Borrowed(entries));
    }

    let mut stripped_entries = entries.to_vec();
    if let Some(stripped_first) = stripped_entries.first_mut() {
        stripped_first.message = body.to_string();
    }

    (frontmatter, Cow::Owned(stripped_entries))
}

fn apply_frontmatter(chunk: &mut Chunk, frontmatter: &crate::frontmatter::ReportFrontmatter) {
    chunk.run_id = frontmatter.telemetry.run_id.clone();
    chunk.prompt_id = frontmatter.telemetry.prompt_id.clone();
    chunk.agent_model = frontmatter.telemetry.model.clone();
    chunk.started_at = frontmatter.telemetry.started_at.clone();
    chunk.completed_at = frontmatter.telemetry.completed_at.clone();
    chunk.token_usage = frontmatter.telemetry.token_usage;
    chunk.findings_count = frontmatter.telemetry.findings_count;
    chunk.workflow_phase = frontmatter.steering.workflow_phase.clone();
    chunk.mode = frontmatter.steering.mode.clone();
    chunk.skill_code = frontmatter.steering.skill_code.clone();
    chunk.framework_version = frontmatter.steering.framework_version.clone();
}

// ============================================================================
// Chunking logic
// ============================================================================

/// Chunk timeline entries into semantic windows with overlap.
///
/// Groups entries by date, then applies sliding window within each day.
/// Returns chunks sorted by date and sequence number.
pub fn chunk_entries(
    entries: &[TimelineEntry],
    project: &str,
    agent: &str,
    config: &ChunkerConfig,
) -> Vec<Chunk> {
    if entries.is_empty() {
        return vec![];
    }

    let (frontmatter, prepared_entries) = prepare_entries_for_chunking(entries);
    let prepared_entries = prepared_entries.as_ref();

    // Group entries by date
    let mut by_date: BTreeMap<String, Vec<(usize, &TimelineEntry)>> = BTreeMap::new();
    for (idx, entry) in prepared_entries.iter().enumerate() {
        let date = entry.timestamp.format("%Y-%m-%d").to_string();
        by_date.entry(date).or_default().push((idx, entry));
    }

    let mut chunks = Vec::new();

    for (date, day_entries) in &by_date {
        let mut day_chunks = chunk_day_entries(day_entries, project, agent, date, config);
        if let Some(frontmatter) = frontmatter.as_ref() {
            for chunk in &mut day_chunks {
                apply_frontmatter(chunk, frontmatter);
            }
        }
        chunks.extend(day_chunks);
    }

    chunks
}

/// Apply sliding window chunking to a single day's entries.
fn chunk_day_entries(
    entries: &[(usize, &TimelineEntry)],
    project: &str,
    agent: &str,
    date: &str,
    config: &ChunkerConfig,
) -> Vec<Chunk> {
    if entries.is_empty() {
        return vec![];
    }

    let mut chunks = Vec::new();
    let mut seq = 1usize;
    let mut start = 0usize;

    while start < entries.len() {
        // Find window end: accumulate until target_tokens reached
        let mut end = start;
        let mut accumulated_tokens = 0usize;

        while end < entries.len() {
            let msg_tokens = estimate_tokens(&entries[end].1.message);
            let next_total = accumulated_tokens + msg_tokens + 20; // ~20 tokens for timestamp/role header

            if next_total > config.max_tokens && end > start {
                break;
            }

            accumulated_tokens = next_total;
            end += 1;

            if accumulated_tokens >= config.target_tokens {
                break;
            }
        }

        // Build chunk from entries[start..end]
        let window: Vec<&TimelineEntry> = entries[start..end].iter().map(|(_, e)| *e).collect();
        let highlights = extract_highlights(&window);
        let signals = extract_signals(&window);
        let text = format_chunk_text_inner(&window, project, agent, date, &signals, &highlights);
        let token_estimate = estimate_tokens(&text);

        let session_id = window
            .first()
            .map(|e| e.session_id.clone())
            .unwrap_or_default();
        let cwd = window.first().and_then(|entry| entry.cwd.clone());

        let global_start = entries[start].0;
        let global_end = entries[end - 1].0 + 1;

        let kind =
            crate::store::classify_kind(&window.iter().map(|e| (*e).clone()).collect::<Vec<_>>());

        chunks.push(Chunk {
            id: format!("{}_{}_{}_{{:03}}", project, agent, date)
                .replace("{:03}", &format!("{:03}", seq)),
            project: project.to_string(),
            agent: agent.to_string(),
            date: date.to_string(),
            session_id,
            cwd,
            kind,
            run_id: None,
            prompt_id: None,
            agent_model: None,
            started_at: None,
            completed_at: None,
            token_usage: None,
            findings_count: None,
            workflow_phase: None,
            mode: None,
            skill_code: None,
            framework_version: None,
            msg_range: (global_start, global_end),
            text,
            token_estimate,
            highlights,
        });

        seq += 1;

        // Next window starts at (end - overlap), but always advance at least 1
        let overlap = config.overlap_messages.min(end - start);
        let next_start = if end >= entries.len() {
            entries.len() // done
        } else if end - overlap > start {
            end - overlap
        } else {
            end // avoid infinite loop
        };

        start = next_start;
    }

    chunks
}

/// Format entries into chunk text with metadata header.
pub fn format_chunk_text(
    entries: &[&TimelineEntry],
    project: &str,
    agent: &str,
    date: &str,
) -> String {
    let highlights = extract_highlights(entries);
    let signals = extract_signals(entries);
    format_chunk_text_inner(entries, project, agent, date, &signals, &highlights)
}

fn format_chunk_text_inner(
    entries: &[&TimelineEntry],
    project: &str,
    agent: &str,
    date: &str,
    signals: &ChunkSignals,
    highlights: &[String],
) -> String {
    let mut text = format!(
        "[project: {} | agent: {} | date: {}]\n\n",
        project, agent, date
    );

    if let Some(block) = format_signals_block(signals, highlights) {
        text.push_str(&block);
        text.push('\n');
    }

    for entry in entries {
        let time = entry.timestamp.format("%H:%M:%S");
        // Truncate very long messages to avoid monster chunks (UTF-8 safe).
        let msg = if entry.message.len() > 4000 {
            truncate_message_bytes(&entry.message, 4000)
        } else {
            entry.message.clone()
        };
        text.push_str(&format!("[{}] {}: {}\n", time, entry.role, msg));
    }

    text
}

const HIGHLIGHT_KEYWORDS: &[&str] = &[
    "decision:",
    "plan:",
    "architecture",
    "breaking",
    "todo:",
    "fixme:",
];

const HIGHLIGHT_KEYWORDS_CASE_SENSITIVE: &[&str] = &["WAŻNE", "KEY"];

fn extract_highlights(entries: &[&TimelineEntry]) -> Vec<String> {
    let mut highlights = Vec::new();
    for entry in entries {
        if highlights.len() >= 3 {
            break;
        }
        if !is_highlight_message(&entry.message) {
            continue;
        }

        if let Some(line) = entry.message.lines().map(str::trim).find(|l| !l.is_empty())
            && highlights.last().map(String::as_str) != Some(line)
        {
            highlights.push(line.to_string());
        }
    }
    highlights
}

fn is_highlight_message(message: &str) -> bool {
    let lower = message.to_lowercase();
    HIGHLIGHT_KEYWORDS.iter().any(|kw| lower.contains(kw))
        || HIGHLIGHT_KEYWORDS_CASE_SENSITIVE
            .iter()
            .any(|kw| message.contains(kw))
}

// ============================================================================
// Signals (intent + checklists)
// ============================================================================

#[derive(Debug, Clone, Default)]
struct ChunkSignals {
    todo_open: Vec<String>,
    todo_done: Vec<String>,
    ultrathink: Vec<String>,
    insights: Vec<String>,
    plan_mode: Vec<String>,
    intents: Vec<String>,
    results: Vec<String>,
    skills: Vec<String>,
    decisions: Vec<String>,
    outcomes: Vec<String>,
}

const MAX_TODO_ITEMS: usize = 8;
const MAX_ULTRATHINK_BLOCKS: usize = 4;
const MAX_INSIGHT_BLOCKS: usize = 6;
const MAX_PLAN_MODE_EVENTS: usize = 8;
const MAX_INTENT_LINES: usize = 6;
const MAX_RESULT_LINES: usize = 6;
const MAX_TAG_BLOCK_LINES: usize = 4;

pub(crate) const INTENT_KEYWORDS: &[&str] = &[
    // Polish
    "mam pomysl",
    "mam pomysł",
    "mam taki pomysl",
    "mam taki pomysł",
    "pomysl",
    "pomysł",
    "proponuje",
    "proponuję",
    "zrobmy",
    "zróbmy",
    "ustalmy",
    "ustalmy",
    "chce",
    "chcę",
    "chcialbym",
    "chciałbym",
    "potrzebuje",
    "potrzebuję",
    "następny krok",
    "nastepny krok",
    "kolejny krok",
    // English
    "i want",
    "i'd like",
    "let's",
    "next step",
];

const RESULT_KEYWORDS: &[&str] = &[
    "smoke test",
    "passed",
    "all checks passed",
    "0 failed",
    "completed",
    "done",
    "zrobione",
    "dowiezione",
    "gotowe",
    "dziala",
    "działa",
];

fn extract_signals(entries: &[&TimelineEntry]) -> ChunkSignals {
    let (todo_open, todo_done) = extract_checklist_items(entries);
    let ultrathink = extract_tag_blocks(entries, is_ultrathink_tag, MAX_ULTRATHINK_BLOCKS);
    let insights = extract_tag_blocks(entries, is_insight_tag, MAX_INSIGHT_BLOCKS);
    let plan_mode = extract_tag_blocks(entries, is_plan_mode_tag, MAX_PLAN_MODE_EVENTS);
    let intents = extract_intent_lines(entries);
    let results = extract_result_lines(entries);
    let skills = extract_tag_blocks(entries, is_skill_tag, 4);
    let decisions = extract_tag_blocks(entries, is_decision_tag, 4);
    let outcomes = extract_tag_blocks(entries, is_outcome_tag, 4);

    ChunkSignals {
        todo_open,
        todo_done,
        ultrathink,
        insights,
        plan_mode,
        intents,
        results,
        skills,
        decisions,
        outcomes,
    }
}

fn extract_checklist_items(entries: &[&TimelineEntry]) -> (Vec<String>, Vec<String>) {
    #[derive(Debug, Clone, Copy)]
    enum TaskState {
        Open,
        Done,
    }

    let mut state_by_key: HashMap<String, TaskState> = HashMap::new();
    let mut display_by_key: HashMap<String, String> = HashMap::new();
    let mut order: Vec<String> = Vec::new();

    for entry in entries {
        for line in entry.message.lines() {
            if let Some((is_done, task)) = parse_checklist_task(line) {
                let key = normalize_key(&task);
                if !state_by_key.contains_key(&key) {
                    order.push(key.clone());
                    display_by_key.insert(key.clone(), task);
                    state_by_key.insert(key.clone(), TaskState::Open);
                }

                // Once a task is marked done anywhere, keep it done.
                if is_done {
                    state_by_key.insert(key, TaskState::Done);
                }
            }
        }
    }

    let mut open = Vec::new();
    let mut done = Vec::new();
    for key in order {
        let Some(task) = display_by_key.get(&key) else {
            continue;
        };
        match state_by_key.get(&key) {
            Some(TaskState::Done) => done.push(task.clone()),
            Some(TaskState::Open) => open.push(task.clone()),
            None => {}
        }
    }

    (open, done)
}

pub(crate) fn parse_checklist_task(line: &str) -> Option<(bool, String)> {
    let l = line.trim_start();
    let mut chars = l.chars();
    let bullet = chars.next()?;
    if !matches!(bullet, '-' | '*' | '+') {
        return None;
    }
    let rest = chars.as_str().trim_start();
    let rest = rest.strip_prefix('[')?;
    let mut chars = rest.chars();
    let state = chars.next()?;
    let rest = chars.as_str();
    let rest = rest.strip_prefix(']')?;
    let task = rest.trim_start();
    if task.is_empty() {
        return None;
    }

    match state {
        'x' | 'X' => Some((true, task.trim().to_string())),
        ' ' => Some((false, task.trim().to_string())),
        _ => None,
    }
}

fn extract_intent_lines(entries: &[&TimelineEntry]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for entry in entries {
        if entry.role.to_lowercase() != "user" {
            continue;
        }
        for line in entry.message.lines().map(str::trim) {
            if line.is_empty() {
                continue;
            }
            if !is_intent_line(line) {
                continue;
            }

            let key = normalize_key(line);
            if !seen.insert(key) {
                continue;
            }

            out.push(truncate_signal_line(line));
            if out.len() >= MAX_INTENT_LINES {
                return out;
            }
        }
    }

    out
}

pub(crate) fn is_intent_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    INTENT_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

fn extract_result_lines(entries: &[&TimelineEntry]) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for entry in entries {
        for line in entry.message.lines().map(str::trim) {
            if line.is_empty() {
                continue;
            }
            if !is_result_line(line) {
                continue;
            }
            let key = normalize_key(line);
            if !seen.insert(key) {
                continue;
            }
            out.push(truncate_signal_line(line));
            if out.len() >= MAX_RESULT_LINES {
                return out;
            }
        }
    }

    out
}

pub(crate) fn is_result_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    RESULT_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

pub(crate) fn normalize_key(s: &str) -> String {
    s.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub(crate) fn truncate_signal_line(line: &str) -> String {
    const MAX_BYTES: usize = 240;
    if line.len() <= MAX_BYTES {
        return line.to_string();
    }
    truncate_message_bytes(line, MAX_BYTES)
}

fn is_ultrathink_tag(line: &str) -> bool {
    line.to_lowercase().contains("ultrathink")
}

fn is_insight_tag(line: &str) -> bool {
    let lower = line.to_lowercase();
    // Prefer common "tag" forms like "Insight:" / "★ Insight" / "Insight ─".
    lower.starts_with("insight")
        || lower.contains("★ insight")
        || lower.contains("insight ─")
        || lower.contains("insight -")
}

fn is_plan_mode_tag(line: &str) -> bool {
    let lower = line.to_lowercase();
    // Capture Plan Mode session transitions + explicit accept/approval actions.
    lower.contains("plan mode")
        || lower.contains("accept plan")
        || lower.contains("user accepted the plan")
        || lower.contains("approve and bypass permissions")
        || lower.contains("bypass permissions")
}

fn is_skill_tag(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("[skill_enter]")
        || lower.contains("vetcoders-partner")
        || lower.contains("vetcoders-spawn")
        || lower.contains("vetcoders-ownership")
        || lower.contains("vetcoders-workflow")
}

pub(crate) fn is_decision_tag(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("[decision]") || lower.starts_with("decision:")
}

pub(crate) fn is_outcome_tag(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("[skill_outcome]")
        || lower.starts_with("outcome:")
        || lower.starts_with("validation:")
}

fn extract_tag_blocks(
    entries: &[&TimelineEntry],
    is_tag: fn(&str) -> bool,
    max_blocks: usize,
) -> Vec<String> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    for entry in entries {
        let lines: Vec<&str> = entry.message.lines().collect();
        for (i, raw) in lines.iter().enumerate() {
            let line = raw.trim();
            if line.is_empty() || !is_tag(line) {
                continue;
            }

            let mut block = Vec::new();
            block.push(line);

            for raw_next in lines.iter().skip(i + 1) {
                let next = raw_next.trim();
                if next.is_empty() {
                    break;
                }
                if is_tag(next) {
                    break;
                }
                block.push(next);
                if block.len() >= MAX_TAG_BLOCK_LINES {
                    break;
                }
            }

            let joined = block.join(" ");
            let key = normalize_key(&joined);
            if !seen.insert(key) {
                continue;
            }

            out.push(truncate_signal_line(&joined));
            if out.len() >= max_blocks {
                return out;
            }
        }
    }

    out
}

fn format_signals_block(signals: &ChunkSignals, highlights: &[String]) -> Option<String> {
    let has_any = !signals.todo_open.is_empty()
        || !signals.todo_done.is_empty()
        || !signals.ultrathink.is_empty()
        || !signals.insights.is_empty()
        || !signals.plan_mode.is_empty()
        || !signals.intents.is_empty()
        || !signals.results.is_empty()
        || !signals.skills.is_empty()
        || !signals.decisions.is_empty()
        || !signals.outcomes.is_empty()
        || !highlights.is_empty();
    if !has_any {
        return None;
    }

    let mut out = String::new();
    out.push_str("[signals]\n");

    if !signals.skills.is_empty() {
        out.push_str("=== SKILL ENTER ===\n");
        for line in &signals.skills {
            out.push_str(&format!("{}\n", line));
        }
        out.push_str("===================\n");
    }

    if !signals.todo_open.is_empty() || !signals.todo_done.is_empty() {
        if !signals.todo_open.is_empty() {
            out.push_str(&format!(
                "RED LIGHT: checklist detected (open: {}, done: {})\n",
                signals.todo_open.len(),
                signals.todo_done.len()
            ));
        } else {
            out.push_str(&format!(
                "Checklist detected (open: 0, done: {})\n",
                signals.todo_done.len()
            ));
        }

        for task in signals.todo_open.iter().take(MAX_TODO_ITEMS) {
            out.push_str(&format!("- [ ] {}\n", task));
        }
        if signals.todo_open.len() > MAX_TODO_ITEMS {
            out.push_str(&format!(
                "... (+{} more open)\n",
                signals.todo_open.len() - MAX_TODO_ITEMS
            ));
        }

        for task in signals.todo_done.iter().take(MAX_TODO_ITEMS) {
            out.push_str(&format!("- [x] {}\n", task));
        }
        if signals.todo_done.len() > MAX_TODO_ITEMS {
            out.push_str(&format!(
                "... (+{} more done)\n",
                signals.todo_done.len() - MAX_TODO_ITEMS
            ));
        }
    }

    if !signals.ultrathink.is_empty() {
        out.push_str("Ultrathink:\n");
        for line in &signals.ultrathink {
            out.push_str(&format!("- {}\n", line));
        }
    }

    if !signals.insights.is_empty() {
        out.push_str("Insight:\n");
        for line in &signals.insights {
            out.push_str(&format!("- {}\n", line));
        }
    }

    if !signals.plan_mode.is_empty() {
        out.push_str("Plan mode:\n");
        for line in &signals.plan_mode {
            out.push_str(&format!("- {}\n", line));
        }
    }

    if !signals.intents.is_empty() {
        out.push_str("Intent:\n");
        for line in &signals.intents {
            out.push_str(&format!("- {}\n", line));
        }
    }

    if !signals.decisions.is_empty() {
        out.push_str("Decision:\n");
        for line in &signals.decisions {
            out.push_str(&format!("- {}\n", line));
        }
    }

    if !signals.results.is_empty() {
        out.push_str("Results:\n");
        for line in &signals.results {
            out.push_str(&format!("- {}\n", line));
        }
    }

    if !signals.outcomes.is_empty() {
        out.push_str("Outcome:\n");
        for line in &signals.outcomes {
            out.push_str(&format!("- {}\n", line));
        }
    }

    if !highlights.is_empty() {
        out.push_str("Notes:\n");
        for line in highlights {
            out.push_str(&format!("- {}\n", truncate_signal_line(line)));
        }
    }

    out.push_str("[/signals]\n");
    Some(out)
}

fn truncate_message_bytes(message: &str, max_bytes: usize) -> String {
    let mut cutoff = max_bytes.min(message.len());
    while cutoff > 0 && !message.is_char_boundary(cutoff) {
        cutoff -= 1;
    }
    let mut out = String::with_capacity(cutoff + 15);
    out.push_str(&message[..cutoff]);
    out.push_str("...[truncated]");
    out
}

// ============================================================================
// File output
// ============================================================================

/// Write chunks as individual .txt files to a directory.
///
/// Each file is named `{chunk.id}.txt`. Returns paths of written files.
pub fn write_chunks_to_dir(chunks: &[Chunk], dir: &Path) -> Result<Vec<PathBuf>> {
    fs::create_dir_all(dir)?;

    let mut paths = Vec::new();

    for chunk in chunks {
        let filename = format!("{}.txt", chunk.id);
        let path = dir.join(&filename);
        fs::write(&path, &chunk.text)?;
        let sidecar_path = dir.join(format!("{}.meta.json", chunk.id));
        let sidecar = ChunkMetadataSidecar::from(chunk);
        fs::write(&sidecar_path, serde_json::to_vec_pretty(&sidecar)?)?;
        paths.push(path);
    }

    Ok(paths)
}

/// Summary of chunking results.
pub fn chunk_summary(chunks: &[Chunk]) -> String {
    if chunks.is_empty() {
        return "No chunks generated.".to_string();
    }

    let total_tokens: usize = chunks.iter().map(|c| c.token_estimate).sum();
    let avg_tokens = total_tokens / chunks.len();
    let dates: Vec<&str> = chunks
        .iter()
        .map(|c| c.date.as_str())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect();

    format!(
        "{} chunks, {} total tokens (avg {}), {} days",
        chunks.len(),
        total_tokens,
        avg_tokens,
        dates.len(),
    )
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn make_entry(hour: u32, min: u32, role: &str, msg: &str) -> TimelineEntry {
        TimelineEntry {
            timestamp: Utc.with_ymd_and_hms(2026, 1, 22, hour, min, 0).unwrap(),
            agent: "claude".to_string(),
            session_id: "sess-1".to_string(),
            role: role.to_string(),
            message: msg.to_string(),
            branch: None,
            cwd: None,
        }
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("hi"), 1); // 2 chars → ceil(2/4) = 1
        assert_eq!(estimate_tokens("hello world"), 3); // 11 chars → ceil(11/4) = 3
        assert_eq!(estimate_tokens("1234"), 1); // exactly 4 chars = 1 token
        assert_eq!(estimate_tokens("12345"), 2); // 5 chars → 2 tokens
    }

    #[test]
    fn test_chunk_entries_empty() {
        let config = ChunkerConfig::default();
        let chunks = chunk_entries(&[], "proj", "claude", &config);
        assert!(chunks.is_empty());
    }

    #[test]
    fn test_chunk_entries_single_message() {
        let entries = vec![make_entry(14, 0, "user", "short message")];
        let config = ChunkerConfig::default();
        let chunks = chunk_entries(&entries, "proj", "claude", &config);

        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].project, "proj");
        assert_eq!(chunks[0].agent, "claude");
        assert_eq!(chunks[0].date, "2026-01-22");
        assert!(chunks[0].text.contains("short message"));
    }

    #[test]
    fn test_chunk_entries_basic() {
        // Create 10 entries with ~200 chars each → ~500 tokens total
        // With target=150 tokens, should get multiple chunks
        let entries: Vec<TimelineEntry> = (0..10)
            .map(|i| make_entry(14, i as u32, "user", &"x".repeat(200)))
            .collect();

        let config = ChunkerConfig {
            target_tokens: 150,
            min_tokens: 50,
            max_tokens: 300,
            overlap_messages: 2,
        };

        let chunks = chunk_entries(&entries, "proj", "claude", &config);
        assert!(
            chunks.len() > 1,
            "Expected multiple chunks, got {}",
            chunks.len()
        );

        // Verify sequential IDs
        for (i, chunk) in chunks.iter().enumerate() {
            assert!(chunk.id.contains(&format!("{:03}", i + 1)));
        }
    }

    #[test]
    fn test_chunk_entries_respects_max_tokens() {
        // One very long message
        let entries = vec![make_entry(14, 0, "user", &"x".repeat(20000))];
        let config = ChunkerConfig {
            target_tokens: 1500,
            min_tokens: 500,
            max_tokens: 2500,
            overlap_messages: 2,
        };

        let chunks = chunk_entries(&entries, "proj", "claude", &config);
        // Single long message can't be split within chunker (it's per-message)
        // but format_chunk_text truncates at 4000 bytes
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0].text.contains("[truncated]"));
    }

    #[test]
    fn test_chunk_entries_groups_by_date() {
        let entries = vec![
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 20, 10, 0, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "s1".to_string(),
                role: "user".to_string(),
                message: "day one".to_string(),
                branch: None,
                cwd: None,
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 21, 10, 0, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "s2".to_string(),
                role: "user".to_string(),
                message: "day two".to_string(),
                branch: None,
                cwd: None,
            },
        ];

        let config = ChunkerConfig::default();
        let chunks = chunk_entries(&entries, "proj", "claude", &config);

        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].date, "2026-01-20");
        assert_eq!(chunks[1].date, "2026-01-21");
    }

    #[test]
    fn test_format_chunk_text() {
        let entries = [
            make_entry(14, 30, "user", "hello"),
            make_entry(14, 31, "assistant", "hi there"),
        ];
        let refs: Vec<&TimelineEntry> = entries.iter().collect();

        let text = format_chunk_text(&refs, "TestProj", "claude", "2026-01-22");

        assert!(text.starts_with("[project: TestProj | agent: claude | date: 2026-01-22]"));
        assert!(text.contains("[14:30:00] user: hello"));
        assert!(text.contains("[14:31:00] assistant: hi there"));
    }

    #[test]
    fn test_format_chunk_text_truncates_utf8_safely() {
        let mut msg = "a".repeat(3999);
        msg.push('é'); // 2-byte char forces non-boundary at 4000
        let entries = [make_entry(14, 30, "user", &msg)];
        let refs: Vec<&TimelineEntry> = entries.iter().collect();

        let text = format_chunk_text(&refs, "TestProj", "claude", "2026-01-22");

        assert!(text.contains("[truncated]"));
        assert!(!text.contains('é'));
    }

    #[test]
    fn test_chunk_entries_extracts_frontmatter_telemetry() {
        let entries = vec![make_entry(
            14,
            30,
            "assistant",
            "---\nrun_id: mrbl-001\nprompt_id: api-redesign_20260327\nmodel: gpt-5.4\nstarted_at: 2026-03-27T10:00:00Z\ncompleted_at: 2026-03-27T10:01:00Z\ntoken_usage: 1234\nfindings_count: 4\nphase: implement\nmode: session-first\nskill_code: vc-workflow\nframework_version: 2026-03\n---\n## Report\nContent here",
        )];

        let chunks = chunk_entries(&entries, "proj", "claude", &ChunkerConfig::default());
        assert_eq!(chunks.len(), 1);

        let chunk = &chunks[0];
        assert_eq!(chunk.run_id.as_deref(), Some("mrbl-001"));
        assert_eq!(chunk.prompt_id.as_deref(), Some("api-redesign_20260327"));
        assert_eq!(chunk.agent_model.as_deref(), Some("gpt-5.4"));
        assert_eq!(chunk.started_at.as_deref(), Some("2026-03-27T10:00:00Z"));
        assert_eq!(chunk.completed_at.as_deref(), Some("2026-03-27T10:01:00Z"));
        assert_eq!(chunk.token_usage, Some(1234));
        assert_eq!(chunk.findings_count, Some(4));
        assert_eq!(chunk.workflow_phase.as_deref(), Some("implement"));
        assert_eq!(chunk.mode.as_deref(), Some("session-first"));
        assert_eq!(chunk.skill_code.as_deref(), Some("vc-workflow"));
        assert_eq!(chunk.framework_version.as_deref(), Some("2026-03"));
        assert!(chunk.text.contains("## Report"));
        assert!(!chunk.text.contains("run_id: mrbl-001"));
        assert!(!chunk.text.contains("phase: implement"));
    }

    #[test]
    fn test_chunk_entries_strip_malformed_frontmatter_without_metadata() {
        let entries = vec![make_entry(
            14,
            30,
            "assistant",
            "---\nrun_id: [nope\nmode: session-first\n---\n## Report\nBody survives",
        )];

        let chunks = chunk_entries(&entries, "proj", "claude", &ChunkerConfig::default());
        assert_eq!(chunks.len(), 1);

        let chunk = &chunks[0];
        assert_eq!(chunk.run_id, None);
        assert_eq!(chunk.mode, None);
        assert!(chunk.text.contains("## Report"));
        assert!(chunk.text.contains("Body survives"));
        assert!(!chunk.text.contains("mode: session-first"));
    }

    #[test]
    fn test_write_chunks_to_dir() {
        let tmp = std::env::temp_dir().join("ai-ctx-chunker-test");
        let _ = fs::remove_dir_all(&tmp);

        let chunks = vec![
            Chunk {
                id: "proj_claude_2026-01-22_001".to_string(),
                project: "proj".to_string(),
                agent: "claude".to_string(),
                date: "2026-01-22".to_string(),
                session_id: "s1".to_string(),
                cwd: Some("/Users/tester/workspaces/proj".to_string()),
                kind: crate::store::Kind::Conversations,
                run_id: None,
                prompt_id: None,
                agent_model: None,
                started_at: None,
                completed_at: None,
                token_usage: None,
                findings_count: None,
                workflow_phase: Some("implement".to_string()),
                mode: Some("session-first".to_string()),
                skill_code: Some("vc-workflow".to_string()),
                framework_version: Some("2026-03".to_string()),
                msg_range: (0, 5),
                text: "chunk one content".to_string(),
                token_estimate: 4,
                highlights: vec![],
            },
            Chunk {
                id: "proj_claude_2026-01-22_002".to_string(),
                project: "proj".to_string(),
                agent: "claude".to_string(),
                date: "2026-01-22".to_string(),
                session_id: "s1".to_string(),
                cwd: None,
                kind: crate::store::Kind::Conversations,
                run_id: None,
                prompt_id: None,
                agent_model: None,
                started_at: None,
                completed_at: None,
                token_usage: None,
                findings_count: None,
                workflow_phase: None,
                mode: None,
                skill_code: None,
                framework_version: None,
                msg_range: (3, 8),
                text: "chunk two content".to_string(),
                token_estimate: 4,
                highlights: vec![],
            },
        ];

        let paths = write_chunks_to_dir(&chunks, &tmp).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].exists());
        assert!(paths[1].exists());

        let content = fs::read_to_string(&paths[0]).unwrap();
        assert_eq!(content, "chunk one content");

        let sidecar = fs::read_to_string(tmp.join("proj_claude_2026-01-22_001.meta.json")).unwrap();
        let metadata: ChunkMetadataSidecar = serde_json::from_str(&sidecar).unwrap();
        assert_eq!(metadata.project, "proj");
        assert_eq!(metadata.agent, "claude");
        assert_eq!(metadata.date, "2026-01-22");
        assert_eq!(
            metadata.cwd.as_deref(),
            Some("/Users/tester/workspaces/proj")
        );
        assert_eq!(metadata.kind, crate::store::Kind::Conversations);
        assert_eq!(metadata.workflow_phase.as_deref(), Some("implement"));
        assert_eq!(metadata.mode.as_deref(), Some("session-first"));
        assert_eq!(metadata.skill_code.as_deref(), Some("vc-workflow"));
        assert_eq!(metadata.framework_version.as_deref(), Some("2026-03"));

        let legacy: ChunkMetadataSidecar = serde_json::from_value(serde_json::json!({
            "id": "legacy",
            "project": "proj",
            "agent": "claude",
            "date": "2026-01-22",
            "session_id": "s1",
            "kind": "conversations",
        }))
        .unwrap();
        assert_eq!(legacy.cwd, None);
        assert_eq!(legacy.workflow_phase, None);
        assert_eq!(legacy.mode, None);
        assert_eq!(legacy.skill_code, None);
        assert_eq!(legacy.framework_version, None);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_overlap_messages() {
        // 8 entries with short messages (~22 tokens each incl. header)
        // target=80 → ~4 messages per window, overlap=2 → windows share 2 messages
        let entries: Vec<TimelineEntry> = (0..8)
            .map(|i| make_entry(14, i as u32, "user", &format!("msg_{}", i)))
            .collect();

        let config = ChunkerConfig {
            target_tokens: 80,
            min_tokens: 20,
            max_tokens: 200,
            overlap_messages: 2,
        };

        let chunks = chunk_entries(&entries, "p", "c", &config);

        // With overlap=2, consecutive chunks should share messages
        if chunks.len() >= 2 {
            // Verify ranges overlap (overlap=2 means last 2 msgs of chunk N start chunk N+1)
            let (_, end1) = chunks[0].msg_range;
            let (start2, _) = chunks[1].msg_range;
            assert!(
                start2 < end1,
                "Expected overlap: chunk1 ends at {}, chunk2 starts at {}",
                end1,
                start2
            );
        }
    }

    #[test]
    fn test_chunk_id_format() {
        let entries = vec![make_entry(10, 0, "user", "test")];
        let config = ChunkerConfig::default();
        let chunks = chunk_entries(&entries, "MyProject", "gemini", &config);

        assert_eq!(chunks[0].id, "MyProject_gemini_2026-01-22_001");
    }

    #[test]
    fn test_chunk_summary() {
        let chunks = vec![
            Chunk {
                id: "a".to_string(),
                project: "p".to_string(),
                agent: "c".to_string(),
                date: "2026-01-20".to_string(),
                session_id: "s".to_string(),
                cwd: None,
                kind: crate::store::Kind::Conversations,
                run_id: None,
                prompt_id: None,
                agent_model: None,
                started_at: None,
                completed_at: None,
                token_usage: None,
                findings_count: None,
                workflow_phase: None,
                mode: None,
                skill_code: None,
                framework_version: None,
                msg_range: (0, 5),
                text: "x".repeat(100),
                token_estimate: 25,
                highlights: vec![],
            },
            Chunk {
                id: "b".to_string(),
                project: "p".to_string(),
                agent: "c".to_string(),
                date: "2026-01-21".to_string(),
                session_id: "s".to_string(),
                cwd: None,
                kind: crate::store::Kind::Conversations,
                run_id: None,
                prompt_id: None,
                agent_model: None,
                started_at: None,
                completed_at: None,
                token_usage: None,
                findings_count: None,
                workflow_phase: None,
                mode: None,
                skill_code: None,
                framework_version: None,
                msg_range: (5, 10),
                text: "y".repeat(200),
                token_estimate: 50,
                highlights: vec![],
            },
        ];

        let summary = chunk_summary(&chunks);
        assert!(summary.contains("2 chunks"));
        assert!(summary.contains("75 total tokens"));
        assert!(summary.contains("2 days"));
    }

    #[test]
    fn test_extract_highlights_filters_keywords() {
        let entries = [
            make_entry(10, 0, "user", "Decision: lock chunking heuristics"),
            make_entry(10, 1, "assistant", "Just chatting"),
            make_entry(10, 2, "user", "TODO: add summarization notes"),
            make_entry(10, 3, "user", "KEY architectural choice"),
        ];
        let refs: Vec<&TimelineEntry> = entries.iter().collect();

        let highlights = extract_highlights(&refs);
        assert_eq!(
            highlights,
            vec![
                "Decision: lock chunking heuristics",
                "TODO: add summarization notes",
                "KEY architectural choice"
            ]
        );
    }

    #[test]
    fn test_format_chunk_text_includes_signals_for_checklist_and_intent() {
        let entries = [make_entry(
            14,
            30,
            "user",
            "No i tutaj mam taki pomysł, żeby to zrobić\nPlan mode: enabled\nUser accepted the plan\nUltrathink:\n- [ ] pierwsza rzecz\n- [x] druga rzecz\n\n★ Insight ─ to działa",
        )];
        let refs: Vec<&TimelineEntry> = entries.iter().collect();

        let text = format_chunk_text(&refs, "TestProj", "claude", "2026-01-22");

        assert!(text.contains("[signals]"));
        assert!(text.contains("RED LIGHT: checklist detected (open: 1, done: 1)"));
        assert!(text.contains("- [ ] pierwsza rzecz"));
        assert!(text.contains("- [x] druga rzecz"));
        assert!(text.contains("Ultrathink:"));
        assert!(text.contains("- Ultrathink:"));
        assert!(text.contains("Insight:"));
        assert!(text.contains("- ★ Insight ─ to działa"));
        assert!(text.contains("Plan mode:"));
        assert!(text.contains("- Plan mode: enabled"));
        assert!(text.contains("- User accepted the plan"));
        assert!(text.contains("Intent:"));
        assert!(text.contains("No i tutaj mam taki pomysł, żeby to zrobić"));
        assert!(text.contains("[/signals]"));
    }
}
