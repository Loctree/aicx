// Vibecrafted with AI Agents by VetCoders (c)2024-2026 LibraxisAI
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use aicx_retrieve::{
    ChunkRef, FilterSet, LexicalIndex, LexicalQuery, TANTIVY_INDEX_DIR, TantivyAdapter,
};
use serde_json::json;
use tempfile::TempDir;

fn chunk(id: usize, text: &str, agent: &str) -> ChunkRef {
    ChunkRef {
        id: format!("chunk-{id:03}"),
        source_path: format!("/tmp/source-{id:03}.md"),
        text: text.to_string(),
        metadata: json!({
            "agent": agent,
            "date": "20260515",
            "project": "aicx",
        }),
    }
}

fn query(text: &str, limit: usize) -> LexicalQuery {
    LexicalQuery {
        text: text.to_string(),
        limit,
        filters: FilterSet::default(),
    }
}

fn filter_query(text: &str, limit: usize, key: &str, value: &str) -> LexicalQuery {
    let mut values = BTreeMap::new();
    values.insert(key.to_string(), json!(value));
    LexicalQuery {
        text: text.to_string(),
        limit,
        filters: FilterSet { values },
    }
}

#[test]
fn build_and_query_smoke() {
    let temp = TempDir::new().unwrap();
    let mut adapter = TantivyAdapter::new(temp.path().to_path_buf()).unwrap();
    let chunks: Vec<_> = (0..50)
        .map(|i| chunk(i, &format!("foo body text number {i}"), "codex"))
        .collect();

    adapter.build(&chunks).unwrap();
    let hits = adapter.query(&query("foo", 5)).unwrap();

    assert!(!hits.is_empty());
    assert!(temp.path().join(TANTIVY_INDEX_DIR).exists());
}

#[test]
fn filter_pre_pass_limits_hits_to_agent() {
    let temp = TempDir::new().unwrap();
    let mut adapter = TantivyAdapter::new(temp.path().to_path_buf()).unwrap();
    let chunks: Vec<_> = (0..50)
        .map(|i| {
            let agent = if i % 2 == 0 { "codex" } else { "claude" };
            chunk(i, "shared foo search body", agent)
        })
        .collect();

    adapter.build(&chunks).unwrap();
    let hits = adapter
        .query(&filter_query("foo", 25, "agent", "codex"))
        .unwrap();

    assert!(!hits.is_empty());
    assert!(
        hits.iter()
            .all(|hit| hit.metadata.get("agent") == Some(&json!("codex")))
    );
}

#[test]
fn commit_id_is_stable_across_noop_queries() {
    let temp = TempDir::new().unwrap();
    let mut adapter = TantivyAdapter::new(temp.path().to_path_buf()).unwrap();
    let chunks: Vec<_> = (0..10)
        .map(|i| chunk(i, "stable foo text", "codex"))
        .collect();

    adapter.build(&chunks).unwrap();
    let before = adapter.commit_id().clone();
    adapter.query(&query("foo", 3)).unwrap();
    adapter.query(&query("foo", 3)).unwrap();

    assert_eq!(&before, adapter.commit_id());
}

#[test]
fn doc_count_matches_insert_count() {
    let temp = TempDir::new().unwrap();
    let mut adapter = TantivyAdapter::new(temp.path().to_path_buf()).unwrap();
    let chunks: Vec<_> = (0..100)
        .map(|i| chunk(i, "counted foo text", "codex"))
        .collect();

    adapter.build(&chunks).unwrap();

    assert_eq!(adapter.doc_count(), 100);
}

#[test]
fn per_field_tokenizer_handles_identifiers_and_stems() {
    let temp = TempDir::new().unwrap();
    let mut adapter = TantivyAdapter::new(temp.path().to_path_buf()).unwrap();
    let chunks = vec![chunk(
        1,
        "try_semantic_search returns Vec<QueryHit>",
        "codex",
    )];

    adapter.build(&chunks).unwrap();

    assert!(
        !adapter
            .query(&query("try_semantic_search", 5))
            .unwrap()
            .is_empty()
    );
    assert!(!adapter.query(&query("semantic", 5)).unwrap().is_empty());

    // Tantivy's SimpleTokenizer treats `_` as a separator here, so this prefix
    // currently resolves to the `try` token and is expected to hit.
    assert!(!adapter.query(&query("try_", 5)).unwrap().is_empty());
}

#[test]
fn pl_and_en_stemming_both_hit_mixed_language_body() {
    let temp = TempDir::new().unwrap();
    let mut adapter = TantivyAdapter::new(temp.path().to_path_buf()).unwrap();
    let chunks = vec![chunk(
        1,
        "naprawiony auth bug w session middleware",
        "codex",
    )];

    adapter.build(&chunks).unwrap();

    assert!(!adapter.query(&query("napraw", 5)).unwrap().is_empty());
    assert!(!adapter.query(&query("middleware", 5)).unwrap().is_empty());
}

#[test]
fn local_store_self_recall_at_5_when_available() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let store = PathBuf::from(home).join(".aicx/store");
    if !store.exists() {
        return;
    }

    let mut paths = Vec::new();
    collect_markdown_files(&store, &mut paths);
    paths.sort();

    let chunks: Vec<_> = paths
        .into_iter()
        .filter_map(|path| chunk_from_store_path(&store, &path).ok())
        .filter(|chunk| deterministic_query_token(&chunk.text).is_some())
        .take(100)
        .collect();
    if chunks.len() < 100 {
        return;
    }

    let temp = TempDir::new().unwrap();
    let mut adapter = TantivyAdapter::new(temp.path().to_path_buf()).unwrap();
    adapter.build(&chunks).unwrap();

    let mut evaluated = 0usize;
    let mut recalled = 0usize;
    for chunk in &chunks {
        let Some(term) = deterministic_query_token(&chunk.text) else {
            continue;
        };
        evaluated += 1;
        let hits = adapter.query(&query(&term, 5)).unwrap();
        if hits.iter().any(|hit| hit.chunk_id == chunk.id) {
            recalled += 1;
        }
    }

    assert!(evaluated >= 70);
    let recall = recalled as f32 / evaluated as f32;
    assert!(
        recall >= 0.7,
        "local store self-recall@5 {recall:.2} below 0.70 ({recalled}/{evaluated})"
    );
}

fn collect_markdown_files(dir: &Path, paths: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_markdown_files(&path, paths);
        } else if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            paths.push(path);
        }
    }
}

fn chunk_from_store_path(store: &Path, path: &Path) -> std::io::Result<ChunkRef> {
    let text = fs::read_to_string(path)?;
    let relative = path.strip_prefix(store).unwrap_or(path);
    let mut components = relative
        .components()
        .filter_map(|component| component.as_os_str().to_str());
    let owner = components.next().unwrap_or("unknown");
    let repo = components.next().unwrap_or("unknown");
    let agent = path
        .parent()
        .and_then(Path::file_name)
        .and_then(|name| name.to_str())
        .unwrap_or("unknown");

    Ok(ChunkRef {
        id: relative.to_string_lossy().to_string(),
        source_path: path.to_string_lossy().to_string(),
        text,
        metadata: json!({
            "agent": agent,
            "date": "unknown",
            "project": format!("{owner}/{repo}"),
        }),
    })
}

fn deterministic_query_token(text: &str) -> Option<String> {
    const STOPWORDS: &[&str] = &[
        "assistant",
        "because",
        "codex",
        "content",
        "context",
        "message",
        "system",
        "thinking",
        "user",
    ];

    text.split(|ch: char| !(ch.is_alphanumeric() || ch == '_'))
        .filter_map(|token| {
            let token = token.trim_matches('_').to_lowercase();
            if token.len() >= 8 && !STOPWORDS.contains(&token.as_str()) {
                Some(token)
            } else {
                None
            }
        })
        .max_by_key(|token| token.len())
}
