//! Slim `loctree-consumer` contract walkthrough.
//!
//! Run with the slim profile (the point of this example):
//!   cargo run --example loctree_consumer --no-default-features --features loctree-consumer
//!
//! Everything used here is the stable read core exposed to in-process
//! consumers such as Loctree: store discovery, canonical chunk enumeration
//! and reads, typed chunk references, index-readiness inspection, and pure
//! intent extraction. No CLI, MCP, embedder, or semantic-search surfaces are
//! linked — `semantic_search`/`SearchOptions` live behind `feature = "app"`.

use aicx::api::Aicx;
use aicx::intents::IntentsConfig;
use aicx::store::ChunkRefSpec;

fn main() -> anyhow::Result<()> {
    // 1. Resolve the store the same way Loctree does in-process
    //    (~/.aicx by default, honoring AICX_HOME overrides).
    let client = Aicx::from_env()?;
    println!("Store root: {}", client.store_root().display());

    // 2. Index status is part of the slim surface: a consumer can tell
    //    whether semantic retrieval would be available without linking it.
    let status = client.index_status(None)?;
    println!("Index readiness: {:?}", status.readiness);
    println!("Canonical chunks: {}", status.canonical_chunks);

    // 3. Enumerate canonical chunks and read one back through the typed
    //    reference API. `ChunkRefSpec` accepts `chunk:<id>` ids as well as
    //    store-relative paths.
    let chunks = client.list_chunks()?;
    println!("Stored context files: {}", chunks.len());

    let typed = ChunkRefSpec::parse("chunk:abcdef12")?;
    println!("Typed chunk ref: {typed:?}");

    if let Some(first) = chunks.first() {
        let reference = first
            .path
            .strip_prefix(client.store_root())
            .unwrap_or(&first.path)
            .to_string_lossy()
            .replace('\\', "/");
        match client.read_chunk(&reference, Some(200)) {
            Ok(chunk) => println!(
                "Read {} ({} bytes, project {}, agent {})",
                chunk.relative_path, chunk.bytes, chunk.project, chunk.agent
            ),
            Err(error) => println!("Chunk read unavailable: {error}"),
        }
    }

    // 4. Pure intent extraction — the contract Loctree consumes for
    //    `aicx_intents`-style recall. Typed `IntentRecord`s carry kind,
    //    summary, project, and provenance.
    let config = IntentsConfig {
        project: String::new(),
        hours: 24,
        strict: false,
        min_confidence: None,
        kind_filter: None,
        frame_kind: None,
    };
    let extraction = client.extract_intents(&config)?;
    println!(
        "Intents (last 24h): {} records from {} scanned chunks",
        extraction.records.len(),
        extraction.stats.scanned_count
    );
    if let Some(record) = extraction.records.first() {
        println!(
            "  [{:?}] {} ({} / {})",
            record.kind, record.summary, record.project, record.date
        );
    }

    Ok(())
}
