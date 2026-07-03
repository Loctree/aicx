//! Output engine for ai-contexters
//!
//! Handles report generation in Markdown and JSON formats with support for:
//! - NewFile mode (timestamped files, current behavior)
//! - AppendTimeline mode (append to single file, deduplication by date)
//! - File rotation (keep last N files)
//! - Loctree snapshot embedding
//! - Decision markers and proper code block handling
//!
//! The public surface stays here; implementation lives in focused submodules
//! for types, decision detection, report file writing, and conversation export.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

mod conversation;
mod decision;
mod report;
mod types;

pub use conversation::{
    write_conversation_json, write_conversation_json_with_redaction, write_conversation_markdown,
    write_conversation_markdown_with_redaction,
};
pub use report::{
    capture_loctree_snapshot, rotate_outputs, write_json_report_to_path,
    write_markdown_report_to_path, write_report,
};
pub use types::{
    ConversationExtractStats, ConversationMessage, OutputConfig, OutputFormat, OutputMode,
    ReportMetadata, TimelineEntry,
};

#[cfg(test)]
mod tests;
