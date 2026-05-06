//! ai-contexters library crate.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

pub mod api;
pub mod corpus;
pub mod dashboard;
pub mod dashboard_server;
pub mod doctor;
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
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

pub use aicx_parser as parser;
pub use aicx_parser::{chunker, frontmatter, sanitize, segmentation, timeline, types};
pub use api::{Aicx, AicxConfig, IndexStatus, SearchOptions, SearchResults};

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub use aicx_embeddings as embeddings;

pub mod prelude {
    pub use crate::api::{Aicx, AicxConfig, SearchOptions};
    pub use crate::doctor::{DoctorOptions, DoctorReport};
    pub use crate::intents::{IntentExtraction, IntentRecord, IntentsConfig};
    pub use crate::rank::FuzzyResult;
    pub use crate::store::{ReadContextChunk, StoredContextFile};
}
