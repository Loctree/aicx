//! Shared timeline and segmentation data types.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use chrono::{DateTime, Utc};
use clap::ValueEnum;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::fmt;

/// Canonical kind for a session segment in the store.
///
/// Kind determines the subdirectory under `<project>/<date>/` and is part
/// of the canonical store path. Classification is conservative: when in
/// doubt, segments fall through to `Other`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Conversations,
    Plans,
    Reports,
    #[default]
    Other,
}

impl Kind {
    /// Directory name used in the canonical store layout.
    pub fn dir_name(self) -> &'static str {
        match self {
            Self::Conversations => "conversations",
            Self::Plans => "plans",
            Self::Reports => "reports",
            Self::Other => "other",
        }
    }

    /// Parse from a string (case-insensitive, accepts both singular and plural).
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "conversations" | "conversation" => Some(Self::Conversations),
            "plans" | "plan" => Some(Self::Plans),
            "reports" | "report" => Some(Self::Reports),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.dir_name())
    }
}

/// Canonical stream/frame classification for a timeline entry or stored chunk.
///
/// This axis is intentionally orthogonal to `role`: source formats drift in how
/// they spell assistant reasoning or tool payloads, but downstream retrieval
/// needs one stable vocabulary for "which channel is this?".
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ValueEnum, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
#[value(rename_all = "snake_case")]
pub enum FrameKind {
    UserMsg,
    AgentReply,
    InternalThought,
    ToolCall,
}

impl FrameKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UserMsg => "user_msg",
            Self::AgentReply => "agent_reply",
            Self::InternalThought => "internal_thought",
            Self::ToolCall => "tool_call",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "user_msg" | "user" => Some(Self::UserMsg),
            "agent_reply" | "assistant" | "reply" => Some(Self::AgentReply),
            "internal_thought" | "thought" | "thinking" | "reasoning" => {
                Some(Self::InternalThought)
            }
            "tool_call" | "tool" | "tool_result" | "function_call" => Some(Self::ToolCall),
            _ => None,
        }
    }
}

impl fmt::Display for FrameKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Unified timeline entry from any AI agent source.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimelineEntry {
    pub timestamp: DateTime<Utc>,
    pub agent: String,
    pub session_id: String,
    pub role: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame_kind: Option<FrameKind>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// Denoised conversation message — the canonical projection of a TimelineEntry
/// containing only user/assistant messages with repo-centric identity.
///
/// This is the primary unit for "recover the conversation" workflows.
/// Tool calls, tool results, reasoning/thoughts, system noise, and artifact
/// payloads are excluded. Artifact paths may appear as references only.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationMessage {
    pub timestamp: DateTime<Utc>,
    pub agent: String,
    pub session_id: String,
    /// Only "user" or "assistant" — reasoning and system roles are excluded.
    pub role: String,
    /// Raw, untrimmed, untruncated message body.
    pub message: String,
    /// Canonical project/repo identity (derived from cwd + project filter).
    pub repo_project: String,
    /// Secondary provenance: source working directory path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    /// Git branch at time of message (when available).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
}

/// Explicit trust tier for a repo identity signal.
///
/// Not all evidence for "which repo is this?" is equal. A git remote URL
/// is canonical truth; a directory layout is a strong hint; a hex hash is
/// opaque noise. This enum makes the distinction machine-readable so the
/// store can decide whether to assert identity or route to fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub enum SourceTier {
    /// Git remote URL or explicit GitHub/GitLab link in message text.
    /// The strongest signal — the repo literally named itself.
    Primary,
    /// Local git repo discovered on disk (via `.git/` traversal + known layout),
    /// or a projectHash resolved through a trustworthy local mapping file.
    Secondary,
    /// Known directory layout (e.g. `~/hosted/<org>/<repo>`) without a `.git/`
    /// directory or remote confirmation. Plausible but not proven.
    Fallback,
    /// Hex hash, opaque identifier, or source that is explicitly not a
    /// conversation (e.g. `.pb` protobuf, step-output). Must never assert
    /// repo identity on its own.
    Opaque,
}

impl SourceTier {
    /// Whether this tier is strong enough to assert repo identity for
    /// canonical store placement (under `store/<org>/<repo>/`).
    pub fn is_assertable(self) -> bool {
        matches!(self, Self::Primary | Self::Secondary)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RepoIdentity {
    pub organization: String,
    pub repository: String,
}

impl RepoIdentity {
    pub fn slug(&self) -> String {
        format!("{}/{}", self.organization, self.repository)
    }
}

#[derive(Debug, Clone)]
pub struct SemanticSegment {
    pub repo: Option<RepoIdentity>,
    /// The trust tier of the strongest signal that produced `repo`.
    /// `None` when `repo` is `None`.
    pub source_tier: Option<SourceTier>,
    pub kind: Kind,
    pub agent: String,
    pub session_id: String,
    pub entries: Vec<TimelineEntry>,
}

impl SemanticSegment {
    pub fn project_label(&self) -> String {
        self.repo
            .as_ref()
            .map(RepoIdentity::slug)
            .unwrap_or_else(|| "non-repository-contexts".to_string())
    }

    /// Whether the repo identity is strong enough for canonical store placement.
    /// Returns `false` for `None` repo or Fallback/Opaque tiers.
    pub fn has_assertable_identity(&self) -> bool {
        self.source_tier.is_some_and(SourceTier::is_assertable)
    }
}
