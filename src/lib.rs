//! ai-contexters library crate.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

pub mod corpus;
pub mod dashboard;
pub mod dashboard_server;
pub mod doctor;
#[cfg(feature = "native-embedder")]
pub mod embedder;
pub mod hf_cache;
pub mod intents;
pub mod mcp;
pub mod oracle;
pub mod output;
pub mod progress;
pub mod rank;
pub mod redact;
pub mod reports_extractor;
pub mod search_engine;
pub mod sources;
pub mod state;
pub mod steer_index;
pub mod store;
pub mod validation;
pub mod vector_index;
pub mod wizard;

pub use aicx_parser::{chunker, frontmatter, sanitize, segmentation, timeline, types};
