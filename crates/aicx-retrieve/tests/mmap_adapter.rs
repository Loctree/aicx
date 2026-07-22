// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use std::fs;

use aicx_retrieve::{
    BruteForceAdapter, ChunkRef, DenseChunkRef, DenseIndex, Distance, FilterSet,
    MMAP_DENSE_HEADER_LEN, MmapDenseAdapter,
};
use rand::{Rng, SeedableRng, rngs::StdRng};
use serde_json::json;
use tempfile::tempdir;

const SOURCE_HASH: [u8; 32] = [0x5a; 32];

fn chunk(id: impl Into<String>, project: &str, embedding: Vec<f32>) -> DenseChunkRef {
    let id = id.into();
    DenseChunkRef {
        chunk: ChunkRef {
            id: id.clone(),
            source_path: format!("projects/{project}/{id}.md"),
            text: format!("body for {id}"),
            metadata: json!({"project": project, "agent": "codex"}),
        },
        embedding,
    }
}

fn write_fixture(path: &std::path::Path) -> Vec<DenseChunkRef> {
    let chunks = vec![
        chunk("a", "alpha", vec![1.0, 0.0, 0.0]),
        chunk("b", "beta", vec![0.9, 0.1, 0.0]),
        chunk("c", "alpha", vec![0.0, 1.0, 0.0]),
    ];
    let mut adapter = MmapDenseAdapter::create(path, 3, Distance::Cosine, SOURCE_HASH);
    adapter.build(&chunks).expect("build mmap fixture");
    chunks
}

#[test]
fn round_trip_maps_read_only_and_preserves_header_contract() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dense.mmap");
    write_fixture(&path);

    let adapter = MmapDenseAdapter::open(&path, 3, Distance::Cosine, Some(SOURCE_HASH))
        .expect("open mmap fixture");
    assert_eq!(adapter.count(), 3);
    assert_eq!(adapter.source_hash(), SOURCE_HASH);
    assert!(adapter.mapped_len() > MMAP_DENSE_HEADER_LEN);

    let hits = adapter
        .query(&[1.0, 0.0, 0.0], 2, &FilterSet::default())
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.chunk_id.as_str())
            .collect::<Vec<_>>(),
        ["a", "b"]
    );
}

#[test]
fn every_structural_truncation_boundary_fails_closed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dense.mmap");
    write_fixture(&path);
    let bytes = fs::read(&path).unwrap();

    let refs_offset = read_u64(&bytes, 64) as usize;
    let refs_len = read_u64(&bytes, 72) as usize;
    let vectors_offset = read_u64(&bytes, 80) as usize;
    let vectors_len = read_u64(&bytes, 88) as usize;
    let metadata_offset = read_u64(&bytes, 96) as usize;
    let boundaries = [
        0,
        7,
        8,
        9,
        10,
        13,
        14,
        15,
        16,
        19,
        20,
        23,
        24,
        31,
        32,
        63,
        64,
        71,
        72,
        79,
        80,
        87,
        88,
        95,
        96,
        103,
        104,
        111,
        112,
        119,
        120,
        MMAP_DENSE_HEADER_LEN - 1,
        MMAP_DENSE_HEADER_LEN,
        refs_offset + refs_len - 1,
        refs_offset + refs_len,
        vectors_offset + vectors_len - 1,
        vectors_offset + vectors_len,
        metadata_offset,
        bytes.len() - 1,
    ];

    for (case, &len) in boundaries.iter().enumerate() {
        let truncated = dir.path().join(format!("truncated-{case}.mmap"));
        fs::write(&truncated, &bytes[..len]).unwrap();
        assert!(
            MmapDenseAdapter::open(&truncated, 3, Distance::Cosine, Some(SOURCE_HASH)).is_err(),
            "truncation at byte {len} must fail"
        );
    }
}

#[test]
fn wrong_dimension_endian_version_and_corrupt_header_fail_closed() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dense.mmap");
    write_fixture(&path);

    assert!(MmapDenseAdapter::open(&path, 2, Distance::Cosine, Some(SOURCE_HASH)).is_err());
    assert!(MmapDenseAdapter::open(&path, 3, Distance::Dot, Some(SOURCE_HASH)).is_err());
    assert!(MmapDenseAdapter::open(&path, 3, Distance::Cosine, Some([0xff; 32])).is_err());

    for (name, offset, replacement) in [
        ("magic", 0usize, vec![b'X']),
        ("version", 8, 99u16.to_le_bytes().to_vec()),
        ("endian", 10, 0x0102_0304u32.to_be_bytes().to_vec()),
        ("vectors-offset", 80, u64::MAX.to_le_bytes().to_vec()),
    ] {
        let mut bytes = fs::read(&path).unwrap();
        bytes[offset..offset + replacement.len()].copy_from_slice(&replacement);
        let corrupt = dir.path().join(format!("corrupt-{name}.mmap"));
        fs::write(&corrupt, bytes).unwrap();
        assert!(
            MmapDenseAdapter::open(&corrupt, 3, Distance::Cosine, Some(SOURCE_HASH)).is_err(),
            "corrupt {name} must fail"
        );
    }
}

#[test]
fn corrupt_metadata_reference_fails_during_open() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dense.mmap");
    write_fixture(&path);
    let mut bytes = fs::read(&path).unwrap();
    let refs_offset = read_u64(&bytes, 64) as usize;
    bytes[refs_offset..refs_offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    fs::write(&path, bytes).unwrap();

    assert!(MmapDenseAdapter::open(&path, 3, Distance::Cosine, Some(SOURCE_HASH)).is_err());
}

#[test]
fn project_filter_is_applied_before_distance_work() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dense.mmap");
    write_fixture(&path);
    let adapter = MmapDenseAdapter::open(&path, 3, Distance::Cosine, None).unwrap();
    let mut filters = FilterSet::default();
    filters.values.insert("project".into(), json!("alpha"));

    let hits = adapter.query(&[1.0, 0.0, 0.0], 10, &filters).unwrap();
    assert_eq!(hits.len(), 2);
    assert!(hits.iter().all(|hit| hit.metadata["project"] == "alpha"));
    let stats = adapter.last_query_stats();
    assert_eq!(stats.metadata_examined, 3);
    assert_eq!(stats.vectors_scored, 2);
}

#[test]
fn deterministic_top_k_matches_legacy_on_seeded_random_corpora() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("dense.mmap");
    let mut rng = StdRng::seed_from_u64(0x0a1c_0004);
    let chunks: Vec<_> = (0..257)
        .map(|index| {
            let embedding = (0..31).map(|_| rng.gen_range(-1.0..1.0)).collect();
            chunk(format!("row-{index:04}"), "alpha", embedding)
        })
        .collect();
    let query: Vec<f32> = (0..31).map(|_| rng.gen_range(-1.0..1.0)).collect();

    let mut legacy = BruteForceAdapter::new(31);
    legacy.build(&chunks).unwrap();
    let mut mmap = MmapDenseAdapter::create(&path, 31, Distance::Cosine, SOURCE_HASH);
    mmap.build(&chunks).unwrap();

    let legacy_hits = legacy.query(&query, 17, &FilterSet::default()).unwrap();
    let mmap_hits = mmap.query(&query, 17, &FilterSet::default()).unwrap();
    assert_eq!(
        mmap_hits
            .iter()
            .map(|hit| &hit.chunk_id)
            .collect::<Vec<_>>(),
        legacy_hits
            .iter()
            .map(|hit| &hit.chunk_id)
            .collect::<Vec<_>>()
    );
    for (left, right) in mmap_hits.iter().zip(legacy_hits.iter()) {
        assert_eq!(left.score.to_bits(), right.score.to_bits());
    }
}

#[test]
fn score_ties_preserve_source_order_at_the_top_k_boundary() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("ties.mmap");
    let chunks: Vec<_> = (0..8)
        .map(|index| chunk(format!("tie-{index}"), "alpha", vec![1.0, 0.0]))
        .collect();
    let mut adapter = MmapDenseAdapter::create(&path, 2, Distance::Cosine, SOURCE_HASH);
    adapter.build(&chunks).unwrap();

    let hits = adapter
        .query(&[1.0, 0.0], 3, &FilterSet::default())
        .unwrap();
    assert_eq!(
        hits.iter()
            .map(|hit| hit.chunk_id.as_str())
            .collect::<Vec<_>>(),
        ["tie-0", "tie-1", "tie-2"]
    );
}

#[test]
fn insert_streams_existing_payload_and_remaps_the_extended_shard() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("insert.mmap");
    let mut adapter = MmapDenseAdapter::create(&path, 3, Distance::Cosine, SOURCE_HASH);
    adapter
        .build(&[chunk("before", "alpha", vec![1.0, 0.0, 0.0])])
        .unwrap();
    adapter
        .insert(&chunk("after", "alpha", vec![0.0, 1.0, 0.0]))
        .unwrap();

    assert_eq!(adapter.count(), 2);
    let hits = adapter
        .query(&[0.0, 1.0, 0.0], 2, &FilterSet::default())
        .unwrap();
    assert_eq!(hits[0].chunk_id, "after");
    assert_eq!(hits[1].chunk_id, "before");
}

#[test]
fn heap_accounting_does_not_scale_with_vector_payload() {
    let dir = tempdir().unwrap();
    let small_path = dir.path().join("small.mmap");
    let large_path = dir.path().join("large.mmap");
    let small = vec![chunk("same", "alpha", vec![0.25; 8])];
    let large = vec![chunk("same", "alpha", vec![0.25; 1_048_576])];

    let mut small_adapter = MmapDenseAdapter::create(&small_path, 8, Distance::Dot, SOURCE_HASH);
    small_adapter.build(&small).unwrap();
    let mut large_adapter =
        MmapDenseAdapter::create(&large_path, 1_048_576, Distance::Dot, SOURCE_HASH);
    large_adapter.build(&large).unwrap();

    assert!(large_adapter.mapped_len() > small_adapter.mapped_len() + 4_000_000);
    assert_eq!(
        large_adapter.heap_bytes_upper_bound(),
        small_adapter.heap_bytes_upper_bound()
    );
    eprintln!(
        "fixture small_mapped={} large_mapped={} adapter_heap={} top_k=1",
        small_adapter.mapped_len(),
        large_adapter.mapped_len(),
        large_adapter.heap_bytes_upper_bound()
    );
    let hits = large_adapter
        .query(&vec![0.25; 1_048_576], 1, &FilterSet::default())
        .unwrap();
    assert_eq!(hits[0].chunk_id, "same");
    assert_eq!(large_adapter.last_query_stats().vectors_scored, 1);
}

fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap())
}
