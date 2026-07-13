//! Sources / Extractors module for AI Contexters
//!
//! Thin facade over provider-specific extractors and shared source utilities.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

pub(crate) use anyhow::Context;
use anyhow::Result;
pub(crate) use chrono::{DateTime, Utc};
use std::collections::HashSet;
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
    CodescribeTranscript, OperatorMarkdown, discover_codescribe_transcripts,
    discover_codescribe_transcripts_at, discover_operator_markdown,
    discover_operator_markdown_from, discover_operator_markdown_from_input, extract_claude,
    extract_claude_file, extract_claude_history, extract_codescribe, extract_codescribe_from_home,
    extract_codex, extract_codex_file, extract_codex_sessions, extract_gemini,
    extract_gemini_antigravity_file, extract_gemini_file, extract_grok, extract_grok_file,
    extract_grok_sessions, extract_junie, extract_junie_file, extract_operator_markdown,
    extract_operator_markdown_from_home, extract_operator_markdown_from_home_and_repo,
    extract_operator_markdown_from_input, parse_codescribe_transcript,
};
pub(crate) use shared::*;
pub use shared::{
    ConversationProjection, decode_claude_project_path, detect_project_name,
    infer_repo_name_from_current_dir, is_harness_injected_noise, list_available_sources,
    repo_labels_from_entries, repo_name_from_cwd, to_conversation, to_conversation_with_stats,
};

pub fn extract_all(config: &ExtractionConfig) -> Result<Vec<TimelineEntry>> {
    let mut all: Vec<TimelineEntry> = Vec::new();

    // Claude
    match extract_claude(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Claude extraction warning: {}", e),
    }

    // Codex
    match extract_codex(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Codex extraction warning: {}", e),
    }

    // Grok (Codex v1/responses format under ~/.grok)
    match extract_grok(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Grok extraction warning: {}", e),
    }

    // Gemini
    match extract_gemini(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Gemini extraction warning: {}", e),
    }

    // Junie
    match extract_junie(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Junie extraction warning: {}", e),
    }

    // Codescribe
    match extract_codescribe(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Codescribe extraction warning: {}", e),
    }

    // Operator markdown
    match extract_operator_markdown(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Operator markdown extraction warning: {}", e),
    }

    // Claude history.jsonl
    match extract_claude_history(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Claude history extraction warning: {}", e),
    }

    // Codex sessions
    match extract_codex_sessions(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Codex sessions extraction warning: {}", e),
    }

    // Grok sessions (separate for parity with codex; extract_grok already includes them too)
    match extract_grok_sessions(config) {
        Ok(entries) => all.extend(entries),
        Err(e) => eprintln!("Grok sessions extraction warning: {}", e),
    }

    // Sort by timestamp
    all.sort_by_key(|a| a.timestamp);

    // Dedup: same timestamp + same first 100 chars of message -> keep first
    let mut seen: HashSet<(i64, String)> = HashSet::new();
    all.retain(|entry| {
        let key_msg: String = format!(
            "{}:{}",
            entry.frame_kind.map(FrameKind::as_str).unwrap_or("unknown"),
            entry.message.chars().take(100).collect::<String>()
        );
        let key = (entry.timestamp.timestamp(), key_msg);
        seen.insert(key)
    });

    Ok(all)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod conversation_tests;
