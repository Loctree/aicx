//! Shared canonical types used across segmentation and store.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::sources::TimelineEntry;

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
