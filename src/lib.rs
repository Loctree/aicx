//! ai-contexters library crate.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

pub mod chunker;
pub mod dashboard;
pub mod dashboard_server;
#[cfg(feature = "native-embedder")]
pub mod embedder;
pub mod frontmatter;
pub mod hf_cache;
pub mod intents;
pub mod mcp;
pub mod output;
pub mod rank;
pub mod redact;
pub mod reports_extractor;
pub mod sanitize;
pub mod segmentation;
pub mod sources;
pub mod state;
pub mod steer_index;
pub mod store;
pub mod types;
