//! Integration tests for the native embedder.
//!
//! These tests only compile when the `native-embedder` feature is active, and
//! they no-op when the runtime model resources are not available (embedded
//! bytes absent AND HF cache empty for the configured repo). This keeps the
//! workspace fast on clean machines while giving real coverage on dev boxes
//! that already have the model cached.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

#![cfg(feature = "native-embedder")]

use aicx::embedder::{EmbedderConfig, EmbedderEngine, NativeEmbeddingSource};

#[test]
fn engine_embeds_and_returns_unit_norm_vectors() {
    let engine = EmbedderEngine::with_config(EmbedderConfig {
        prefer_embedded: false,
        ..Default::default()
    });
    let Ok(mut engine) = engine else {
        eprintln!("skip: no GGUF embedder model available in local HF cache");
        return;
    };

    let vectors = engine
        .embed_batch(&["hello world", "hola mundo", "let mut x = 42;"])
        .expect("embed_batch should succeed");

    assert_eq!(vectors.len(), 3);
    for (idx, v) in vectors.iter().enumerate() {
        assert_eq!(
            v.len(),
            engine.dimension(),
            "vector {idx} has unexpected dimension"
        );
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!(
            (norm - 1.0).abs() < 1e-2,
            "vector {idx} should be L2-normalised (got norm={norm})"
        );
    }

    // Semantic sanity: identical text embeds to the same vector.
    let repeat = engine
        .embed("hello world")
        .expect("single embed should succeed");
    let sim = EmbedderEngine::similarity(&repeat, &vectors[0]);
    assert!(sim > 0.999, "self-similarity should be ~1.0 (got {sim})");
}

#[test]
fn source_describes_runtime_provenance() {
    let Ok(engine) = EmbedderEngine::with_config(EmbedderConfig {
        prefer_embedded: false,
        ..Default::default()
    }) else {
        eprintln!("skip: no GGUF embedder model available for source check");
        return;
    };

    match engine.source() {
        NativeEmbeddingSource::HfCache {
            repo,
            path,
            filename,
        } => {
            assert!(!repo.is_empty());
            assert!(filename.ends_with(".gguf"));
            assert!(
                path.exists(),
                "HF cache model file should point at a real file"
            );
        }
        NativeEmbeddingSource::ExplicitPath(p) => assert!(p.exists()),
    }
}
