//! Document importers — operator-owned source documents ingested into the
//! canonical corpus.
//!
//! Importers are an integration line, not agent session adapters: they consume
//! human-authored artifacts (CodeScribe transcripts, operator Markdown) and
//! project them into `TimelineEntry` records for the store pipeline. Nothing in
//! this namespace implements or registers `aicx_parser::adapters::AgentAdapter`,
//! and nothing here participates in session catalog resolution.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

pub mod codescribe;
pub mod operator_markdown;

pub use codescribe::{
    CodescribeTranscript, discover_codescribe_transcripts, discover_codescribe_transcripts_at,
    extract_codescribe, extract_codescribe_from_home, parse_codescribe_transcript,
};
pub use operator_markdown::{
    OperatorMarkdown, discover_operator_markdown, discover_operator_markdown_from,
    discover_operator_markdown_from_input, extract_operator_markdown,
    extract_operator_markdown_from_home, extract_operator_markdown_from_home_and_repo,
    extract_operator_markdown_from_input,
};
