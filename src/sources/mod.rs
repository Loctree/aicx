//! Sources / Extractors — new modular home (post 2026-05-27 decomposition).
//!
//! During the transition the old monolithic implementation lives in `legacy.rs`
//! and is re-exported 1:1 so nothing outside this module notices the change yet.
//!
//! Step by step we will:
//! - Move provider logic into `providers/<name>.rs`
//! - Extract shared concerns into a clean `shared` surface
//! - Hollow out legacy.rs until it can be deleted
//!
//! The public API surface must remain stable throughout.

mod legacy;

// During migration we re-export the entire previous public API verbatim.
pub use legacy::*;

// New modular structure (work in progress)
pub mod providers;
pub mod shared;
