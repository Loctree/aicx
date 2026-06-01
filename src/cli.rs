//! CLI-boundary helpers shared across `main.rs` dispatch handlers.
//!
//! This module hosts cross-cutting surfaces that need to live on the
//! `aicx::` import path (so they can be unit-tested via `tests/`) but
//! that are conceptually closer to the CLI dispatch than to the library
//! API. Today this is just structured failure-as-state — see
//! [`failure::StructuredFailure`].
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

pub mod failure;
