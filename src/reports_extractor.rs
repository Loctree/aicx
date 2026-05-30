//! Vibecrafted artifact report extractor and standalone HTML explorer.
//!
//! Scans `$HOME/.vibecrafted/artifacts/<org>/<repo>/...` style trees, merges markdown
//! reports with optional `.meta.json` companions, and produces a shareable HTML
//! artifact plus JSON bundle for client-side re-import.

mod assets;
mod render;
mod scan;
mod support;
mod types;

pub use types::{
    ReportsExplorerPayload, ReportsExplorerRecord, ReportsExplorerStats, ReportsExtractorArtifact,
    ReportsExtractorConfig,
};

use anyhow::{Context, Result};

/// Build a standalone HTML explorer and JSON bundle from Vibecrafted artifacts.
pub fn build_reports_explorer(config: &ReportsExtractorConfig) -> Result<ReportsExtractorArtifact> {
    let artifacts_root = crate::sanitize::validate_dir_path(&config.artifacts_root)?;
    let repo_root = artifacts_root.join(&config.org).join(&config.repo);
    let repo_root = crate::sanitize::validate_dir_path(&repo_root).with_context(|| {
        format!(
            "Artifacts repo not found: {}/{} under {}",
            config.org,
            config.repo,
            artifacts_root.display()
        )
    })?;
    let payload = scan::scan_reports(&repo_root, &artifacts_root, config)?;
    let bundle_json =
        serde_json::to_string_pretty(&payload).context("Failed to serialize reports bundle")?;
    let html = render::render_reports_html(&payload, &config.title)?;

    Ok(ReportsExtractorArtifact {
        html,
        bundle_json,
        stats: payload.stats.clone(),
        assumptions: payload.assumptions.clone(),
    })
}

#[cfg(test)]
mod tests;
