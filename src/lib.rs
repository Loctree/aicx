//! ai-contexters library crate.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

pub mod dashboard;
pub mod dashboard_server;
pub mod doctor;
#[cfg(feature = "native-embedder")]
pub mod embedder;
pub mod hf_cache;
pub mod intents;
pub mod mcp;
pub mod output;
pub mod progress;
pub mod rank;
pub mod redact;
pub mod reports_extractor;
pub mod sources;
pub mod state;
pub mod steer_index;
pub mod store;
pub mod validation;
pub mod wizard;

pub use aicx_parser::{chunker, frontmatter, sanitize, segmentation, timeline, types};
