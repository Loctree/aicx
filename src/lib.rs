//! AICX library crate.
//!
//! Use [`Aicx`] for the supported in-process boundary: store timeline entries,
//! scan/read canonical chunks, search, extract intents, and run doctor checks
//! without importing CLI-private code from `main.rs`.
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
pub mod locks;
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
#[cfg(feature = "lance")]
pub mod steer_index;
#[cfg(not(feature = "lance"))]
#[path = "steer_index_stub.rs"]
pub mod steer_index;
pub mod store;
pub mod validation;
pub mod vector_index;
pub mod wizard;

pub use aicx_parser as parser;
pub use aicx_parser::{chunker, frontmatter, sanitize, segmentation, timeline, types};
pub use api::{Aicx, AicxConfig, IndexStatus, SearchOptions, SearchResults, StoreOptions};

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub use aicx_embeddings as embeddings;

pub mod prelude {
    pub use crate::api::{Aicx, AicxConfig, SearchOptions, StoreOptions};
    pub use crate::doctor::{DoctorOptions, DoctorReport};
    pub use crate::intents::{IntentExtraction, IntentRecord, IntentsConfig};
    pub use crate::rank::FuzzyResult;
    pub use crate::store::{ReadContextChunk, StoreWriteSummary, StoredContextFile};
    pub use crate::timeline::TimelineEntry;
}
