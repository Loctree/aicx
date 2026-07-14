use chrono::{DateTime, Utc};
use serde::Serialize;
use std::path::PathBuf;

/// Re-export from timeline — single definition, no twin.
pub use crate::timeline::TimelineEntry;

/// Re-export the denoised conversation message type.
pub use crate::timeline::ConversationMessage;

/// Configuration for the output engine.
#[derive(Debug, Clone)]
pub struct OutputConfig {
    pub dir: PathBuf,
    pub format: OutputFormat,
    pub mode: OutputMode,
    /// Rotation: keep last N files (0 = unlimited)
    pub max_files: usize,
    /// Maximum message characters in markdown (0 = no truncation)
    pub max_message_chars: usize,
    /// Include loctree snapshot in output
    pub include_loctree: bool,
    /// Project root for loctree snapshot
    pub project_root: Option<PathBuf>,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            format: OutputFormat::Both,
            mode: OutputMode::NewFile,
            max_files: 0,
            max_message_chars: 0,
            include_loctree: false,
            project_root: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutputFormat {
    Markdown,
    Json,
    Both,
}

#[derive(Debug, Clone)]
pub enum OutputMode {
    /// Create new timestamped file each run (original behavior)
    NewFile,
    /// Append to a single timeline file, deduplicating by date
    AppendTimeline(PathBuf),
}

/// Metadata about the generated report.
#[derive(Debug, Clone, Serialize)]
pub struct ReportMetadata {
    pub generated_at: DateTime<Utc>,
    pub project_filter: Option<String>,
    pub hours_back: u64,
    pub total_entries: usize,
    pub sessions: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConversationExtractStats {
    pub aicx_version: &'static str,
    pub redaction_enabled: bool,
    pub raw_entries: usize,
    pub conversation_messages: usize,
    pub conversation_projection: &'static str,
    pub exact_short_duplicates_dropped: usize,
    /// Harness-injected synthetic user turns dropped from the conversation
    /// projection (slash-command / skill bodies, inline `! command` I/O,
    /// system/hook reminders).
    pub harness_noise_dropped: usize,
}
