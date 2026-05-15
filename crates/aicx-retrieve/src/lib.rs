// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
//! Retrieval trait facade for aicx hybrid indexes.
//!
//! This crate defines the public contracts shared by lexical, dense, and
//! fusion retrieval adapters plus default-on retrieval implementations.

pub mod adapter_brute_force;
pub mod adapter_tantivy;
pub mod error;
pub mod manifest;
pub mod trait_dense;
pub mod trait_fusion;
pub mod trait_lexical;
pub mod types;

pub use trait_lexical::*;

pub use trait_dense::*;

pub use trait_fusion::*;

pub use manifest::*;

pub use error::*;

pub use types::*;

pub use adapter_brute_force::{
    BRUTE_FORCE_KIND, BruteForceAdapter, DEFAULT_NDJSON_FILE_NAME, LoadStats, default_ndjson_path,
    load_from_ndjson,
};

pub use adapter_tantivy::*;
