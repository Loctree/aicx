// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use crate::Hit;

/// Contract for combining lexical and dense retrieval results.
pub trait FusionStrategy {
    fn fuse(&self, lex: Vec<Hit>, dense: Vec<Hit>, limit: usize) -> Vec<Hit>;
    fn name(&self) -> &str;
}
