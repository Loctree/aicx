use anyhow::Result;
use rmcp_memex::storage::{ChromaDocument, StorageManager};
use std::path::PathBuf;

/// Builds or updates the fast steer index using rmcp-memex LanceDB backend.
/// Only inserts new chunks. Doesn't perform embeddings, just stores metadata.
pub async fn sync_steer_index(new_files: &[&PathBuf]) -> Result<()> {
    if new_files.is_empty() {
        return Ok(());
    }

    let db_path = crate::store::store_base_dir()?.join("steer_db");
    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    storage.ensure_collection().await?;

    let mut docs = Vec::with_capacity(new_files.len());
    for file in new_files {
        let chunk_id = file
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();

        let sidecar = crate::store::load_sidecar(file);

        let mut meta = serde_json::Map::new();
        meta.insert(
            "path".to_string(),
            serde_json::Value::String(file.display().to_string()),
        );
        if let Some(s) = sidecar {
            if let Ok(val) = serde_json::to_value(s) {
                if let Some(obj) = val.as_object() {
                    for (k, v) in obj {
                        meta.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        docs.push(ChromaDocument::new_flat(
            chunk_id,
            "steer".to_string(),
            vec![0.0], // Dummy vector since we only care about metadata filtering
            serde_json::Value::Object(meta),
            "".to_string(),
        ));
    }

    // Batched add to avoid LanceDB memory issues
    for chunk in docs.chunks(1000) {
        storage.add_to_store(chunk.to_vec()).await?;
    }

    Ok(())
}

pub async fn query_steer_index() -> Result<Vec<ChromaDocument>> {
    let db_path = crate::store::store_base_dir()?.join("steer_db");
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    storage.get_all_in_namespace("steer").await
}

pub async fn rebuild_steer_index_if_needed() -> Result<()> {
    let all_files = crate::store::scan_context_files()?;
    if all_files.is_empty() {
        return Ok(());
    }

    let existing_docs = query_steer_index().await.unwrap_or_default();

    // Simple heuristic: if counts differ significantly, rebuild
    if (existing_docs.len() as isize - all_files.len() as isize).abs() > 10 {
        tracing::info!(
            "Rebuilding steer index ({} docs vs {} files)",
            existing_docs.len(),
            all_files.len()
        );

        let db_path = crate::store::store_base_dir()?.join("steer_db");
        let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
        let _ = storage.purge_namespace("steer").await;

        let paths: Vec<PathBuf> = all_files.into_iter().map(|f| f.path).collect();
        let path_refs: Vec<&PathBuf> = paths.iter().collect();
        // Since sync_steer_index expects new_files, and we purged, they are all new
        sync_steer_index(&path_refs).await?;
    }

    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub async fn search_steer_index(
    run_id: Option<&str>,
    prompt_id: Option<&str>,
    agent: Option<&str>,
    kind: Option<&str>,
    project: Option<&str>,
    date_lo: Option<&str>,
    date_hi: Option<&str>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    rebuild_steer_index_if_needed().await?;

    let docs = query_steer_index().await?;

    let project_lower = project.map(str::to_ascii_lowercase);
    let agent_lower = agent.map(str::to_ascii_lowercase);
    let kind_lower = kind.map(str::to_ascii_lowercase);

    let mut results = Vec::new();

    for doc in docs {
        if results.len() >= limit {
            break;
        }

        let meta = &doc.metadata;

        if let Some(ref needle) = project_lower {
            if let Some(p) = meta.get("project").and_then(|v| v.as_str()) {
                if !p.to_ascii_lowercase().contains(needle) {
                    continue;
                }
            } else {
                continue;
            }
        }
        if let Some(ref needle) = agent_lower {
            if let Some(a) = meta.get("agent").and_then(|v| v.as_str()) {
                if a.to_ascii_lowercase() != *needle {
                    continue;
                }
            } else {
                continue;
            }
        }
        if let Some(ref needle) = kind_lower {
            if let Some(k) = meta.get("kind").and_then(|v| v.as_str()) {
                if k.to_ascii_lowercase() != *needle {
                    continue;
                }
            } else {
                continue;
            }
        }
        if let Some(lo) = date_lo {
            if let Some(d) = meta.get("date").and_then(|v| v.as_str()) {
                if d < lo {
                    continue;
                }
            } else {
                continue;
            }
        }
        if let Some(hi) = date_hi {
            if let Some(d) = meta.get("date").and_then(|v| v.as_str()) {
                if d > hi {
                    continue;
                }
            } else {
                continue;
            }
        }

        if let Some(wanted) = run_id {
            if meta.get("run_id").and_then(|v| v.as_str()) != Some(wanted) {
                continue;
            }
        }
        if let Some(wanted) = prompt_id {
            if meta.get("prompt_id").and_then(|v| v.as_str()) != Some(wanted) {
                continue;
            }
        }

        results.push(doc.metadata.clone());
    }

    Ok(results)
}
