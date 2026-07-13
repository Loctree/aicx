//! Temporary sealed vessel after removal of the legacy session parser engine.
//!
//! This module deliberately contains no parsing, discovery, fallback, or
//! compatibility implementation. It preserves compile-time call boundaries
//! while the deterministic `aicx-parser` engine is installed by the scaffold.

use anyhow::{Result, bail};
use std::path::Path;

use crate::timeline::{ExtractionConfig, TimelineEntry};

const REMOVED: &str = "legacy session parser removed; install the deterministic aicx-parser engine and use `aicx extract <agent> ...`";

#[cold]
fn unavailable<T>(agent: &str) -> Result<T> {
    bail!("{REMOVED} (agent: {agent})")
}

macro_rules! bulk_boundary {
    ($name:ident, $agent:literal) => {
        pub fn $name(_config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
            unavailable($agent)
        }
    };
}

macro_rules! file_boundary {
    ($name:ident, $agent:literal) => {
        pub fn $name(_path: &Path, _config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
            unavailable($agent)
        }
    };
}

bulk_boundary!(extract_claude, "claude");
bulk_boundary!(extract_claude_history, "claude-history");
bulk_boundary!(extract_codex, "codex");
bulk_boundary!(extract_codex_sessions, "codex");
bulk_boundary!(extract_gemini, "gemini");
bulk_boundary!(extract_grok, "grok");
bulk_boundary!(extract_grok_sessions, "grok");
bulk_boundary!(extract_junie, "junie");

file_boundary!(extract_claude_file, "claude");
file_boundary!(extract_codex_file, "codex");
file_boundary!(extract_gemini_file, "gemini");
file_boundary!(extract_gemini_antigravity_file, "gemini-antigravity");
file_boundary!(extract_grok_file, "grok");
file_boundary!(extract_junie_file, "junie");

pub(crate) fn count_codex_sessions(_path: &Path) -> Result<usize> {
    unavailable("codex")
}
