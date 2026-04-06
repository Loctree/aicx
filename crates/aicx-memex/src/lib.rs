//! Explicit indexer-side boundary for steer repair, memex materialization,
//! and daemonized background maintenance around the canonical AICX store.

pub use aicx_parser::{chunker, rank, sanitize, store};

pub mod daemon;
pub mod memex;
pub mod steer_index;
