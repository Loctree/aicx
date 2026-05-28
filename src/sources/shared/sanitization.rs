//! Content sanitization, line capping, and oversized line handling.
//!
//! Extracted during Faza 1 of the sources decomposition (2026-05-27).

pub const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

// Additional sanitization helpers will be moved here from legacy.rs.
