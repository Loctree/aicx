//! Shared canonical types used by the intent engine.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fmt;
use std::hash::{Hash, Hasher};

pub use crate::timeline::{FrameKind, Kind, RepoIdentity, SemanticSegment, SourceTier};

// ── Intent Engine schema ─────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryType {
    Intent,
    Why,
    Argue,
    Decision,
    Assumption,
    Outcome,
    Result,
    Question,
    Insight,
}

impl EntryType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Why => "why",
            Self::Argue => "argue",
            Self::Decision => "decision",
            Self::Assumption => "assumption",
            Self::Outcome => "outcome",
            Self::Result => "result",
            Self::Question => "question",
            Self::Insight => "insight",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "intent" => Some(Self::Intent),
            "why" => Some(Self::Why),
            "argue" | "argument" | "debate" => Some(Self::Argue),
            "decision" => Some(Self::Decision),
            "assumption" | "hypothesis" => Some(Self::Assumption),
            "outcome" => Some(Self::Outcome),
            "result" => Some(Self::Result),
            "question" => Some(Self::Question),
            "insight" => Some(Self::Insight),
            _ => None,
        }
    }
}

impl fmt::Display for EntryType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum EntryState {
    Proposed,
    Active,
    Superseded,
    Done,
    Contradicted,
}

impl EntryState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Proposed => "proposed",
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Done => "done",
            Self::Contradicted => "contradicted",
        }
    }
}

impl fmt::Display for EntryState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LinkType {
    DerivedFrom,
    Supersedes,
    Verifies,
    Contradicts,
    Supports,
    ResultsIn,
    Answers,
    LinksTo,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Link {
    pub relation: LinkType,
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub confidence: Option<f32>,
}

impl Eq for Link {}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IntentEntry {
    pub id: String,
    pub entry_type: EntryType,
    pub state: EntryState,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub links: Vec<Link>,
    pub confidence: f32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    pub date: String,
    pub source_chunk: String,
}

impl Eq for IntentEntry {}

impl IntentEntry {
    pub fn stable_id(source_chunk: &str, byte_offset: usize, entry_type: EntryType) -> String {
        let mut hasher = DefaultHasher::new();
        source_chunk.hash(&mut hasher);
        byte_offset.hash(&mut hasher);
        entry_type.as_str().hash(&mut hasher);
        format!("{:016x}", hasher.finish())
    }
}
