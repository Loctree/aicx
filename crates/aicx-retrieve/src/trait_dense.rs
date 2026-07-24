// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use anyhow::Result;

use crate::{DenseChunkRef, Distance, FilterSet, Hit};

/// Contract for dense-vector retrieval adapters.
///
/// Trait boundaries use `anyhow::Result` so adapter crates can preserve their
/// native error context while manifest validation remains typed.
pub trait DenseIndex {
    fn dim(&self) -> usize;
    fn distance(&self) -> Distance;
    fn kind(&self) -> &str;
    fn build(&mut self, chunks: &[DenseChunkRef]) -> Result<()>;
    fn insert(&mut self, chunk: &DenseChunkRef) -> Result<()>;
    /// Return deterministic exact top-k results. Implementations should apply
    /// filters before distance work and bound query heap growth by `limit` and
    /// decoded metadata rather than the stored vector payload.
    fn query(&self, embedding: &[f32], limit: usize, filters: &FilterSet) -> Result<Vec<Hit>>;
    fn count(&self) -> usize;
}
