use chrono::{DateTime, Utc};
use serde::Serialize;
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
}

impl IntentsConfig {
    pub fn default_frame_kind() -> FrameKind {
        FrameKind::UserMsg
    }

    pub fn effective_frame_kind(&self) -> FrameKind {
        self.frame_kind.unwrap_or_else(Self::default_frame_kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IntentExtractionStats {
    pub scanned_count: usize,
    pub candidate_count: usize,
    pub source_paths_verified: bool,
}

#[derive(Debug, Clone)]
pub struct IntentExtraction {
    pub records: Vec<IntentRecord>,
    pub stats: IntentExtractionStats,
}

#[derive(Debug, Clone)]
pub(super) struct StoredChunkFile {
    pub(super) agent: String,
    pub(super) date: String,
    pub(super) path: PathBuf,
    pub(super) project: String,
    pub(super) sequence: u32,
    pub(super) timestamp: DateTime<Utc>,
    pub(super) session_id: String,
    pub(super) honesty: ClaimHonesty,
}

#[derive(Debug, Clone)]
pub(super) struct TranscriptEntry {
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
