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

#[derive(Debug, Clone)]
pub struct CorpusValidateOptions {
    pub roots: Vec<PathBuf>,
    pub strict: bool,
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
pub struct CorpusValidateReport {
    pub roots: Vec<RootValidateReport>,
    pub totals: CorpusValidateTotals,
    pub strict: bool,
    pub passed: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CorpusValidateTotals {
    pub roots_present: usize,
    pub roots_missing: usize,
    pub cards: usize,
    pub ok: usize,
    pub warn: usize,
    pub error: usize,
    pub hard_violations: usize,
    pub warnings: usize,
    pub violations_by_class: BTreeMap<String, usize>,
    pub warnings_by_class: BTreeMap<String, usize>,
    pub verdicts: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RootValidateReport {
    pub root: PathBuf,
    pub present: bool,
    pub cards: usize,
    pub ok: usize,
    pub warn: usize,
    pub error: usize,
    pub hard_violations: usize,
    pub warnings: usize,
    pub violations_by_class: BTreeMap<String, usize>,
    pub warnings_by_class: BTreeMap<String, usize>,
    pub verdicts: BTreeMap<String, usize>,
    pub samples: Vec<CorpusCardFinding>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CorpusCardFinding {
    pub path: PathBuf,
    pub sidecar_path: Option<PathBuf>,
    pub severity: String,
    pub class: String,
    pub message: String,
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
    /// Total files counted as skipped — equals
    /// `skipped_charter_protected + skipped_other`. Kept for back-compat with
    /// existing manifest consumers; new code should prefer the explicit
    /// breakdown fields below.
    pub skipped_files: usize,
    /// Files whose only detected noise is charter-protected (e.g.
    /// `internal_thought_frame`). Repair leaves these untouched by design —
    /// the charter forbids inventing/summarizing semantic content, so they
    /// require human review rather than being a deterministic-repair target.
    pub skipped_charter_protected: usize,
    /// Files that had repair candidates but were not modified for reasons
    /// other than charter protection (e.g. repair was already idempotent on
    /// the detected noise classes).
    pub skipped_other: usize,
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

    /// Charter-protected classes carry semantic content the deterministic
    /// repair must never invent, summarize, or strip. They are surfaced for
    /// human review, not auto-repair.
    pub(super) fn is_charter_protected(&self) -> bool {
        matches!(self, Self::InternalThoughtFrame)
    }
}

pub(super) type NoiseSet = BTreeSet<NoiseClass>;

/// Reason a candidate file ended up in the skipped bucket. Lets the manifest
/// distinguish design-by-charter skips ("human review required") from generic
/// no-op skips.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SkipReason {
    /// Every detected noise class is charter-protected. Repair must not touch
    /// the file; the operator needs to review it manually.
    CharterProtected,
    /// File had at least one repairable noise class but the deterministic
    /// repair produced identical content (already clean modulo detection).
    NoChange,
}

pub(super) fn classify_skip(noise: &NoiseSet) -> SkipReason {
    if !noise.is_empty() && noise.iter().all(NoiseClass::is_charter_protected) {
        SkipReason::CharterProtected
    } else {
        SkipReason::NoChange
    }
}
