//! Parser, timeline, segmentation, and chunking primitives for aicx.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

pub mod chunker;
pub mod frontmatter;
pub mod sanitize;
pub mod segmentation;
pub mod timeline;
pub mod types;

pub use chunker::{Chunk, ChunkMetadataSidecar, ChunkerConfig, classify_kind};
pub use frontmatter::ReportFrontmatter;
pub use sanitize::{filter_self_echo, is_self_echo, normalize_query};
pub use segmentation::{
    ProjectHashRegistry, TieredIdentity, classify_cwd_tier, infer_repo_identity_from_entry,
    infer_tiered_identity_from_entry, semantic_segments, semantic_segments_with_registry,
};
pub use timeline::{
    ConversationMessage, ExtractionConfig, FrameKind, Kind, RepoIdentity, SemanticSegment,
    SourceInfo, SourceTier, TimelineEntry,
};
pub use types::{EntryState, EntryType, IntentEntry, Link, LinkType};
