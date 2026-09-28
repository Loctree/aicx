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

/// Version of the `SessionModel` contract.
///
/// `v2` (W2-R1 follow-up) widens [`ScopeStatus`] with `unattributed` and adds
/// [`Segment::scope_root`]. Both are additive, but a decoder written against
/// the `v1` grammar treats `ScopeStatus` as closed and rejects the new value
/// outright, so the widening is announced rather than slipped in. The emitted
/// grammar itself is [`ScopeStatus::ALL`], which
/// `docs/OUTPUT_PROJECTION_CONTRACT.md` is held to by a contract test.
pub const SESSION_MODEL_SCHEMA: &str = "aicx.parser.session_model.v2";

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
    /// Explicit tool-call workdirs inside this span proved two repository
    /// identities. This is NOT the same fact as `scope_status`: a segment is
    /// a `MixedCandidate` for ordinary branch drift inside one unchanged
    /// checkout, which is fully attributable. Only a proven workdir conflict
    /// leaves the span with no repository to belong to, so downstream filters
    /// read this flag instead of inferring it from the generic status.
    #[serde(default)]
    pub scope_conflict: bool,
    /// Repository identity this span's explicit tool-call workdirs resolved
    /// to ON THIS HOST, when they agreed on one.
    ///
    /// Deliberately separate from [`Self::cwd`], which stays the fact the
    /// rollout RECORDED. Resolving an identity reads the local filesystem —
    /// which checkouts exist, how symlinks resolve, what `.gitmodules`
    /// declares — so folding it into `cwd` made the canonical fingerprint of
    /// identical source bytes differ from machine to machine. This field is
    /// excluded from the canonical projection for that reason; consumers that
    /// want the resolved bucket read it explicitly and fall back to `cwd`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope_root: Option<String>,
    /// Explicit tool-call workdirs this span's turn windows RECORDED, joined
    /// onto the window's baseline and lexically normalized — no filesystem
    /// reads. Empty when no window in the span named a workdir.
    ///
    /// This is the evidence the scope verdict was drawn from, kept so a
    /// privacy filter can judge every checkout the span touched: a conflict
    /// window has no single scope to test against `.aicxignore`, and a
    /// window absorbed into its baseline still ran inside the paths it names.
    /// Like `scope_root` it stays out of the canonical projection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub scope_workdirs: Vec<String>,
}

/// Structural scope verdict for a [`Segment`] or a whole conversation.
///
/// `MixedCandidate` is a candidate, not a verdict: the structure (several
/// working directories or branches inside one span) says the history may
/// braid more than one workstream. It also covers a branch switch inside one
/// checkout, so consumers that distill *one* history (`continuity`) refuse
/// with `RefusalReason::MixedWorkstream` on proven evidence of a second
/// scope (a conflict, a hidden scope, another cwd), not on this value alone.
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
    /// Explicit scope evidence exists but does not resolve (historical or
    /// foreign-machine workdir that is not the baseline): the span must not
    /// inherit any project bucket downstream.
    Unattributed,
    /// No cwd/branch evidence at all: scope cannot be judged.
    #[default]
    Unknown,
}

impl ScopeStatus {
    /// Every variant, in the order the contract documents them.
    ///
    /// The one place the emitted grammar is enumerated, so that the
    /// contract document can be held to the code rather than maintained
    /// beside it: a value listed here and missing from
    /// `docs/OUTPUT_PROJECTION_CONTRACT.md` fails the contract test in
    /// `crates/aicx-parser/tests/normative_contract.rs`. The `homogeneous`
    /// → `no_drift_observed` rename reached consumers with that document
    /// still naming the old value, and `unattributed` repeated it; this is
    /// what stops a third round.
    ///
    /// Adding a variant means adding it here too — the exhaustive matches
    /// below name every variant, but nothing forces this array to grow, so
    /// it is a convention the test enforces one step later, not a compiler
    /// guarantee.
    pub const ALL: [Self; 4] = [
        Self::NoDriftObserved,
        Self::MixedCandidate,
        Self::Unattributed,
        Self::Unknown,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NoDriftObserved => "no_drift_observed",
            Self::MixedCandidate => "mixed_candidate",
            Self::Unattributed => "unattributed",
            Self::Unknown => "unknown",
        }
    }

    /// Combine two spans: any unattributed → unattributed; otherwise any
    /// mixed → mixed; unknown survives only when nothing is known at all.
    ///
    /// `Unattributed` outranks `MixedCandidate` because the two are different
    /// facts and only one of them forbids attribution: mixed also covers an
    /// ordinary branch switch inside one checkout, which whole-session
    /// attribution survives, while unattributed means evidence this host
    /// cannot place. Letting branch drift absorb it would hand a span with
    /// unplaceable evidence the one status that no longer refuses a bucket.
    /// Mixing is not lost by this: consumers that must refuse a braided
    /// history decide on the proven conflicts and cwds, not on this enum.
    pub const fn join(self, other: Self) -> Self {
        match (self, other) {
            (Self::Unattributed, _) | (_, Self::Unattributed) => Self::Unattributed,
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
    /// Kimi Code CLI `wire.jsonl`: one file per agent lane under
    /// `session_<uuid>/agents/<agentId>/`; `session_id` names the session
    /// directory (tree root), `agent_id` the lane (`main` or a subagent id).
    Kimi {
        session_id: String,
        agent_id: Option<String>,
        unobserved: Vec<String>,
    },
    /// Cursor agent-transcript JSONL: store id is the filename UUID
    /// (`~/.cursor/projects/<slug>/agent-transcripts/<uuid>/<uuid>.jsonl`).
    /// Records do not carry a session field; `worker_id` lives on the
    /// cursor-agent-worker process, not in the transcript.
    Cursor {
        session_id: String,
        worker_id: Option<String>,
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
            AgentKind::Kimi => Self::Kimi {
                session_id: store_id,
                agent_id: None,
                unobserved: vec!["agent_id".to_owned()],
            },
            AgentKind::Cursor => Self::Cursor {
                session_id: store_id,
                worker_id: None,
                unobserved: vec!["worker_id".to_owned()],
            },
        }
    }

    pub const fn agent(&self) -> AgentKind {
        match self {
            Self::Claude { .. } => AgentKind::Claude,
            Self::Codex { .. } => AgentKind::Codex,
            Self::Gemini { .. } => AgentKind::Gemini,
            Self::Grok { .. } => AgentKind::Grok,
            Self::Junie { .. } => AgentKind::Junie,
            Self::Kimi { .. } => AgentKind::Kimi,
            Self::Cursor { .. } => AgentKind::Cursor,
        }
    }

    /// The id that names *this* conversation node (Codex: the thread, not
    /// the tree root). This is what a lineage graph keys its nodes by.
    pub fn node_id(&self) -> &str {
        match self {
            Self::Claude { session_id, .. }
            | Self::Gemini { session_id, .. }
            | Self::Grok { session_id, .. }
            | Self::Junie { session_id, .. }
            | Self::Kimi { session_id, .. }
            | Self::Cursor { session_id, .. } => session_id,
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
            | Self::Gemini { unobserved, .. }
            | Self::Grok { unobserved, .. }
            | Self::Junie { unobserved, .. }
            | Self::Kimi { unobserved, .. }
            | Self::Cursor { unobserved, .. } => unobserved,
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
    ///
    /// A segment's workdirs count as cwd evidence through its `scope_root`,
    /// next to the recorded cwd: a turn window re-scoped to another checkout
    /// keeps the session's recorded cwd, so reading `cwd` alone reported a
    /// session that worked in two repositories as one with no drift.
    pub fn scope_status(&self) -> ScopeStatus {
        let per_segment = self
            .segments
            .iter()
            .fold(ScopeStatus::Unknown, |acc, segment| {
                acc.join(segment.scope_status)
            });
        let cwds = self.segments.iter().flat_map(|segment| {
            let recorded = match &segment.cwd {
                Known::Value(cwd) => Some(cwd.as_str()),
                Known::Unknown(_) => None,
            };
            recorded.into_iter().chain(segment.scope_root.as_deref())
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
