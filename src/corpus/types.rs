use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

#[derive(Debug, Clone)]
pub struct CorpusAuditOptions {
    pub roots: Vec<PathBuf>,
}

#[derive(Debug, Clone)]
pub struct CorpusRepairOptions {
    pub roots: Vec<PathBuf>,
    pub dry_run: bool,
    pub apply: bool,
    pub backup: bool,
    pub manifest_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusAuditReport {
    pub roots: Vec<RootAuditReport>,
    pub totals: CorpusAuditTotals,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CorpusAuditTotals {
    pub roots_present: usize,
    pub roots_missing: usize,
    pub markdown_files: usize,
    pub files_with_noise: usize,
    pub noise_classes: BTreeMap<String, usize>,
    pub agents: BTreeMap<String, usize>,
    pub frame_kinds: BTreeMap<String, usize>,
    pub path_dates: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RootAuditReport {
    pub root: PathBuf,
    pub present: bool,
    pub markdown_files: usize,
    pub files_with_noise: usize,
    pub noise_classes: BTreeMap<String, usize>,
    pub agents: BTreeMap<String, usize>,
    pub frame_kinds: BTreeMap<String, usize>,
    pub path_dates: BTreeMap<String, usize>,
    pub artifact_birthtime_dates: BTreeMap<String, usize>,
    pub artifact_mtime_dates: BTreeMap<String, usize>,
    pub examples: Vec<CorpusFileFinding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusFileFinding {
    pub path: PathBuf,
    pub agent: String,
    pub frame_kind: Option<String>,
    pub path_date: Option<String>,
    pub artifact_birthtime: Option<String>,
    pub artifact_mtime: Option<String>,
    pub noise_classes: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusRepairManifest {
    pub repair_version: String,
    pub generated_at: String,
    pub dry_run: bool,
    pub apply: bool,
    pub backup: bool,
    pub roots: Vec<PathBuf>,
    pub scanned_markdown_files: usize,
    pub candidates: usize,
    pub repaired_files: usize,
    pub skipped_files: usize,
    pub manifest_path: Option<PathBuf>,
    pub items: Vec<CorpusRepairItem>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusRepairItem {
    pub path: PathBuf,
    pub action: String,
    pub backup_path: Option<PathBuf>,
    pub sidecar_path: PathBuf,
    pub removed_noise_classes: Vec<String>,
    pub original_content_hash: String,
    pub repaired_content_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(super) enum NoiseClass {
    Signature,
    ThoughtSignature,
    EmptyThinking,
    InlineThinkingJson,
    InternalThoughtFrame,
    MassiveToolJson,
}

impl NoiseClass {
    pub(super) fn as_str(&self) -> &'static str {
        match self {
            Self::Signature => "signature",
            Self::ThoughtSignature => "thoughtSignature",
            Self::EmptyThinking => "empty_thinking",
            Self::InlineThinkingJson => "inline_thinking_json",
            Self::InternalThoughtFrame => "internal_thought_frame",
            Self::MassiveToolJson => "massive_tool_json",
        }
    }
}

pub(super) type NoiseSet = BTreeSet<NoiseClass>;
