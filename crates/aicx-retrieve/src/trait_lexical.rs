// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use anyhow::Result;

use crate::{ChunkRef, Hit, LexicalCommitId, LexicalQuery};

/// Contract for lexical retrieval adapters.
///
/// Trait boundaries use `anyhow::Result` so adapter crates can preserve their
/// native error context while manifest validation remains typed.
pub trait LexicalIndex {
    fn schema_version(&self) -> &str;
    fn build(&mut self, chunks: &[ChunkRef]) -> Result<LexicalCommitId>;
    fn insert(&mut self, chunk: &ChunkRef) -> Result<()>;
    fn query(&self, q: &LexicalQuery) -> Result<Vec<Hit>>;
    fn commit_id(&self) -> &LexicalCommitId;
    fn doc_count(&self) -> usize;
}
