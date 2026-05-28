//! Shared core for the Sources extraction layer.
//!
//! This module contains logic that is truly common across all providers:
//! - Conversation projection and deduplication
//! - Project filtering and identity resolution
//! - Timeline entry building primitives
//! - Content sanitization and line capping utilities
//!
//! Provider-specific modules should depend on this, not on each other.
//!
//! Part of the 2026-05-27 sources monolith decomposition.

pub mod conversation;
pub mod project_filter;
pub mod sanitization;
pub mod timeline_building;

// Re-export the most commonly needed items at the shared level for convenience
pub use conversation::{ConversationProjection, to_conversation, to_conversation_with_stats};
pub use project_filter::{project_filter_matches, repo_name_from_cwd};
pub use sanitization::MAX_LINE_BYTES;
