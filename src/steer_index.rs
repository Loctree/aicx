use anyhow::Result;
use rmcp_memex::storage::{ChromaDocument, StorageManager};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const STEER_NAMESPACE: &str = "steer";

fn chunk_id_for_path(file: &Path) -> String {
    file.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn build_steer_doc(file: &Path) -> ChromaDocument {
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

    ChromaDocument::new_flat(
        chunk_id_for_path(file),
        STEER_NAMESPACE.to_string(),
        vec![0.0], // Dummy vector since we only care about metadata filtering
        serde_json::Value::Object(meta),
        "".to_string(),
    )
}

fn doc_ids(docs: &[ChromaDocument]) -> HashSet<String> {
    docs.iter().map(|doc| doc.id.clone()).collect()
}

fn file_ids(files: &[crate::store::StoredContextFile]) -> HashSet<String> {
    files
        .iter()
        .map(|file| chunk_id_for_path(&file.path))
        .collect()
}

fn steer_index_needs_rebuild(existing_ids: &HashSet<String>, store_ids: &HashSet<String>) -> bool {
    existing_ids != store_ids
}

async fn sync_steer_index_at(base: &Path, new_files: &[&PathBuf]) -> Result<()> {
    let db_path = base.join("steer_db");
    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    storage.ensure_collection().await?;

    let docs: Vec<ChromaDocument> = new_files
        .iter()
        .map(|file| build_steer_doc(file.as_path()))
        .collect();

    for doc in &docs {
        let _ = storage.delete_document(STEER_NAMESPACE, &doc.id).await;
    }

    for chunk in docs.chunks(1000) {
        storage.add_to_store(chunk.to_vec()).await?;
    }

    Ok(())
}

async fn query_steer_index_at(base: &Path) -> Result<Vec<ChromaDocument>> {
    let db_path = base.join("steer_db");
    if !db_path.exists() {
        return Ok(vec![]);
    }
    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    storage.get_all_in_namespace(STEER_NAMESPACE).await
}

async fn rebuild_steer_index_if_needed_at(base: &Path) -> Result<()> {
    let all_files = crate::store::scan_context_files_at(base)?;
    if all_files.is_empty() {
        return Ok(());
    }

    let existing_docs = query_steer_index_at(base).await.unwrap_or_default();
    let existing_ids = doc_ids(&existing_docs);
    let store_ids = file_ids(&all_files);

    if steer_index_needs_rebuild(&existing_ids, &store_ids) {
        tracing::info!(
            "Rebuilding steer index ({} docs vs {} files)",
            existing_ids.len(),
            store_ids.len()
        );

        let db_path = base.join("steer_db");
        let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
        let _ = storage.purge_namespace(STEER_NAMESPACE).await;

        let paths: Vec<PathBuf> = all_files.into_iter().map(|f| f.path).collect();
        let path_refs: Vec<&PathBuf> = paths.iter().collect();
        sync_steer_index_at(base, &path_refs).await?;
    }

    Ok(())
}

/// Builds or updates the fast steer index using rmcp-memex LanceDB backend.
/// Treats the sidecar as the source of truth for every touched chunk.
pub async fn sync_steer_index(new_files: &[&PathBuf]) -> Result<()> {
    if new_files.is_empty() {
        return Ok(());
    }

    let base = crate::store::store_base_dir()?;
    sync_steer_index_at(&base, new_files).await
}

pub async fn query_steer_index() -> Result<Vec<ChromaDocument>> {
    let base = crate::store::store_base_dir()?;
    query_steer_index_at(&base).await
}

pub async fn rebuild_steer_index_if_needed() -> Result<()> {
    let base = crate::store::store_base_dir()?;
    rebuild_steer_index_if_needed_at(&base).await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunker::ChunkMetadataSidecar;
    use crate::store::Kind;
    use std::fs;

    fn write_chunk_with_sidecar(
        base: &Path,
        file_name: &str,
        run_id: &str,
        prompt_id: &str,
    ) -> PathBuf {
        let chunk_path = base
            .join("store")
            .join("VetCoders")
            .join("ai-contexters")
            .join("2026_0331")
            .join("reports")
            .join("codex")
            .join(file_name);
        fs::create_dir_all(chunk_path.parent().unwrap()).unwrap();
        fs::write(&chunk_path, "# chunk\n\nbody").unwrap();

        let sidecar = ChunkMetadataSidecar {
            id: chunk_id_for_path(&chunk_path),
            project: "VetCoders/ai-contexters".to_string(),
            agent: "codex".to_string(),
            date: "2026-03-31".to_string(),
            session_id: "sess-1".to_string(),
            cwd: Some("/Users/tester/workspaces/ai-contexters".to_string()),
            kind: Kind::Reports,
            run_id: Some(run_id.to_string()),
            prompt_id: Some(prompt_id.to_string()),
            agent_model: Some("gpt-5.4".to_string()),
            started_at: Some("2026-03-31T16:00:00Z".to_string()),
            completed_at: Some("2026-03-31T16:05:00Z".to_string()),
            token_usage: Some(1200),
            findings_count: Some(2),
            workflow_phase: Some("marbles".to_string()),
            mode: Some("session-first".to_string()),
            skill_code: Some("vc-marbles".to_string()),
            framework_version: Some("2026-03".to_string()),
        };

        fs::write(
            chunk_path.with_extension("meta.json"),
            serde_json::to_string(&sidecar).unwrap(),
        )
        .unwrap();

        chunk_path
    }

    #[test]
    fn rebuild_detects_small_id_drift() {
        let existing_ids = HashSet::from([
            "2026_0331_codex_sess1_001".to_string(),
            "2026_0331_codex_sess1_002".to_string(),
        ]);
        let store_ids = HashSet::from([
            "2026_0331_codex_sess1_001".to_string(),
            "2026_0331_codex_sess2_001".to_string(),
        ]);

        assert!(steer_index_needs_rebuild(&existing_ids, &store_ids));
    }

    #[test]
    fn sync_replaces_existing_sidecar_metadata() {
        let temp = std::env::temp_dir().join(format!(
            "ai-ctx-steer-index-{}-{}",
            std::process::id(),
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&temp).unwrap();

        let chunk_path =
            write_chunk_with_sidecar(&temp, "2026_0331_codex_sess1_001.md", "mrbl-001", "p1");
        let rt = tokio::runtime::Runtime::new().unwrap();
        let first_refs = vec![&chunk_path];
        rt.block_on(sync_steer_index_at(&temp, &first_refs))
            .unwrap();

        let mut updated_sidecar = crate::store::load_sidecar(&chunk_path).unwrap();
        updated_sidecar.run_id = Some("mrbl-002".to_string());
        updated_sidecar.prompt_id = Some("p2".to_string());
        fs::write(
            chunk_path.with_extension("meta.json"),
            serde_json::to_string(&updated_sidecar).unwrap(),
        )
        .unwrap();

        let second_refs = vec![&chunk_path];
        rt.block_on(sync_steer_index_at(&temp, &second_refs))
            .unwrap();

        let docs = rt.block_on(query_steer_index_at(&temp)).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(
            docs[0].metadata.get("run_id").and_then(|v| v.as_str()),
            Some("mrbl-002")
        );
        assert_eq!(
            docs[0].metadata.get("prompt_id").and_then(|v| v.as_str()),
            Some("p2")
        );

        let _ = fs::remove_dir_all(&temp);
    }
}
