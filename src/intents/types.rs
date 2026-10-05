use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

use crate::oracle::ClaimHonesty;
use crate::timeline::FrameKind;

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

    pub(super) fn sort_rank(self) -> u8 {
        match self {
            Self::Decision => 0,
            Self::Intent => 1,
            Self::Outcome => 2,
            Self::Task => 3,
        }
    }
}

/// Source provenance; a classifier label never certifies a human decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntentProvenance {
    pub role: String,
    pub locator: String,
    pub scope: Option<String>,
    pub timestamp_basis: String,
    pub attribution: String,
}

/// Receipt from the actual source-selection pass, before render budgets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SourceSelection {
    pub agent: String,
    pub session_id: String,
    pub path: String,
    pub catalog_project: Option<String>,
    pub admitted: bool,
    pub status: String,
    pub parsed_frames: usize,
    pub scoped_frames: usize,
    pub qualified_frames: usize,
    pub unknown_time_frames: usize,
    pub outside_window_frames: usize,
    pub scope_withheld_frames: usize,
    pub parser_coverage: Option<String>,
    pub latest_activity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IntentRecord {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provenance: Option<IntentProvenance>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Claim-honesty frame lifted from the source card sidecar (schema v2).
    /// Flattened so records expose `claim_scope`/`freshness_contract`/
    /// `verification_state` as plain keys; pre-v2 cards serialize no keys at
    /// all, keeping the JSON additive for existing consumers.
    #[serde(flatten)]
    pub honesty: ClaimHonesty,
}
#[derive(Debug, Clone)]
pub struct IntentsConfig {
    pub project: String,
    pub hours: u64,
    pub strict: bool,
    pub min_confidence: Option<u8>,
    pub kind_filter: Option<IntentKind>,
    pub frame_kind: Option<FrameKind>,
    /// Hot-window mode: also admit sessions the durable catalog census does
    /// not know yet (unadmitted) and catalog rows whose live mtime is inside
    /// the window even though their rebuild-time date fell outside it. Those
    /// records carry the `open_session`/`live_unverified` honesty frame.
    pub live: bool,
}

/// Widest retrieval window that turns the live source scan on by default.
/// Beyond this the census/index is authoritative and a live walk would only
/// add cost without hot-window value.
pub const LIVE_WINDOW_MAX_HOURS: u64 = 48;

impl IntentsConfig {
    pub fn default_frame_kind() -> FrameKind {
        FrameKind::UserMsg
    }

    /// Default live-window rule shared by every surface (CLI, MCP, wizard):
    /// hot when a bounded window of ≤ [`LIVE_WINDOW_MAX_HOURS`] is requested.
    pub fn auto_live(hours: u64) -> bool {
        hours > 0 && hours <= LIVE_WINDOW_MAX_HOURS
    }

    pub fn effective_frame_kind(&self) -> FrameKind {
        self.frame_kind.unwrap_or_else(Self::default_frame_kind)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentExtractionStats {
    pub scanned_count: usize,
    pub candidate_count: usize,
    pub source_paths_verified: bool,
    pub source_errors: usize,
    pub candidate_cap: usize,
    pub dropped_candidates: usize,
    pub dropped_task_events: usize,
    pub matched_project_buckets: Vec<String>,
    pub identity_source: String,
    pub path_heuristic_records: usize,
    /// Sessions admitted through the live window (open/unadmitted sources
    /// newer than the catalog census). 0 when live mode was off or nothing
    /// was fresher than the census.
    pub live_sessions: usize,
    /// Sessions the lanes could not serve whole under the requested project
    /// ([`IntentExtraction::mixed_scope`]).
    pub mixed_scope_sessions: usize,
    /// Frames the project filter withheld because the turn window they sit in
    /// could not be placed ([`IntentExtraction::unplaced_scope`]).
    pub unplaced_frames: usize,
}

/// Machine-readable honesty about whether an intents payload is exhaustive.
///
/// This deliberately lives beside extraction stats rather than in stderr:
/// JSON consumers (including MCP) must be able to distinguish a complete
/// result from a cap-truncated, identity-derived, or limit-saturated view.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectResolutionScope {
    pub match_mode: String,
    pub selected: Vec<String>,
    pub candidates: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IntentsCompleteness {
    pub complete: bool,
    pub source_errors: usize,
    pub candidate_cap: usize,
    pub candidate_cap_reached: bool,
    pub dropped_candidates: usize,
    pub dropped_task_events: usize,
    pub matched_project_buckets: Vec<String>,
    pub orphaned_buckets: Vec<String>,
    pub identity_source: String,
    /// Sessions admitted through the hot live window (open/unadmitted
    /// sources newer than the catalog census).
    #[serde(default)]
    pub live_sessions: usize,
    /// Sessions cataloged under the requested project that were not served
    /// whole: part or all of their work ran outside its checkout, in a proven
    /// workdir conflict, or in a scope `.aicxignore` hides. Those frames are
    /// withheld from every project's answer, so this one is not complete.
    #[serde(default)]
    pub mixed_scope_sessions: usize,
    /// Frames withheld because no checkout could claim the turn window they
    /// sit in. They may belong to the requested project or to another one:
    /// the answer cannot say, so it is not complete.
    #[serde(default)]
    pub unplaced_frames: usize,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requested_limit: Option<usize>,
    pub available_before_limit: usize,
    pub limit_saturated: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scope: Option<ProjectResolutionScope>,
}

impl IntentsCompleteness {
    pub fn with_project_scope(
        mut self,
        match_mode: impl Into<String>,
        selected: Vec<String>,
        candidates: Vec<String>,
    ) -> Self {
        let match_mode = match_mode.into();
        if match_mode == "fuzzy"
            && !self
                .warnings
                .iter()
                .any(|warning| warning == "fuzzy project matching active")
        {
            self.warnings
                .push("fuzzy project matching active".to_string());
        }
        self.scope = Some(ProjectResolutionScope {
            match_mode,
            selected,
            candidates,
        });
        self
    }
}

impl IntentExtractionStats {
    pub fn completeness(
        &self,
        requested_limit: Option<usize>,
        available_before_limit: usize,
    ) -> IntentsCompleteness {
        let limit_saturated = requested_limit
            .is_some_and(|limit| available_before_limit > 0 && available_before_limit >= limit);
        let candidate_cap_reached = self.dropped_candidates > 0 || self.dropped_task_events > 0;
        let complete = self.source_errors == 0
            && self.mixed_scope_sessions == 0
            && self.unplaced_frames == 0
            && !candidate_cap_reached
            && !limit_saturated;
        let orphaned_buckets = self
            .matched_project_buckets
            .iter()
            .filter(|project| crate::legacy_archive::is_ownerless_project_address(project))
            .cloned()
            .collect();
        let mut warnings = Vec::new();
        if self.identity_source == super::PATH_HEURISTIC_IDENTITY_SOURCE {
            warnings.push(format!(
                "{} record(s) resolved by path heuristic",
                self.path_heuristic_records
            ));
        }
        if candidate_cap_reached {
            warnings.push(format!(
                "candidate cap of {} reached; {} candidate(s) and {} task event(s) dropped",
                self.candidate_cap, self.dropped_candidates, self.dropped_task_events
            ));
        }
        if self.source_errors > 0 {
            warnings.push(format!(
                "{} catalog source(s) were unreadable or unsupported",
                self.source_errors
            ));
        }
        if self.live_sessions > 0 {
            warnings.push(format!(
                "{} session(s) admitted from the live window (open/unadmitted, unverified)",
                self.live_sessions
            ));
        }
        warnings.extend(self.withheld_scope());

        IntentsCompleteness {
            complete,
            source_errors: self.source_errors,
            candidate_cap: self.candidate_cap,
            candidate_cap_reached,
            dropped_candidates: self.dropped_candidates,
            dropped_task_events: self.dropped_task_events,
            matched_project_buckets: self.matched_project_buckets.clone(),
            orphaned_buckets,
            identity_source: self.identity_source.clone(),
            live_sessions: self.live_sessions,
            mixed_scope_sessions: self.mixed_scope_sessions,
            unplaced_frames: self.unplaced_frames,
            warnings,
            requested_limit,
            available_before_limit,
            limit_saturated,
            scope: None,
        }
    }

    /// One line per kind of work the project filters withheld: sessions not
    /// served whole, and frames whose turn window could not be placed.
    fn withheld_scope(&self) -> Vec<String> {
        let mut lines = Vec::new();
        if self.mixed_scope_sessions > 0 {
            lines.push(format!(
                "{} session(s) not served whole: work that ran outside this project's checkout was withheld",
                self.mixed_scope_sessions
            ));
        }
        if self.unplaced_frames > 0 {
            lines.push(format!(
                "{} frame(s) withheld: their turn window ran where no checkout could claim it",
                self.unplaced_frames
            ));
        }
        lines
    }

    /// The withheld work as a Markdown note, for the surfaces that print no
    /// `completeness`. An answer the filters emptied is otherwise blank, and
    /// reads as a project with no intents at all. `None` when nothing was
    /// withheld.
    pub fn withheld_scope_note(&self) -> Option<String> {
        let lines = self.withheld_scope();
        if lines.is_empty() {
            return None;
        }
        Some(
            lines
                .iter()
                .map(|line| format!("> {line}\n"))
                .collect::<String>()
                + "\n",
        )
    }
}

#[derive(Debug, Clone)]
pub struct IntentExtraction {
    pub selection: Vec<SourceSelection>,
    pub records: Vec<IntentRecord>,
    pub stats: IntentExtractionStats,
    /// Sessions in the window whose structural scope is a mixed-workstream
    /// candidate (W2-R1): several cwds and/or branches in one conversation.
    /// Their frames inside the requested project are still returned, each
    /// record carrying a `scope_status=mixed_candidate` evidence line. The
    /// rest are withheld, so `completeness` counts these sessions and is not
    /// complete. Single-history distillers (`continuity`) use this list to
    /// refuse by default.
    pub mixed_scope: Vec<MixedScopeSession>,
    /// Sessions the project filter served without some of their frames,
    /// because the turn window those frames sit in could not be placed: its
    /// workdir evidence named a directory that is unreadable, or that no
    /// checkout on this host claims. Not a mixed candidate — one unplaceable
    /// window does not withhold a session's placed history — but the answer is
    /// not the whole session either, and it has to say so.
    pub unplaced_scope: Vec<UnplacedScopeSession>,
}

/// A session with frames the project filter withheld as unplaced
/// ([`IntentExtraction::unplaced_scope`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UnplacedScopeSession {
    pub agent: String,
    pub session_id: String,
    /// Frames of the requested kind that were withheld.
    pub frames: usize,
}

/// One mixed-workstream candidate session, with the evidence that made it one:
/// a session its lane could not serve whole under one project, because it
/// spans more than one scope or ran in a checkout other than its cataloged
/// one. `cwds` are the session's own; the cataloged path is never among them
/// unless the session itself worked there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MixedScopeSession {
    pub agent: String,
    pub session_id: String,
    pub cwds: Vec<String>,
    pub branches: Vec<String>,
    /// Frames of the session in a proven workdir conflict.
    pub conflicts: usize,
    /// Scopes `.aicxignore` hid from this view: counted, never named.
    pub hidden_scopes: usize,
    /// The session's structural status as its scope report saw it.
    pub status: aicx_parser::engine::ScopeStatus,
}

impl MixedScopeSession {
    /// Parser agent for the refusal payload; unknown labels (importers such
    /// as codescribe) fall back to Claude only for the enum slot — the
    /// `agent` string above stays the truth.
    pub fn agent_kind(&self) -> aicx_parser::engine::AgentKind {
        use aicx_parser::engine::AgentKind;
        match self.agent.to_ascii_lowercase().as_str() {
            "codex" => AgentKind::Codex,
            "cursor" | "cursor-agent" => AgentKind::Cursor,
            "gemini" | "agy" => AgentKind::Gemini,
            "grok" => AgentKind::Grok,
            "junie" => AgentKind::Junie,
            "kimi" => AgentKind::Kimi,
            _ => AgentKind::Claude,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct StoredChunkFile {
    pub(super) agent: String,
    pub(super) date: String,
    pub(super) path: PathBuf,
    pub(super) project: String,
    pub(super) identity_source: String,
    pub(super) sequence: u32,
    pub(super) timestamp: DateTime<Utc>,
    pub(super) session_id: String,
    pub(super) honesty: ClaimHonesty,
    /// Structural scope of the session the frames came from, computed
    /// BEFORE the project filter narrows them (W2-R1). `None` for chunk
    /// documents that carry no cwd/branch evidence.
    pub(super) scope: Option<crate::extraction::conversation::ScopeReport>,
    pub(super) transcript_entries: Option<Vec<TranscriptEntry>>,
    /// Chunk document already held in memory, read from the committed lexical
    /// index instead of the original transcript.
    ///
    /// The index stores the canonical extract verbatim (`ChunkRef.text`), so
    /// this is the same document `parse_chunk_document` would reconstruct
    /// from disk — minus the re-parse of megabytes of raw JSONL.
    pub(super) body: Option<String>,
}

#[derive(Debug, Clone)]
pub(super) struct TranscriptEntry {
    pub(super) timestamp: Option<DateTime<Utc>>,
    pub(super) locator: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) role: String,
    pub(super) lines: Vec<String>,
}

#[derive(Debug, Clone)]
pub(super) struct IntentCandidate {
    pub(super) record: IntentRecord,
    pub(super) confidence: u8,
    pub(super) timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub(super) struct TaskEvent {
    pub(super) key: String,
    pub(super) candidate: IntentCandidate,
    pub(super) is_open: bool,
}

#[derive(Debug, Clone)]
pub(super) struct CandidateAccumulator {
    pub(super) candidate: IntentCandidate,
}

#[derive(Debug, Clone)]
pub(super) struct TaskAccumulator {
    pub(super) candidate: IntentCandidate,
    pub(super) is_open: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SignalSection {
    None,
    Intent,
    Decision,
    Results,
    Outcome,
    Ignore,
}

#[derive(Debug, Clone, Serialize)]
pub struct MigrationReport {
    pub total_chunks: usize,
    pub entries_found: usize,
    pub per_type: HashMap<String, usize>,
    pub per_project: HashMap<String, usize>,
    pub unresolved_count: usize,
}
