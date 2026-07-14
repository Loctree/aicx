//! Doctor report model: options, severities, check results, fix
//! identifiers, quarantine manifests, and oracle readiness shapes.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::PathBuf;

use crate::oracle::OracleReadiness;

#[derive(Debug, Clone)]
pub struct DoctorOptions {
    /// Rebuild the steer index from the canonical store if it is corrupted
    /// or schema-incompatible. Wired to CLI flag `--rebuild-steer-index`
    /// (legacy alias: `--fix`, deprecated 2026-05-25). Does NOT address
    /// sidecar coverage, index consistency, or empty-body chunks — those
    /// have dedicated flags.
    pub rebuild_steer_index: bool,
    pub fix_buckets: bool,
    /// When `fix_buckets` is true and `dry_run` is also true, the doctor
    /// emits the planned canonicalize/quarantine actions as `fixes_applied`
    /// entries prefixed with `[dry-run]` but performs **no filesystem
    /// changes**. Lets operators preview a `--fix-buckets` run before
    /// committing to it.
    pub dry_run: bool,
    pub rebuild_sidecars: bool,
    pub prune_empty_bodies: bool,
    pub apply_prune_empty_bodies: bool,
    pub check_dedup: bool,
    pub verbose: bool,
    pub smoke: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone, Copy, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Green,
    #[default]
    Unknown,
    Skipped,
    NotConfigured,
    Warning,
    Critical,
}

#[derive(Debug, Serialize, Deserialize, Clone, PartialEq, Eq)]
pub struct CheckResult {
    pub name: String,
    pub severity: Severity,
    pub detail: String,
    pub recommendation: Option<String>,
}

impl Default for CheckResult {
    fn default() -> Self {
        Self {
            name: "unknown".to_string(),
            severity: Severity::Unknown,
            detail: "not checked".to_string(),
            recommendation: None,
        }
    }
}

fn default_schema_version_2() -> u32 {
    2
}

/// Label for the AICX root used in operator-facing recommendation strings.
///
/// When `$AICX_HOME` is pinned to a non-default location, the operator
/// needs to see the resolved path — otherwise doctor recommendations
/// like "rm -rf ~/.aicx/steer_db" point at the wrong directory. When
/// the env override is unset (or empty), keep the familiar `~/.aicx`
/// literal because that matches what most operators expect and what
/// the existing install docs reference.
pub(crate) fn doctor_home_label() -> String {
    match std::env::var_os("AICX_HOME") {
        Some(value) if !value.is_empty() => PathBuf::from(value).display().to_string(),
        _ => "~/.aicx".to_string(),
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DoctorReport {
    #[serde(default = "default_schema_version_2")]
    pub schema_version: u32,
    pub canonical_store: CheckResult,
    pub steer_lance: CheckResult,
    pub steer_bm25: CheckResult,
    pub state: CheckResult,
    pub sidecars: CheckResult,
    pub corpus_buckets: CheckResult,
    pub noise_health: CheckResult,
    #[serde(default)]
    pub semantic_health: CheckResult,
    #[serde(default)]
    pub index_freshness: CheckResult,
    #[serde(default)]
    pub index_consistency: CheckResult,
    #[serde(default)]
    pub sidecar_coverage: CheckResult,
    #[serde(default)]
    pub embedder_warmth: CheckResult,
    #[serde(default)]
    pub empty_body_chunks: CheckResult,
    #[serde(default)]
    pub content_dedup: CheckResult,
    #[serde(default)]
    pub context_corpus: CheckResult,
    /// Informational: which AICX_HOME the runtime resolved, whether it is
    /// pinned via env, and whether store/indexed live there. Not part of
    /// `overall` — diagnostic, not a gate.
    #[serde(default)]
    pub aicx_home: CheckResult,
    /// Informational: aicx CLI vs aicx-mcp version parity on PATH. Catches the
    /// "fresh CLI, stale MCP service" drift class. Not part of `overall`.
    #[serde(default)]
    pub binary_pair: CheckResult,
    /// Informational: where the HTTP auth token resolves from (env / file /
    /// would-generate). Never exposes the token value. Not part of `overall`.
    #[serde(default)]
    pub http_auth_token: CheckResult,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rebuild_sidecars_script: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prune_empty_bodies_script: Option<String>,
    pub fixes_applied: Vec<String>,
    pub overall: Severity,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DoctorFixId {
    RebuildSteerIndex,
    QuarantineBuckets,
    QuarantineEmptyBodies,
}

impl DoctorFixId {
    pub(crate) fn title(self) -> &'static str {
        match self {
            Self::RebuildSteerIndex => "Rebuild steer index",
            Self::QuarantineBuckets => "Quarantine suspicious corpus buckets",
            Self::QuarantineEmptyBodies => "Quarantine empty-body chunks",
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct DoctorFixChoice {
    pub(crate) id: DoctorFixId,
    pub(crate) title: String,
    pub(crate) detail: String,
}

impl fmt::Display for DoctorFixChoice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} -- {}", self.title, self.detail)
    }
}

#[derive(Debug, Serialize)]
pub struct DoctorDryRunPreview {
    pub fix: DoctorFixId,
    pub title: String,
    pub summary: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct DoctorApplyPhase {
    pub fix: DoctorFixId,
    pub title: String,
    pub status: String,
    pub detail: String,
    pub elapsed_ms: u128,
}

#[derive(Debug, Serialize)]
pub struct DoctorCleanupRunReport {
    pub mode: String,
    pub selected: Vec<DoctorFixId>,
    pub dry_run: Vec<DoctorDryRunPreview>,
    pub applied: Vec<DoctorApplyPhase>,
    pub final_report: DoctorReport,
}

#[derive(Debug, Serialize, Deserialize, Default)]
pub struct QuarantineManifest {
    #[serde(default = "default_quarantine_manifest_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub slug: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub items: Vec<QuarantineManifestItem>,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct QuarantineManifestItem {
    #[serde(default)]
    pub original_path: PathBuf,
    #[serde(default)]
    pub quarantined_path: PathBuf,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct QuarantineRestoreReport {
    pub slug: String,
    pub manifest_path: PathBuf,
    pub restored: usize,
    pub skipped: usize,
    pub failures: Vec<String>,
}

fn default_quarantine_manifest_schema() -> u32 {
    1
}

#[derive(Debug, Serialize)]
pub struct OracleReadinessReport {
    pub readiness: OracleReadiness,
    pub readiness_label: &'static str,
    pub canonical_corpus_health: Severity,
    pub metadata_steer_index_health: Severity,
    pub content_semantic_index_health: Severity,
    pub dashboard_semantic_route_health: Severity,
    pub loctree_oracle_readiness: OracleReadiness,
    pub reason: String,
}
