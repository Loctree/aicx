use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub struct ReportsExtractorConfig {
    /// Vibecrafted artifacts root (default from CLI: ~/.vibecrafted/artifacts).
    pub artifacts_root: PathBuf,
    /// Org filter under the artifacts root.
    pub org: String,
    /// Repo name under the artifacts root.
    pub repo: String,
    /// Inclusive start date filter.
    pub date_from: Option<NaiveDate>,
    /// Inclusive end date filter.
    pub date_to: Option<NaiveDate>,
    /// Optional workflow/path filter (case-insensitive substring).
    pub workflow: Option<String>,
    /// HTML document title.
    pub title: String,
    /// Max characters in record previews (0 = no truncation).
    pub preview_chars: usize,
    /// If true, derive `generated_at` from the latest record timestamp instead
    /// of wall-clock `Utc::now()`. Bit-for-bit reproducible runs on a frozen
    /// artifact tree.
    pub deterministic: bool,
}

/// Generation output for the standalone reports explorer.
#[derive(Debug, Clone)]
pub struct ReportsExtractorArtifact {
    /// Rendered standalone HTML page.
    pub html: String,
    /// Pretty JSON bundle with the same embedded payload.
    pub bundle_json: String,
    /// Aggregate stats for CLI output.
    pub stats: ReportsExplorerStats,
    /// Scan assumptions surfaced to the operator and the HTML.
    pub assumptions: Vec<String>,
}

/// Aggregate stats for the explorer payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ReportsExplorerStats {
    pub total_records: usize,
    pub total_reports: usize,
    pub total_plans: usize,
    pub total_meta_only: usize,
    pub total_transcript_backed: usize,
    pub completed_records: usize,
    pub incomplete_records: usize,
    pub total_days: usize,
    pub total_workflows: usize,
    pub total_agents: usize,
    pub avg_duration_s: Option<f64>,
}

/// Embedded JSON payload consumed by the standalone HTML app.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportsExplorerPayload {
    pub schema_version: u32,
    pub generated_at: String,
    pub artifacts_root: String,
    pub resolved_org: String,
    pub resolved_repo: String,
    pub scan_root: String,
    pub selected_date: Option<String>,
    pub selected_workflow: Option<String>,
    pub stats: ReportsExplorerStats,
    pub assumptions: Vec<String>,
    pub workflows: Vec<String>,
    pub agents: Vec<String>,
    pub statuses: Vec<String>,
    pub lanes: Vec<String>,
    pub days: Vec<String>,
    pub records: Vec<ReportsExplorerRecord>,
}

/// One workflow/report artifact entry shown in the HTML explorer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportsExplorerRecord {
    pub id: usize,
    pub key: String,
    pub org: String,
    pub repo: String,
    pub workflow: String,
    pub lane: String,
    pub record_kind: String,
    pub status: String,
    pub agent: String,
    pub skill_code: Option<String>,
    pub mode: Option<String>,
    pub run_id: Option<String>,
    pub prompt_id: Option<String>,
    pub session_id: Option<String>,
    pub date_bucket: String,
    pub date_iso: String,
    pub title: String,
    pub file_name: String,
    pub relative_path: String,
    pub absolute_path: String,
    pub meta_path: Option<String>,
    pub transcript_path: Option<String>,
    pub input_path: Option<String>,
    pub launcher_path: Option<String>,
    pub updated_at: Option<String>,
    pub completed_at: Option<String>,
    pub duration_s: Option<f64>,
    pub loop_nr: Option<u32>,
    pub headings: Vec<String>,
    pub preview: String,
    pub detail_text: String,
    pub search_blob: String,
    pub has_markdown: bool,
    pub has_meta: bool,
    pub has_transcript: bool,
    pub sort_ts: i64,
}

#[derive(Debug, Default, Clone)]
pub(super) struct Candidate {
    pub(super) md_path: Option<PathBuf>,
    pub(super) meta_path: Option<PathBuf>,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub(super) struct ArtifactMeta {
    pub(super) updated_at: Option<String>,
    pub(super) status: Option<String>,
    pub(super) agent: Option<String>,
    pub(super) mode: Option<String>,
    pub(super) input: Option<String>,
    pub(super) report: Option<String>,
    pub(super) transcript: Option<String>,
    pub(super) launcher: Option<String>,
    pub(super) prompt_id: Option<String>,
    pub(super) run_id: Option<String>,
    pub(super) loop_nr: Option<u32>,
    pub(super) skill_code: Option<String>,
    pub(super) exit_code: Option<i32>,
    pub(super) completed_at: Option<String>,
    pub(super) duration_s: Option<f64>,
    pub(super) session_id: Option<String>,
}

#[derive(Debug, Default, Deserialize, Clone)]
pub(super) struct ArtifactFrontmatterEnvelope {
    pub(super) status: Option<String>,
    pub(super) created: Option<String>,
    #[serde(flatten)]
    pub(super) report: crate::frontmatter::ReportFrontmatter,
}

#[derive(Debug, Default, Clone)]
pub(super) struct DateFilter {
    pub(super) start: Option<NaiveDate>,
    pub(super) end: Option<NaiveDate>,
}
