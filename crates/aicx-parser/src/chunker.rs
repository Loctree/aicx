//! Semantic windowing chunker for RAG indexing.
//!
//! Splits timeline entries into overlapping windows of ~1.5k tokens,
//! suitable for vector embedding and semantic search via rust-memex.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use anyhow::Result;
use serde::{Deserialize, Deserializer, Serialize};
use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::timeline::{FrameKind, Kind, TimelineEntry};

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
    /// Timestamp provenance when any source frame used an inferred timestamp.
    pub timestamp_source: Option<String>,
    /// Classified kind for this chunk's content
    pub kind: Kind,
    /// Stable stream/channel classification for the chunk contents.
    pub frame_kind: Option<FrameKind>,
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
    /// Foreign-import provenance (operator-md and similar): originating file,
    /// its format, and a stable content-hash import id (Round II / oś 3+5).
    pub source_file: Option<String>,
    pub source_format: Option<String>,
    pub import_id: Option<String>,
    /// Index range in original day's entries (start, end exclusive)
    pub msg_range: (usize, usize),
    /// Formatted chunk text with header
    pub text: String,
    /// Estimated token count (~chars/4)
    pub token_estimate: usize,
    /// Decision/plan highlights extracted from the chunk
    pub highlights: Vec<String>,
    /// Number of structural-noise lines (line-numbered grep matches, tool
    /// echoes, stray YAML delimiters) dropped from the source entries while
    /// building this chunk. Operators use this as observability — high values
    /// flag corpora that should be re-ingested with the filter on, or
    /// upstream emitters that produce excessive scaffolding.
    pub noise_lines_dropped: usize,
}

/// Structured metadata sidecar persisted alongside each chunk file.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ChunkMetadataSidecar {
    pub id: String,
    #[serde(
        default = "default_card_schema_version",
        deserialize_with = "deserialize_card_schema_version",
        skip_serializing_if = "is_default_card_schema_version"
    )]
    pub schema_version: u32,
    pub project: String,
    pub agent: String,
    pub date: String,
    pub session_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp_source: Option<String>,
    pub kind: Kind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_kind: Option<FrameKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub speaker_hint: Option<String>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_format: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<CardSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claim_scope: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freshness_contract: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signals: Option<Vec<CardSignal>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub intent_entries: Vec<crate::types::IntentEntry>,
    /// Weak repo/content mentions preserved for query/tag surfaces. This is
    /// append-only so pre-tag sidecars deserialize with an empty vector.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truth_status: Option<TruthStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub learning_use: Option<LearningUse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keywords: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,
    /// Number of noise lines dropped during chunk construction. Defaults to
    /// `0` when the field is absent in older sidecars.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub noise_lines_dropped: usize,
}

pub const CARD_SCHEMA_VERSION: u32 = 2;
pub const CARD_CLAIM_SCOPE_SESSION_CLOSE: &str = "session_close";
pub const CARD_FRESHNESS_CONTRACT_HISTORICAL: &str = "historical";
pub const CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX: &str = "not_verified_by_aicx";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CardSource {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CardSignal {
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line_span: Option<(u64, u64)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extractor_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TruthStatus {
    pub role: TruthRole,
    #[serde(default)]
    pub runtime_authoritative: bool,
    #[serde(default)]
    pub stale_against_current_head: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_head_when_ingested: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TruthRole {
    Live,
    Example,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct LearningUse {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub forbidden: Vec<String>,
}

fn default_card_schema_version() -> u32 {
    1
}

fn is_default_card_schema_version(value: &u32) -> bool {
    *value == default_card_schema_version()
}

fn deserialize_card_schema_version<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SchemaVersionWire {
        Number(u32),
        String(String),
    }

    match SchemaVersionWire::deserialize(deserializer)? {
        SchemaVersionWire::Number(version) => Ok(version),
        SchemaVersionWire::String(version) => parse_schema_version_string(&version)
            .ok_or_else(|| serde::de::Error::custom(format!("invalid schema_version {version:?}"))),
    }
}

fn parse_schema_version_string(value: &str) -> Option<u32> {
    if let Ok(version) = value.parse::<u32>() {
        return Some(version);
    }

    ["card.v", "context_corpus.v", "v"]
        .iter()
        .find_map(|prefix| value.strip_prefix(prefix)?.parse::<u32>().ok())
}

fn is_zero_usize(value: &usize) -> bool {
    *value == 0
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
            timestamp_source: chunk.timestamp_source.clone(),
            kind: chunk.kind,
            frame_kind: chunk.frame_kind,
            speaker_hint: speaker_hint_from_chunk_text(&chunk.text),
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
            source_file: chunk.source_file.clone(),
            source_format: chunk.source_format.clone(),
            import_id: chunk.import_id.clone(),
            schema_version: default_card_schema_version(),
            source: None,
            claim_scope: None,
            freshness_contract: None,
            verification_state: None,
            signals: None,
            intent_entries: Vec::new(),
            tags: Vec::new(),
            artifact_family: None,
            truth_status: None,
            learning_use: None,
            keywords: None,
            content_sha256: None,
            noise_lines_dropped: chunk.noise_lines_dropped,
        }
    }
}

fn speaker_hint_from_chunk_text(text: &str) -> Option<String> {
    text.lines()
        .find_map(|line| line.strip_prefix("speaker_hint: "))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
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
    /// Whether to strip structural noise (line-numbered grep matches, tool
    /// echoes, stray YAML delimiters) before signal/highlight extraction.
    /// Default: `true`. Set to `false` for debugging or when raw upstream
    /// content must be preserved verbatim.
    pub noise_filter_enabled: bool,
}

impl Default for ChunkerConfig {
    fn default() -> Self {
        Self {
            target_tokens: 1500,
            min_tokens: 500,
            max_tokens: 2500,
            overlap_messages: 2,
            noise_filter_enabled: true,
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

// ── Kind heuristics ────────────────────────────────────────────────────────

const PLAN_KEYWORDS: &[&str] = &[
    "implementation plan",
    "plan:",
    "## plan",
    "step 1:",
    "step 2:",
    "step 3:",
    "action items",
    "milestones",
    "roadmap",
    "todo list",
    "acceptance criteria",
    "## steps",
    "## phases",
];

const REPORT_KEYWORDS: &[&str] = &[
    "## findings",
    "## summary",
    "## report",
    "audit report",
    "coverage report",
    "test results",
    "## metrics",
    "## recommendations",
    "## conclusion",
    "status report",
    "incident report",
    "pr review",
    "code review",
];

/// Classify a set of timeline entries into a canonical `Kind`.
///
/// Uses a lightweight keyword-scoring approach:
/// - Scans assistant messages (where classification signal is strongest)
/// - Scores plan vs report keywords
/// - Conversations win by default when neither plan nor report signal is strong
///
/// The approach is intentionally conservative: ambiguous content falls to
/// `Conversations` (the most common kind), not `Other`.
pub fn classify_kind(entries: &[TimelineEntry]) -> Kind {
    if entries.is_empty() {
        return Kind::Other;
    }

    let mut plan_score: u32 = 0;
    let mut report_score: u32 = 0;
    let mut has_conversation = false;

    for entry in entries {
        let lower = entry.message.to_lowercase();

        // Only count strong signals from assistant messages.
        if entry.role == "assistant" {
            for kw in PLAN_KEYWORDS {
                if lower.contains(kw) {
                    plan_score += 1;
                }
            }
            for kw in REPORT_KEYWORDS {
                if lower.contains(kw) {
                    report_score += 1;
                }
            }
        }

        if entry.role == "user" || entry.role == "assistant" {
            has_conversation = true;
        }
    }

    let threshold = 3;

    if plan_score >= threshold && plan_score > report_score {
        Kind::Plans
    } else if report_score >= threshold && report_score > plan_score {
        Kind::Reports
    } else if has_conversation {
        Kind::Conversations
    } else {
        Kind::Other
    }
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
    if chunk.frame_kind.is_none() {
        chunk.frame_kind = frontmatter.telemetry.frame_kind;
    }
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
    chunk.source_file = frontmatter.telemetry.source_file.clone();
    chunk.source_format = frontmatter.telemetry.source_format.clone();
    chunk.import_id = frontmatter.telemetry.import_id.clone();
}

fn split_day_entries_by_frame_kind<'a>(
    entries: &'a [(usize, &'a TimelineEntry)],
) -> Vec<&'a [(usize, &'a TimelineEntry)]> {
    if entries.is_empty() {
        return Vec::new();
    }

    let mut groups = Vec::new();
    let mut start = 0usize;

    for idx in 1..entries.len() {
        let previous = entries[idx - 1].1.frame_kind;
        let current = entries[idx].1.frame_kind;
        if previous != current {
            groups.push(&entries[start..idx]);
            start = idx;
        }
    }

    groups.push(&entries[start..]);
    groups
}

fn frame_kind_for_window(entries: &[&TimelineEntry]) -> Option<FrameKind> {
    let first = entries.first().and_then(|entry| entry.frame_kind)?;
    entries
        .iter()
        .all(|entry| entry.frame_kind == Some(first))
        .then_some(first)
}

fn timestamp_source_for_window(entries: &[&TimelineEntry]) -> Option<String> {
    let mut sources = entries
        .iter()
        .filter_map(|entry| entry.timestamp_source.as_deref());
    let first = sources.next()?;
    let mut source = first.to_string();
    if sources.any(|candidate| candidate != first) {
        source = "mixed".to_string();
    }
    Some(source)
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

    let project = canonical_project_label(project);
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
        let mut day_chunks = Vec::new();
        let mut next_seq = 1usize;
        for frame_group in split_day_entries_by_frame_kind(day_entries) {
            let (mut group_chunks, updated_seq) =
                chunk_day_entries(frame_group, &project, agent, date, config, next_seq);
            next_seq = updated_seq;
            day_chunks.append(&mut group_chunks);
        }
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
    start_seq: usize,
) -> (Vec<Chunk>, usize) {
    if entries.is_empty() {
        return (vec![], start_seq);
    }

    let mut chunks = Vec::new();
    let mut seq = start_seq;
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

        // Build chunk from entries[start..end].
        //
        // Pre-sanitize each entry's message through the noise filter BEFORE
        // signal/highlight extraction so the `[signals]` block and the
        // entry-level body are both built from semantic content only. The
        // filter can be disabled via `ChunkerConfig::noise_filter_enabled`
        // for debugging/raw modes.
        let window: Vec<&TimelineEntry> = entries[start..end].iter().map(|(_, e)| *e).collect();
        let (sanitized_owned, noise_lines_dropped) = sanitize_window(&window, config);
        let window: Vec<&TimelineEntry> = sanitized_owned.iter().collect();
        let highlights = extract_highlights(&window);
        let signals = extract_signals(&window);
        let frame_kind = frame_kind_for_window(&window);
        let timestamp_source = timestamp_source_for_window(&window);
        let text = format_chunk_text_inner(
            &window,
            project,
            agent,
            date,
            frame_kind,
            &signals,
            &highlights,
        );
        let token_estimate = estimate_tokens(&text);

        let session_id = window
            .first()
            .map(|e| e.session_id.clone())
            .unwrap_or_default();
        let cwd = window.first().and_then(|entry| entry.cwd.clone());

        let global_start = entries[start].0;
        let global_end = entries[end - 1].0 + 1;

        let kind = classify_kind(&window.iter().map(|e| (*e).clone()).collect::<Vec<_>>());

        chunks.push(Chunk {
            id: format!("{}_{}_{}_{{:03}}", project, agent, date)
                .replace("{:03}", &format!("{:03}", seq)),
            project: project.to_string(),
            agent: agent.to_string(),
            date: date.to_string(),
            session_id,
            cwd,
            timestamp_source,
            kind,
            frame_kind,
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
            source_file: None,
            source_format: None,
            import_id: None,
            msg_range: (global_start, global_end),
            text,
            token_estimate,
            highlights,
            noise_lines_dropped,
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

    (chunks, seq)
}

/// Format entries into chunk text with metadata header.
///
/// The public surface uses the chunker's default configuration; callers that
/// need non-default behavior (e.g. disabled noise filter) should go through
/// [`chunk_entries`] which threads [`ChunkerConfig`] in full.
pub fn format_chunk_text(
    entries: &[&TimelineEntry],
    project: &str,
    agent: &str,
    date: &str,
) -> String {
    let config = ChunkerConfig::default();
    let (sanitized_owned, _dropped) = sanitize_window(entries, &config);
    let entries: Vec<&TimelineEntry> = sanitized_owned.iter().collect();
    let highlights = extract_highlights(&entries);
    let signals = extract_signals(&entries);
    let project = canonical_project_label(project);
    format_chunk_text_inner(
        &entries,
        &project,
        agent,
        date,
        frame_kind_for_window(&entries),
        &signals,
        &highlights,
    )
}

fn canonical_project_label(project: &str) -> String {
    project
        .split('/')
        .map(|segment| segment.trim().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

/// Clone a window's entries with their messages run through
/// [`crate::noise::filter_noise_lines_with_count`]. Used before any signal
/// or highlight extraction so structural noise never leaks into the semantic
/// surface. Returns the cloned window plus the aggregate count of dropped
/// noise lines for observability sidecars.
///
/// When `config.noise_filter_enabled` is `false`, returns a plain clone of
/// the input window with `dropped == 0`.
fn sanitize_window(
    window: &[&TimelineEntry],
    config: &ChunkerConfig,
) -> (Vec<TimelineEntry>, usize) {
    if !config.noise_filter_enabled {
        let cloned = window.iter().map(|entry| (*entry).clone()).collect();
        return (cloned, 0);
    }

    let mut total_dropped = 0usize;
    let cloned: Vec<TimelineEntry> = window
        .iter()
        .map(|entry| {
            let mut cloned = (*entry).clone();
            let (filtered, dropped) = crate::noise::filter_noise_lines_with_count(&entry.message);
            cloned.message = filtered;
            total_dropped += dropped;
            cloned
        })
        .collect();
    (cloned, total_dropped)
}

fn format_chunk_text_inner(
    entries: &[&TimelineEntry],
    project: &str,
    agent: &str,
    date: &str,
    frame_kind: Option<FrameKind>,
    signals: &ChunkSignals,
    highlights: &[String],
) -> String {
    let mut text = if let Some(frame_kind) = frame_kind {
        format!(
            "[project: {} | agent: {} | date: {} | frame_kind: {}]\n\n",
            project, agent, date, frame_kind
        )
    } else {
        format!(
            "[project: {} | agent: {} | date: {}]\n\n",
            project, agent, date
        )
    };

    if let Some(block) = format_signals_block(signals, highlights) {
        text.push_str(&block);
        text.push('\n');
    }

    // Note: callers (`format_chunk_text`, `chunk_day_entries`) pass entries
    // that have already been routed through `sanitize_window`, so message
    // bodies here are noise-free. Skip empty messages so windows that reduced
    // to pure scaffolding don't emit empty role lines.
    for entry in entries {
        if entry.message.is_empty() {
            continue;
        }
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

pub const INTENT_KEYWORDS: &[&str] = &[
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
    "prosze",
    "proszę",
    "odpal",
    "uruchom",
    "usun",
    "usuń",
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
        // Track fenced code blocks per entry. Checklist-looking lines inside
        // ``` fences (pasted markdown snippets, code samples documenting
        // checklist syntax) must not be promoted to actual tasks.
        let mut in_fence = false;
        for line in entry.message.lines() {
            if line.trim_start().starts_with("```") {
                in_fence = !in_fence;
                continue;
            }
            if in_fence {
                continue;
            }
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

pub fn parse_checklist_task(line: &str) -> Option<(bool, String)> {
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
        if is_local_command_artifact_entry(entry) {
            continue;
        }
        for line in entry.message.lines().map(str::trim) {
            if line.is_empty() {
                continue;
            }
            if is_source_metadata_line(line) {
                continue;
            }
            if is_local_command_artifact_line(line) {
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

fn is_local_command_artifact_entry(entry: &TimelineEntry) -> bool {
    entry
        .message
        .lines()
        .take(3)
        .any(is_local_command_artifact_line)
}

pub fn is_local_command_artifact_line(line: &str) -> bool {
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

fn strip_signal_bullet(line: &str) -> &str {
    let trimmed = line.trim_start();
    if let Some(rest) = trimmed.strip_prefix("- ") {
        return rest.trim_start();
    }
    if let Some(rest) = trimmed.strip_prefix("* ") {
        return rest.trim_start();
    }
    trimmed
}

pub(crate) fn is_intent_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.starts_with("intent:")
        || lower.starts_with("[intent]")
        || severity_marker(line).is_some()
        || INTENT_KEYWORDS.iter().any(|kw| lower.contains(kw))
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

pub fn is_result_line(line: &str) -> bool {
    let lower = line.to_lowercase();
    RESULT_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

pub fn normalize_key(s: &str) -> String {
    // Strip invisible characters that would otherwise let "fix au\u{200B}th"
    // bypass dedup of "fix auth". Covers zero-width family
    // (U+200B/200C/200D/FEFF) and bidi controls (U+202A-202E/2066-2069) that
    // can be pasted from chat clients or copy-paste from PDFs.
    let cleaned: String = s
        .chars()
        .filter(|ch| !is_invisible_normalize_char(*ch))
        .collect();
    cleaned
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

fn is_invisible_normalize_char(ch: char) -> bool {
    matches!(
        ch,
        '\u{200B}'
            | '\u{200C}'
            | '\u{200D}'
            | '\u{FEFF}'
            | '\u{202A}'
            | '\u{202B}'
            | '\u{202C}'
            | '\u{202D}'
            | '\u{202E}'
            | '\u{2066}'
            | '\u{2067}'
            | '\u{2068}'
            | '\u{2069}'
    )
}

pub fn truncate_signal_line(line: &str) -> String {
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

pub fn is_decision_tag(line: &str) -> bool {
    let lower = line.to_lowercase();
    lower.contains("[decision]") || lower.starts_with("decision:")
}

pub fn is_outcome_tag(line: &str) -> bool {
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
            frame_kind: None,
            branch: None,
            cwd: None,
            timestamp_source: None,
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
            noise_filter_enabled: true,
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
            noise_filter_enabled: true,
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
                frame_kind: None,
                branch: None,
                cwd: None,
                timestamp_source: None,
            },
            TimelineEntry {
                timestamp: Utc.with_ymd_and_hms(2026, 1, 21, 10, 0, 0).unwrap(),
                agent: "claude".to_string(),
                session_id: "s2".to_string(),
                role: "user".to_string(),
                message: "day two".to_string(),
                frame_kind: None,
                branch: None,
                cwd: None,
                timestamp_source: None,
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

        assert!(text.starts_with("[project: testproj | agent: claude | date: 2026-01-22]"));
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
            "---\nrun_id: mrbl-001\nprompt_id: api-redesign_20260327\nmodel: gpt-5.4\nstarted_at: 2026-03-27T10:00:00Z\ncompleted_at: 2026-03-27T10:01:00Z\ntoken_usage: 1234\nfindings_count: 4\nframe_kind: agent_reply\nphase: implement\nmode: session-first\nskill_code: vc-workflow\nframework_version: 2026-03\n---\n## Report\nContent here",
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
        assert_eq!(chunk.frame_kind, Some(FrameKind::AgentReply));
        assert_eq!(chunk.workflow_phase.as_deref(), Some("implement"));
        assert_eq!(chunk.mode.as_deref(), Some("session-first"));
        assert_eq!(chunk.skill_code.as_deref(), Some("vc-workflow"));
        assert_eq!(chunk.framework_version.as_deref(), Some("2026-03"));
        assert!(chunk.text.contains("## Report"));
        assert!(!chunk.text.contains("run_id: mrbl-001"));
        assert!(!chunk.text.contains("phase: implement"));
    }

    #[test]
    fn test_chunk_entries_extracts_foreign_import_provenance() {
        // Round II / oś 3+5 cut 1: foreign-import provenance carried structurally
        // from frontmatter through Chunk to the sidecar (not left as body text).
        let entries = vec![make_entry(
            14,
            30,
            "user",
            "---\nsource_file: Downloads/ChatGPT-export.md\nsource_format: chatgpt-markdown\nimport_id: blake3:abc123\nframe_kind: user_msg\n---\n## Prompt\nzbadaj temat",
        )];

        let chunks = chunk_entries(&entries, "proj", "operator", &ChunkerConfig::default());
        assert_eq!(chunks.len(), 1);
        let chunk = &chunks[0];

        assert_eq!(
            chunk.source_file.as_deref(),
            Some("Downloads/ChatGPT-export.md")
        );
        assert_eq!(chunk.source_format.as_deref(), Some("chatgpt-markdown"));
        assert_eq!(chunk.import_id.as_deref(), Some("blake3:abc123"));
        // provenance is stripped from the chunk body (lives in metadata now)
        assert!(!chunk.text.contains("import_id: blake3:abc123"));

        // and it flows into the structural sidecar
        let sidecar = ChunkMetadataSidecar::from(chunk);
        assert_eq!(
            sidecar.source_file.as_deref(),
            Some("Downloads/ChatGPT-export.md")
        );
        assert_eq!(sidecar.source_format.as_deref(), Some("chatgpt-markdown"));
        assert_eq!(sidecar.import_id.as_deref(), Some("blake3:abc123"));
    }

    #[test]
    fn test_chunk_entries_skips_unsupported_frontmatter_values_without_dropping_metadata() {
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
        assert_eq!(chunk.mode.as_deref(), Some("session-first"));
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
                timestamp_source: None,
                kind: Kind::Conversations,
                frame_kind: Some(FrameKind::UserMsg),
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
                source_file: None,
                source_format: None,
                import_id: None,
                msg_range: (0, 5),
                text: "chunk one content".to_string(),
                token_estimate: 4,
                highlights: vec![],
                noise_lines_dropped: 0,
            },
            Chunk {
                id: "proj_claude_2026-01-22_002".to_string(),
                project: "proj".to_string(),
                agent: "claude".to_string(),
                date: "2026-01-22".to_string(),
                session_id: "s1".to_string(),
                cwd: None,
                timestamp_source: None,
                kind: Kind::Conversations,
                frame_kind: None,
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
                source_file: None,
                source_format: None,
                import_id: None,
                msg_range: (3, 8),
                text: "chunk two content".to_string(),
                token_estimate: 4,
                highlights: vec![],
                noise_lines_dropped: 0,
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
        assert_eq!(metadata.kind, Kind::Conversations);
        assert_eq!(metadata.frame_kind, Some(FrameKind::UserMsg));
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
        assert_eq!(legacy.frame_kind, None);
        assert_eq!(legacy.workflow_phase, None);
        assert_eq!(legacy.mode, None);
        assert_eq!(legacy.skill_code, None);
        assert_eq!(legacy.framework_version, None);
        assert_eq!(legacy.artifact_family, None);
        assert_eq!(legacy.schema_version, 1);
        assert_eq!(legacy.truth_status, None);
        assert_eq!(legacy.learning_use, None);
        assert_eq!(legacy.keywords, None);
        assert_eq!(legacy.content_sha256, None);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn sidecar_deserializes_context_corpus_contract_fields() {
        let sidecar: ChunkMetadataSidecar = serde_json::from_value(serde_json::json!({
            "id": "ctx-001",
            "project": "vetcoders/aicx",
            "agent": "loct-context-pack",
            "date": "2026-05-08",
            "session_id": "batch-001",
            "kind": "reports",
            "artifact_family": "loct-context-pack",
            "schema_version": "context_corpus.v1",
            "truth_status": {
                "role": "example",
                "runtime_authoritative": false,
                "stale_against_current_head": true,
                "current_head_when_ingested": "269d13c"
            },
            "learning_use": {
                "allowed": ["retrieval-test"],
                "forbidden": ["live-truth"]
            },
            "keywords": ["prism", "context"],
            "content_sha256": "abc123"
        }))
        .unwrap();

        assert_eq!(
            sidecar.artifact_family.as_deref(),
            Some("loct-context-pack")
        );
        assert_eq!(sidecar.schema_version, 1);
        assert_eq!(
            sidecar.truth_status.as_ref().map(|status| status.role),
            Some(TruthRole::Example)
        );
        assert_eq!(
            sidecar.keywords.as_deref(),
            Some(&["prism".to_string(), "context".to_string()][..])
        );
        assert_eq!(sidecar.content_sha256.as_deref(), Some("abc123"));
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
            noise_filter_enabled: true,
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

        assert_eq!(chunks[0].id, "myproject_gemini_2026-01-22_001");
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
                timestamp_source: None,
                kind: Kind::Conversations,
                frame_kind: None,
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
                source_file: None,
                source_format: None,
                import_id: None,
                msg_range: (0, 5),
                text: "x".repeat(100),
                token_estimate: 25,
                highlights: vec![],
                noise_lines_dropped: 0,
            },
            Chunk {
                id: "b".to_string(),
                project: "p".to_string(),
                agent: "c".to_string(),
                date: "2026-01-21".to_string(),
                session_id: "s".to_string(),
                cwd: None,
                timestamp_source: None,
                kind: Kind::Conversations,
                frame_kind: None,
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
                source_file: None,
                source_format: None,
                import_id: None,
                msg_range: (5, 10),
                text: "y".repeat(200),
                token_estimate: 50,
                highlights: vec![],
                noise_lines_dropped: 0,
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

    #[test]
    fn test_format_chunk_text_skips_local_command_artifact_intents() {
        let entries = [
            make_entry(
                14,
                31,
                "user",
                "<local-command-caveat>DO NOT respond to these messages</local-command-caveat>",
            ),
            make_entry(
                14,
                32,
                "user",
                "<bash-stdout>curl output\n* issuer: C=US; O=Let's Encrypt; CN=E7\n* SSL certificate verify ok.\n</bash-stdout>",
            ),
        ];
        let refs: Vec<&TimelineEntry> = entries.iter().collect();

        let text = format_chunk_text(&refs, "TestProj", "claude", "2026-01-22");

        assert!(!text.contains("Intent:"));
    }

    // ── Area E.8 + E.12 regression coverage ─────────────────────────────

    #[test]
    fn test_normalize_key_strips_zero_width_chars() {
        assert_eq!(normalize_key("fix au\u{200B}th"), "fix auth");
        assert_eq!(normalize_key("fix au\u{200C}th"), "fix auth");
        assert_eq!(normalize_key("fix au\u{200D}th"), "fix auth");
        assert_eq!(normalize_key("\u{FEFF}fix auth"), "fix auth");
    }

    #[test]
    fn test_normalize_key_strips_bidi_controls() {
        assert_eq!(normalize_key("fix\u{202A}auth\u{202C}"), "fixauth");
        assert_eq!(normalize_key("fix \u{2066}auth\u{2069}"), "fix auth");
    }

    #[test]
    fn test_normalize_key_preserves_visible_chars() {
        assert_eq!(normalize_key("  Fix  AUTH  Bug  "), "fix auth bug");
        assert_eq!(normalize_key("naprawdę"), "naprawdę");
    }

    #[test]
    fn test_extract_checklist_skips_lines_inside_code_fence() {
        let entry = make_entry(
            12,
            0,
            "user",
            "Here is sample syntax:\n```\n- [ ] sample task A\n- [x] sample task B\n```\nReal work:\n- [ ] real task open\n- [x] real task done",
        );
        let entries = vec![&entry];
        let (open, done) = extract_checklist_items(&entries);
        assert_eq!(open, vec!["real task open".to_string()]);
        assert_eq!(done, vec!["real task done".to_string()]);
    }

    #[test]
    fn test_extract_checklist_fence_toggle_resets_per_entry() {
        let entry_a = make_entry(12, 0, "user", "```\n- [ ] fenced leak");
        let entry_b = make_entry(12, 1, "user", "- [ ] honest task");
        let entries = vec![&entry_a, &entry_b];
        let (open, _done) = extract_checklist_items(&entries);
        assert_eq!(open, vec!["honest task".to_string()]);
    }
}
