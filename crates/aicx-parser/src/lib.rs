//! Explicit parser-side boundary for canonical extraction, chunking, store layout,
//! and read/query helpers that do not own the memex indexing runtime.

pub mod chunker;
pub mod frontmatter;
pub mod rank;
pub mod redact;
pub mod sanitize;
pub mod segmentation;
pub mod sources;
pub mod state;
pub mod store;
pub mod types;
