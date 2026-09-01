//! Typed parser model. Heuristic intent/outcome/title fields do not live here.
//!
//! W2-R1 (identity re-entry): the model separates three things the old
//! `session_id: String` collapsed into one word —
//! * [`ProviderConversationRef`]: which conversation the provider says this
//!   is, in the provider's own vocabulary (Claude `session_id`; Codex tree
//!   `session_id` / `thread.id` / `forked_from_id` / `parent_thread_id`);
//! * [`SourceSnapshotRef`]: which bytes were read, when, and under what
//!   cutoff — a content hash says "identical bytes", never "same event";
//! * [`ContextEpochRef`]: a compaction boundary inside one conversation,
//!   which is never a second source.
//!   `SessionModel::session_id` stays as the store-side display handle.

use super::coverage::CoverageReport;
use super::frames::FrameClass;
use super::source::AgentKind;
use serde::{Deserialize, Serialize};

pub const SESSION_MODEL_SCHEMA: &str = "aicx.parser.session_model.v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Known<T> {
    Unknown(UnknownValue),
    Value(T),
}

impl<T> Known<T> {
    pub const fn unknown() -> Self {
        Self::Unknown(UnknownValue::Unknown)
    }

    pub const fn value(value: T) -> Self {
        Self::Value(value)
    }

    pub const fn as_ref(&self) -> Known<&T> {
        match self {
            Self::Value(value) => Known::Value(value),
            Self::Unknown(_) => Known::Unknown(UnknownValue::Unknown),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnknownValue {
    #[serde(rename = "unknown")]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Provenance {
    pub agent: AgentKind,
    pub model: Known<String>,
    pub cli_version: Known<String>,
    pub cwd: Known<String>,
    pub branch: Known<String>,
    pub started_at: Known<String>,
    pub ended_at: Known<String>,
    pub original_source_hash: String,
    pub original_source_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Segment {
    pub segment_id: u32,
    pub cwd: Known<String>,
    pub branch: Known<String>,
    pub started_at: Known<String>,
    pub ended_at: Known<String>,
    pub turn_range: TurnRange,
    /// Whether this segment is one workstream, several, or unknowable from
    /// structure alone. Adapters set it from evidence (cwd known, branch
    /// drift inside the segment); they never guess from content.
    #[serde(default)]
    pub scope_status: ScopeStatus,
}

/// Structural scope verdict for a [`Segment`] or a whole conversation.
///
/// `MixedCandidate` is a candidate, not a verdict: the structure (several
/// working directories or branches inside one span) says the history may
/// braid more than one workstream. Consumers that distill *one* history
/// (`continuity`) must not do so silently on a candidate; they refuse with
/// `RefusalReason::MixedWorkstream` unless told to distill anyway.
/// Topic-level mixing inside one cwd/branch is not detectable here. Absence of
/// drift evidence is not evidence of homogeneity, so the non-mixed state only
/// reports that no drift was observed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ScopeStatus {
    /// One known cwd and no observed branch drift inside the span. This does
    /// not prove that the span contains only one logical workstream.
    NoDriftObserved,
    /// Several cwds and/or branches inside the span.
    MixedCandidate,
    /// No cwd/branch evidence at all: scope cannot be judged.
    #[default]
    Unknown,
}

impl ScopeStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoDriftObserved => "no_drift_observed",
            Self::MixedCandidate => "mixed_candidate",
            Self::Unknown => "unknown",
        }
    }

    /// Combine two spans: any mixed → mixed; otherwise unknown dominates
    /// no-drift evidence only when nothing is known at all.
    pub const fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::MixedCandidate, _) | (_, Self::MixedCandidate) => Self::MixedCandidate,
            (Self::NoDriftObserved, _) | (_, Self::NoDriftObserved) => Self::NoDriftObserved,
            (Self::Unknown, Self::Unknown) => Self::Unknown,
        }
    }

    /// Scope of a span from the evidence it carries: distinct known cwds and
    /// distinct known branches. Unknown values are not counted as a second
    /// workstream (absence of evidence is not evidence of mixing).
    pub fn from_evidence<'a>(
        cwds: impl IntoIterator<Item = &'a str>,
        branches: impl IntoIterator<Item = &'a str>,
    ) -> Self {
        let mut seen_cwd = std::collections::BTreeSet::new();
        for cwd in cwds {
            let cwd = cwd.trim();
            if !cwd.is_empty() {
                seen_cwd.insert(cwd);
            }
        }
        let mut seen_branch = std::collections::BTreeSet::new();
        for branch in branches {
            let branch = branch.trim();
            if !branch.is_empty() {
                seen_branch.insert(branch);
            }
        }
        if seen_cwd.len() > 1 || seen_branch.len() > 1 {
            Self::MixedCandidate
        } else if seen_cwd.len() == 1 {
            Self::NoDriftObserved
        } else {
            Self::Unknown
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillInvocation {
    pub turn_idx: u64,
    pub skill_name: String,
    pub payload_hash: String,
    pub payload_bytes: u64,
    pub first_invoked_at: Known<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnRole {
    User,
    Assistant,
    System,
    Tool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TurnKind {
    UserMsg,
    AgentReply,
    InternalThought,
    ToolCall,
    ToolResult,
    SystemNote,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawUnitRef {
    pub evidence_event_id: String,
    pub coverage_ordinal: u64,
    pub physical_ordinal: u64,
    pub locator: String,
    pub unit_kind: String,
    pub artifact: String,
    pub content_hash: String,
    pub original_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    pub turn_idx: u64,
    pub role: TurnRole,
    pub timestamp: Known<String>,
    pub kind: TurnKind,
    /// Full source text is retained for deterministic projections. The canonical
    /// fingerprint uses only `text_hash` and `text_chars`.
    pub text: String,
    pub text_hash: String,
    pub text_chars: u64,
    pub tool_name: Known<String>,
    pub segment_id: u32,
    pub raw_unit_refs: Vec<RawUnitRef>,
    /// The throne's class for this turn (`engine::frames::classify`), carried
    /// unchanged so consumers read `EchoSeal` / `InterAgent` / `LineageMeta`
    /// from the model instead of re-deriving them from `kind` + `role`.
    /// `None` only for lanes the throne does not own yet (tool call/result,
    /// reasoning, harness events) — those keep `kind` as their lane.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_class: Option<FrameClass>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolEventKind {
    Call,
    Result,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolEvent {
    pub kind: ToolEventKind,
    pub turn_idx: u64,
    pub tool_name: String,
    pub correlation_id: Known<String>,
    pub payload_hash: String,
    pub payload_bytes: u64,
    pub raw_unit_refs: Vec<RawUnitRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CounterSemantics {
    Snapshot,
    Delta,
    Cumulative,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenComponents {
    pub input: Known<u64>,
    pub output: Known<u64>,
    pub reasoning: Known<u64>,
    pub cache_read: Known<u64>,
    pub cache_creation: Known<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReportedCost {
    pub amount: f64,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageSpan {
    pub start: String,
    pub end: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageEvent {
    pub provider: String,
    pub model: Known<String>,
    pub tokens: TokenComponents,
    pub cost: Known<ReportedCost>,
    pub timestamp: Known<String>,
    pub span: Known<UsageSpan>,
    pub counter_semantics: CounterSemantics,
    pub evidence: RawUnitRef,
}

/// Which conversation the provider says this source belongs to, in the
/// provider's own identity vocabulary. Tagged by provider on purpose: a
/// Claude `session_id` names a saved conversation (a fork copies the prefix
/// and mints a new id), a Codex `session_id` names the tree root while
/// `thread.id` names the branch and `parent_thread_id` a sub-agent thread.
/// Flattening these into one string was the W2 hole this type closes.
///
/// Fields the rollout did not carry are `None` and listed in `unobserved`
/// — an absent field is bookkept, never defaulted to a guess.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "provider", rename_all = "snake_case")]
pub enum ProviderConversationRef {
    /// Claude Code JSONL: every row carries `sessionId` (= file stem). A
    /// fork (`/fork`) writes a second file whose prefix repeats the origin's
    /// records with identical `uuid`s; there is no session-level parent
    /// pointer, so fork detection is a catalog concern (shared prefix), not
    /// a field here.
    Claude {
        session_id: String,
        /// Sub-agent lane (`agentId` on sidechain rows) when the file is a
        /// sub-agent transcript rather than the operator conversation.
        agent_id: Option<String>,
        unobserved: Vec<String>,
    },
    /// Codex rollout `session_meta.payload`: `session_id` (tree root),
    /// `id` (this thread), `forked_from_id` (explicit fork origin),
    /// `parent_thread_id` (sub-agent parent), `context_window.window_id`.
    Codex {
        tree_session_id: String,
        thread_id: Option<String>,
        forked_from_id: Option<String>,
        parent_thread_id: Option<String>,
        window_id: Option<String>,
        unobserved: Vec<String>,
    },
    Gemini {
        session_id: String,
        unobserved: Vec<String>,
    },
    Grok {
        session_id: String,
        unobserved: Vec<String>,
    },
    Junie {
        session_id: String,
        unobserved: Vec<String>,
    },
    /// Cursor CLI agent transcript
    /// (`agent-transcripts/<uuid>/<uuid>.jsonl`): the store-side UUID in the
    /// file/directory name is the only identity; rows carry no session, cwd,
    /// or model fields.
    Cursor {
        session_id: String,
        unobserved: Vec<String>,
    },
}

impl ProviderConversationRef {
    /// The reference an adapter starts from before it has read any
    /// provider metadata: only the store id is known, everything else is
    /// unobserved. Adapters replace it once `session_meta` / the first row
    /// has been read.
    pub fn from_store_id(agent: AgentKind, store_id: impl Into<String>) -> Self {
        let store_id = store_id.into();
        match agent {
            AgentKind::Claude => Self::Claude {
                session_id: store_id,
                agent_id: None,
                unobserved: vec!["agent_id".to_owned()],
            },
            AgentKind::Codex => Self::Codex {
                tree_session_id: store_id,
                thread_id: None,
                forked_from_id: None,
                parent_thread_id: None,
                window_id: None,
                unobserved: vec![
                    "thread_id".to_owned(),
                    "forked_from_id".to_owned(),
                    "parent_thread_id".to_owned(),
                    "window_id".to_owned(),
                ],
            },
            AgentKind::Gemini => Self::Gemini {
                session_id: store_id,
                unobserved: Vec::new(),
            },
            AgentKind::Grok => Self::Grok {
                session_id: store_id,
                unobserved: Vec::new(),
            },
            AgentKind::Junie => Self::Junie {
                session_id: store_id,
                unobserved: Vec::new(),
            },
            AgentKind::Cursor => Self::Cursor {
                session_id: store_id,
                unobserved: Vec::new(),
            },
        }
    }

    pub const fn agent(&self) -> AgentKind {
        match self {
            Self::Claude { .. } => AgentKind::Claude,
            Self::Codex { .. } => AgentKind::Codex,
            Self::Cursor { .. } => AgentKind::Cursor,
            Self::Gemini { .. } => AgentKind::Gemini,
            Self::Grok { .. } => AgentKind::Grok,
            Self::Junie { .. } => AgentKind::Junie,
        }
    }

    /// The id that names *this* conversation node (Codex: the thread, not
    /// the tree root). This is what a lineage graph keys its nodes by.
    pub fn node_id(&self) -> &str {
        match self {
            Self::Claude { session_id, .. }
            | Self::Cursor { session_id, .. }
            | Self::Gemini { session_id, .. }
            | Self::Grok { session_id, .. }
            | Self::Junie { session_id, .. } => session_id,
            Self::Codex {
                tree_session_id,
                thread_id,
                ..
            } => thread_id.as_deref().unwrap_or(tree_session_id.as_str()),
        }
    }

    /// Explicit parent pointers the provider wrote down, in priority order:
    /// Codex `forked_from_id` (fork origin) then `parent_thread_id`
    /// (sub-agent parent). Claude has none at session level.
    pub fn declared_parents(&self) -> Vec<(&'static str, &str)> {
        match self {
            Self::Codex {
                forked_from_id,
                parent_thread_id,
                ..
            } => {
                let mut parents = Vec::new();
                if let Some(id) = forked_from_id.as_deref() {
                    parents.push(("forked_from_id", id));
                }
                if let Some(id) = parent_thread_id.as_deref() {
                    parents.push(("parent_thread_id", id));
                }
                parents
            }
            _ => Vec::new(),
        }
    }

    pub fn unobserved(&self) -> &[String] {
        match self {
            Self::Claude { unobserved, .. }
            | Self::Codex { unobserved, .. }
            | Self::Cursor { unobserved, .. }
            | Self::Gemini { unobserved, .. }
            | Self::Grok { unobserved, .. }
            | Self::Junie { unobserved, .. } => unobserved,
        }
    }
}

/// Which bytes were read. This is the identity of a *snapshot* of a
/// conversation, not of the conversation: a turn appended tomorrow changes
/// `content_hash` without any fork, and a Claude fork shares a prefix with
/// its origin while having a different hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSnapshotRef {
    /// Source path as the reader opened it; `Unknown` for in-memory sources.
    pub path: Known<String>,
    /// SHA-256 of the bytes read (`Provenance::original_source_hash`).
    pub content_hash: String,
    pub bytes: u64,
    /// When the snapshot was taken (reader clock, RFC 3339), if recorded.
    pub observed_at: Known<String>,
    /// Last timestamp the snapshot reaches (`Provenance::ended_at`): the
    /// conversation may continue past it.
    pub cutoff: Known<String>,
}

impl SourceSnapshotRef {
    pub fn from_provenance(provenance: &Provenance, path: Known<String>) -> Self {
        Self {
            path,
            content_hash: provenance.original_source_hash.clone(),
            bytes: provenance.original_source_bytes,
            observed_at: Known::unknown(),
            cutoff: provenance.ended_at.clone(),
        }
    }

    /// Same bytes. Says nothing about being the same conversation.
    pub fn same_bytes(&self, other: &Self) -> bool {
        self.content_hash == other.content_hash
    }
}

/// A compaction boundary inside one conversation. Codex: `compacted` /
/// `context_compacted` with `replacement_history`; Claude: a row with
/// `isCompactSummary` + `compactMetadata`. The summary replaces earlier
/// context; the replaced content is referenced, not re-emitted as speech —
/// a compaction is never a second source in a lineage graph.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextEpochRef {
    /// 0-based order of the compaction inside the conversation.
    pub compaction_index: u32,
    /// Evidence event id of the record that carried the summary.
    pub summary_provenance: String,
    /// Content hashes of the replaced history items the record carried
    /// (Codex `replacement_history[i]`); empty when the provider only
    /// signals the boundary.
    pub replacement_refs: Vec<String>,
    /// Provider trigger word when present (Claude `compactMetadata.trigger`,
    /// Codex `payload.reason`).
    pub trigger: Known<String>,
    /// Turn index of the first turn after the boundary, if any turn follows.
    pub first_turn_after: Option<u64>,
}

/// Where an emitted entry comes from once several conversations are laid
/// out together (`--lineage`). `Own` is the requested conversation's own
/// record; `InheritedFrom` says the same record also lives in a parent
/// (Claude fork prefix copy, Codex fork) and is counted once.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum EntryOrigin {
    Own,
    InheritedFrom {
        conversation: ProviderConversationRef,
        /// How the inheritance was established: `shared_prefix` (identical
        /// records in both files), `declared_fork` (provider pointer).
        via: String,
    },
    /// The parent's own continuation beyond the branch point: shown under
    /// `--lineage`, never part of the child's history.
    ParentOnly {
        conversation: ProviderConversationRef,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionModel {
    pub schema: String,
    /// Store-side display handle (`source_id` / logical session id as the
    /// catalog keys it). A projection of `conversation`, kept for every
    /// consumer that prints or files by id; it is not the identity — see
    /// [`ProviderConversationRef`] and [`SourceSnapshotRef`].
    pub session_id: String,
    /// Provider-tagged identity of the conversation.
    #[serde(default = "unset_conversation_ref")]
    pub conversation: ProviderConversationRef,
    /// Identity of the bytes this model was read from.
    #[serde(default = "unset_snapshot_ref")]
    pub snapshot: SourceSnapshotRef,
    /// Compaction boundaries inside this conversation, in order.
    #[serde(default)]
    pub context_epochs: Vec<ContextEpochRef>,
    pub provenance: Provenance,
    pub segments: Vec<Segment>,
    pub skill_invocations: Vec<SkillInvocation>,
    pub turns: Vec<Turn>,
    pub tool_events: Vec<ToolEvent>,
    pub usage_events: Vec<UsageEvent>,
    pub coverage: CoverageReport,
}

fn unset_conversation_ref() -> ProviderConversationRef {
    ProviderConversationRef::Codex {
        tree_session_id: String::new(),
        thread_id: None,
        forked_from_id: None,
        parent_thread_id: None,
        window_id: None,
        unobserved: vec!["deserialized_without_conversation_ref".to_owned()],
    }
}

fn unset_snapshot_ref() -> SourceSnapshotRef {
    SourceSnapshotRef {
        path: Known::unknown(),
        content_hash: String::new(),
        bytes: 0,
        observed_at: Known::unknown(),
        cutoff: Known::unknown(),
    }
}

impl SessionModel {
    /// Build a model whose identity starts from the store id alone: the
    /// conversation ref is `from_store_id` (everything else unobserved) and
    /// the snapshot ref is derived from the provenance hash. Adapters refine
    /// `conversation` once they have read provider metadata.
    pub fn new(
        session_id: impl Into<String>,
        provenance: Provenance,
        coverage: CoverageReport,
    ) -> Self {
        let session_id = session_id.into();
        let conversation = ProviderConversationRef::from_store_id(provenance.agent, &session_id);
        let snapshot = SourceSnapshotRef::from_provenance(&provenance, Known::unknown());
        Self {
            schema: SESSION_MODEL_SCHEMA.to_owned(),
            session_id,
            conversation,
            snapshot,
            context_epochs: Vec::new(),
            provenance,
            segments: Vec::new(),
            skill_invocations: Vec::new(),
            turns: Vec::new(),
            tool_events: Vec::new(),
            usage_events: Vec::new(),
            coverage,
        }
    }

    /// Conversation-level scope: the join of every segment's verdict plus
    /// the cross-segment evidence (two homogeneous segments in different
    /// cwds are one mixed candidate).
    pub fn scope_status(&self) -> ScopeStatus {
        let per_segment = self
            .segments
            .iter()
            .fold(ScopeStatus::Unknown, |acc, segment| {
                acc.join(segment.scope_status)
            });
        let cwds = self
            .segments
            .iter()
            .filter_map(|segment| match &segment.cwd {
                Known::Value(cwd) => Some(cwd.as_str()),
                Known::Unknown(_) => None,
            });
        let branches = self
            .segments
            .iter()
            .filter_map(|segment| match &segment.branch {
                Known::Value(branch) => Some(branch.as_str()),
                Known::Unknown(_) => None,
            });
        per_segment.join(ScopeStatus::from_evidence(cwds, branches))
    }
}
