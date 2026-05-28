//! Junie provider extraction logic.
//!
//! This file is part of the 2026-05-28 "rozpierducha która leczy" wave.
//! Most of the real implementation is still in legacy.rs and will be moved here.

pub(crate) enum JunieSessionWarning {
    JunieFallbackId {
        fallback: String,
    },
    OversizedLine {
        count: usize,
        samples: Vec<String>,
    },
    ContentSanitization {
        warning: crate::sanitize::ContentSanitizationWarning,
    },
}

// Stub implementations during the wave
impl JunieSessionWarning {
    pub(crate) fn describe(&self, _path: &std::path::Path) -> String {
        "Junie warning (stub during extraction wave)".to_string()
    }
}

pub(crate) fn emit_junie_session_warnings(
    _path: &std::path::Path,
    _warnings: &[JunieSessionWarning],
) {
    // Will be properly implemented when the full Junie block is moved out of legacy.rs
}

// Public API surface (to be filled)
pub fn extract_junie(
    _config: &crate::timeline::ExtractionConfig,
) -> anyhow::Result<Vec<crate::timeline::TimelineEntry>> {
    // TODO: Move real implementation from legacy.rs
    Ok(vec![])
}

pub fn extract_junie_file(
    _path: &std::path::Path,
    _config: &crate::timeline::ExtractionConfig,
) -> anyhow::Result<Vec<crate::timeline::TimelineEntry>> {
    // TODO: Move real implementation from legacy.rs
    Ok(vec![])
}

// Note for future agents / human review:
// This module was created as the first deliberate move in the sources decomposition
// under the Vista "Dead Parrot Spaghetti" operating paradigm.
// The wave is intentionally incomplete while the operator is away.
