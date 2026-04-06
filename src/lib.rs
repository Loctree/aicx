//! ai-contexters library crate.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

pub mod dashboard;
pub mod dashboard_server;
pub mod intents;
pub mod mcp;
pub mod output;

pub use aicx_memex::{daemon, memex, steer_index};
pub use aicx_parser::{
    chunker, frontmatter, rank, redact, sanitize, segmentation, sources, state, store, types,
};
