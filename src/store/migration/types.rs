use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LegacyItemKind {
    ContextBundle,
    LooseFile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationAction {
    Rebuild,
    RebuildAndSalvage,
    Salvage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationExecution {
    Planned,
    Executed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationTotals {
    pub total_items: usize,
    pub total_legacy_files: usize,
    pub rebuild_items: usize,
    pub rebuild_and_salvage_items: usize,
    pub salvage_items: usize,
    pub unclassified_items: usize,
    pub resolved_sources: usize,
    pub missing_source_hints: usize,
    pub ambiguous_source_hints: usize,
    pub rebuilt_items: usize,
    pub salvaged_items: usize,
    pub rebuilt_paths: usize,
    pub salvaged_paths: usize,
    pub failed_items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationManifest {
    pub generated_at: DateTime<Utc>,
    pub legacy_root: String,
    pub store_root: String,
    pub manifest_path: String,
    pub report_path: String,
    pub dry_run: bool,
    pub totals: MigrationTotals,
    pub items: Vec<MigrationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationItem {
    pub item_id: String,
    pub legacy_kind: LegacyItemKind,
    pub legacy_group: String,
    pub legacy_files: Vec<String>,
    pub agent_hint: Option<String>,
    pub date_hint: Option<String>,
    pub source_hints: Vec<String>,
    pub existing_sources: Vec<String>,
    pub missing_sources: Vec<String>,
    pub ambiguous_sources: Vec<String>,
    pub action: MigrationAction,
    pub action_reason: String,
    pub execution: MigrationExecution,
    pub canonical_paths: Vec<String>,
    pub salvage_paths: Vec<String>,
    pub errors: Vec<String>,
}
