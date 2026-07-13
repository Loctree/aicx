//! Sources / Extractors module for AI Contexters
//!
//! Thin facade over provider-specific extractors and shared source utilities.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

pub(crate) use anyhow::Context;
pub(crate) use chrono::{DateTime, Utc};
pub(crate) use std::fs;
pub(crate) use std::path::{Path, PathBuf};

pub(crate) use crate::sanitize;

pub use crate::timeline::{
    CollapseStubKind, ConversationMessage, ExtractionConfig, FrameKind, MessageKind, SourceInfo,
    TimelineEntry,
};

pub mod providers;
pub mod shared;

const UNPROTECTED_SOURCE_WARNING: &str = "unprotected source material; run `aicx sources protect --root <path> --backend git-local --apply` to opt in";

pub(crate) use providers::count_codex_sessions;
pub use providers::{
    extract_claude, extract_claude_file, extract_claude_history, extract_codex, extract_codex_file,
    extract_codex_sessions, extract_gemini, extract_gemini_antigravity_file, extract_gemini_file,
    extract_grok, extract_grok_file, extract_grok_sessions, extract_junie, extract_junie_file,
};
pub(crate) use shared::*;
pub use shared::{
    ConversationProjection, decode_claude_project_path, detect_project_name,
    infer_repo_name_from_current_dir, is_harness_injected_noise, list_available_sources,
    repo_labels_from_entries, repo_name_from_cwd, to_conversation, to_conversation_with_stats,
};

// `extract_all` is intentionally gone: it double-parsed Codex
// (`extract_codex` + `extract_codex_sessions`) and Grok (`extract_grok` +
// `extract_grok_sessions`) and papered over the duplicates with a
// timestamp+prefix dedup. Bulk ingestion enumerates agents in the CLI store
// pipeline and parses each source exactly once.

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod conversation_tests;
