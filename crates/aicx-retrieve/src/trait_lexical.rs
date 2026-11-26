// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use anyhow::Result;
use std::collections::HashMap;

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
    /// Return query-focused context for already selected documents.
    fn query_snippets(
        &self,
        _query: &str,
        _chunk_ids: &[String],
    ) -> Result<HashMap<String, Vec<String>>> {
        Ok(HashMap::new())
    }
    fn commit_id(&self) -> &LexicalCommitId;
    fn doc_count(&self) -> usize;
}
