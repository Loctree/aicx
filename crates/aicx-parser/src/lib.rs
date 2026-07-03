//! Parser, timeline, segmentation, and chunking primitives for aicx.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

pub mod chunker;
pub mod frontmatter;
pub mod noise;
pub mod sanitize;
pub mod segmentation;
pub mod skill_collapse;
pub mod timeline;
pub mod types;

pub use chunker::{
    CARD_CLAIM_SCOPE_SESSION_CLOSE, CARD_FRESHNESS_CONTRACT_HISTORICAL, CARD_SCHEMA_VERSION,
    CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX, CardSignal, CardSource, Chunk,
    ChunkMetadataSidecar, ChunkerConfig, LearningUse, TruthRole, TruthStatus, classify_kind,
};
pub use frontmatter::ReportFrontmatter;
pub use sanitize::{filter_self_echo, is_self_echo, normalize_query, read_state_json_validated};
#[rustfmt::skip]
pub use sanitize::{read_line_capped, MAX_VALIDATED_BYTES};
pub use segmentation::{
    ProjectHashRegistry, TieredIdentity, classify_cwd_tier, infer_repo_identity_from_entry,
    infer_tiered_identity_from_entry, semantic_segments, semantic_segments_with_registry,
};
pub use skill_collapse::{
    CollapseStats, DEFAULT_THRESHOLD_LINES, collapse_repeats, detect_skill_marker,
};
pub use timeline::{
    ConversationMessage, ExtractionConfig, FrameKind, Kind, RepoIdentity, SemanticSegment,
    SourceInfo, SourceTier, TimelineEntry,
};
pub use types::{EntryState, EntryType, IntentEntry, Link, LinkType};
