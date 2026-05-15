// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
//! Retrieval trait facade for aicx hybrid indexes.
//!
//! This crate defines the public contracts shared by lexical, dense, and
//! fusion retrieval adapters. It intentionally contains no index
//! implementations; Tantivy, sqlite-vec, and brute-force adapters live in
//! downstream crates or follow-up tracks.

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
