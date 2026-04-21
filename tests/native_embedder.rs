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

use aicx::embedder::{EmbedderConfig, EmbedderEngine, NativeEmbeddingSource, is_embedded_available};

#[test]
fn embedded_dimension_hint_is_positive_when_available() {
    if !is_embedded_available() {
        eprintln!("skip: embedded embedder bytes not present in this build");
        return;
    }
    let dim = aicx::embedder::embedded_dimension();
    assert!(
        dim.is_some_and(|d| d > 0),
        "embedded model should report a hidden_size >= 1, got {dim:?}"
    );
}

#[test]
fn engine_embeds_and_returns_unit_norm_vectors() {
    let engine = EmbedderEngine::with_config(EmbedderConfig {
        prefer_embedded: true,
        ..Default::default()
    });
    let Ok(mut engine) = engine else {
        eprintln!("skip: no embedder model available (embedded absent + no HF cache snapshot)");
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
    assert!(
        sim > 0.999,
        "self-similarity should be ~1.0 (got {sim})"
    );
}

#[test]
fn source_describes_runtime_provenance() {
    let Ok(engine) = EmbedderEngine::with_config(EmbedderConfig {
        prefer_embedded: true,
        ..Default::default()
    }) else {
        eprintln!("skip: no embedder model available for source check");
        return;
    };

    match engine.source() {
        NativeEmbeddingSource::Embedded { repo } => assert!(!repo.is_empty()),
        NativeEmbeddingSource::HfCache { repo, path } => {
            assert!(!repo.is_empty());
            assert!(path.exists(), "HF cache snapshot should point at a real dir");
        }
        NativeEmbeddingSource::ExplicitPath(p) => assert!(p.exists()),
    }
}
