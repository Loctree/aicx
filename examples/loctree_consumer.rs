use aicx::api::{Aicx, SearchOptions};
use aicx::oracle::OracleStatus;

fn main() -> anyhow::Result<()> {
    // 1. Initialize the library facade.
    // The Aicx client handles resolution of the store root (~/.aicx by default)
    // and exposes the primary operations (read, store, search, extract) without
    // importing the CLI's main.rs glue.
    let client = Aicx::from_env()?;

    // 2. Query the current oracle status for semantic search.
    // The index_status() method determines whether a semantic index is
    // available, pending, or missing.
    let status = client.index_status(None)?;

    println!("Index Readiness: {:?}", status.readiness);
    println!("Canonical Chunks: {}", status.canonical_chunks);
    println!("Semantic Rows: {}", status.semantic_index_rows);
    println!("Temp Index Present: {}", status.temp_index_present);

    // 3. Use the typed Oracle status directly (if we had a direct API for it).
    // The facade provides the foundational structures for evaluating
    // index health before searching.
    let oracle = OracleStatus::canonical_corpus_scan(
        client.store_root(),
        status.canonical_chunks,
        status.canonical_chunks,
        true,
    );

    println!("\nTyped Oracle Contract:");
    println!("Backend: {:?}", oracle.backend);
    println!("Layer: {}", oracle.source_layer);
    println!("Loctree Safe: {}", oracle.loctree_scope_safe);
    println!("Note: {}", oracle.loctree_scope_note);

    let readiness = aicx::oracle::readiness(&[oracle]);
    println!("Readiness: {:?}", readiness);

    // 4. Perform a semantic search.
    // The Aicx facade handles vector search (if the feature is enabled)
    // and returns results with scores.
    println!("\nSearching...");
    match client.semantic_search("oracle contract", SearchOptions::default()) {
        Ok(results) => {
            println!("Found {} results", results.results.len());
            for (i, res) in results.results.iter().take(3).enumerate() {
                println!("  [{i}] Score: {:.4} | File: {}", res.score, res.path);
            }
        }
        Err(e) => {
            println!("Search unavailable: {e}");
        }
    }

    Ok(())
}
