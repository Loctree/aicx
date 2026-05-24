use anyhow::Result;
use rmcp_memex::{search::BM25Index, storage::StorageManager};
use std::collections::HashSet;
use std::path::Path;

use super::documents::add_query_value;
use super::lifecycle::bootstrap_steer_index_if_missing_at;
use super::paths::{
    CANDIDATE_MULTIPLIER, MIN_CANDIDATES, STEER_NAMESPACE, steer_bm25_config, steer_bm25_path,
    steer_db_path,
};
use crate::steer_index_contract::SteerFilter;
use crate::timeline::FrameKind;

trait Bm25CandidateHit {
    fn into_hit(self) -> (String, f32);
}

impl Bm25CandidateHit for (String, f32) {
    fn into_hit(self) -> (String, f32) {
        self
    }
}

impl Bm25CandidateHit for (String, String, f32) {
    fn into_hit(self) -> (String, f32) {
        let (id, _namespace, score) = self;
        (id, score)
    }
}

pub(super) fn build_candidate_query(filter: &SteerFilter<'_>) -> Option<String> {
    let mut terms = Vec::new();

    add_query_value(&mut terms, filter.project);
    add_query_value(&mut terms, filter.agent);
    add_query_value(&mut terms, filter.kind);
    add_query_value(&mut terms, filter.frame_kind.map(FrameKind::as_str));
    add_query_value(&mut terms, filter.run_id);
    add_query_value(&mut terms, filter.prompt_id);

    if matches!((filter.date_lo, filter.date_hi), (Some(lo), Some(hi)) if lo == hi) {
        add_query_value(&mut terms, filter.date_lo);
    }

    if terms.is_empty() {
        None
    } else {
        Some(terms.join(" "))
    }
}

pub(super) fn metadata_matches(meta: &serde_json::Value, filter: &SteerFilter<'_>) -> bool {
    let project_lower = filter.project.map(str::to_ascii_lowercase);
    let agent_lower = filter.agent.map(str::to_ascii_lowercase);
    let kind_lower = filter.kind.map(str::to_ascii_lowercase);

    if let Some(ref needle) = project_lower {
        if let Some(p) = meta.get("project").and_then(|v| v.as_str()) {
            if !p.to_ascii_lowercase().contains(needle) {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(ref needle) = agent_lower {
        if let Some(a) = meta.get("agent").and_then(|v| v.as_str()) {
            if a.to_ascii_lowercase() != *needle {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(ref needle) = kind_lower {
        if let Some(k) = meta.get("kind").and_then(|v| v.as_str()) {
            if k.to_ascii_lowercase() != *needle {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(expected) = filter.frame_kind
        && meta.get("frame_kind").and_then(|v| v.as_str()) != Some(expected.as_str())
    {
        return false;
    }
    if let Some(lo) = filter.date_lo {
        if let Some(d) = meta.get("date").and_then(|v| v.as_str()) {
            if d < lo {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(hi) = filter.date_hi {
        if let Some(d) = meta.get("date").and_then(|v| v.as_str()) {
            if d > hi {
                return false;
            }
        } else {
            return false;
        }
    }
    if let Some(wanted) = filter.run_id
        && meta.get("run_id").and_then(|v| v.as_str()) != Some(wanted)
    {
        return false;
    }
    if let Some(wanted) = filter.prompt_id
        && meta.get("prompt_id").and_then(|v| v.as_str()) != Some(wanted)
    {
        return false;
    }

    true
}

pub(super) fn build_store_scan_metadata(
    file: &crate::store::StoredContextFile,
) -> serde_json::Value {
    let mut meta = serde_json::Map::new();
    meta.insert(
        "path".to_string(),
        serde_json::Value::String(file.path.display().to_string()),
    );
    meta.insert(
        "project".to_string(),
        serde_json::Value::String(file.project.clone()),
    );
    meta.insert(
        "agent".to_string(),
        serde_json::Value::String(file.agent.clone()),
    );
    meta.insert(
        "date".to_string(),
        serde_json::Value::String(file.date_iso.clone()),
    );
    meta.insert(
        "session_id".to_string(),
        serde_json::Value::String(file.session_id.clone()),
    );
    meta.insert(
        "kind".to_string(),
        serde_json::Value::String(file.kind.dir_name().to_string()),
    );

    if let Some(sidecar) = crate::store::load_sidecar(&file.path)
        && let Ok(val) = serde_json::to_value(sidecar)
        && let Some(obj) = val.as_object()
    {
        for (key, value) in obj {
            meta.insert(key.clone(), value.clone());
        }
    }

    serde_json::Value::Object(meta)
}

pub(super) fn search_store_scan_at(
    base: &Path,
    filter: &SteerFilter<'_>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let files = crate::store::scan_context_files_at(base)?;
    let mut results = Vec::new();

    for file in files.into_iter().rev() {
        let meta = build_store_scan_metadata(&file);
        if !metadata_matches(&meta, filter) {
            continue;
        }

        results.push(meta);
        if results.len() >= limit {
            break;
        }
    }

    Ok(results)
}

pub(super) async fn search_bm25_candidates_at(
    base: &Path,
    filter: &SteerFilter<'_>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let Some(query) = build_candidate_query(filter) else {
        return Ok(vec![]);
    };

    if !steer_bm25_path(base).exists() {
        bootstrap_steer_index_if_missing_at(base).await?;
        return Ok(vec![]);
    }

    let bm25 = BM25Index::new(&steer_bm25_config(base, true))?;
    if bm25.doc_count() == 0 {
        bootstrap_steer_index_if_missing_at(base).await?;
        return Ok(vec![]);
    }

    let candidate_limit = (limit.saturating_mul(CANDIDATE_MULTIPLIER)).max(MIN_CANDIDATES);
    let hits = bm25.search(&query, Some(STEER_NAMESPACE), candidate_limit)?;
    if hits.is_empty() {
        return Ok(vec![]);
    }

    let db_path = steer_db_path(base);
    if !db_path.exists() {
        return Ok(vec![]);
    }

    let storage = StorageManager::new_lance_only(&db_path.to_string_lossy()).await?;
    let mut seen_ids = HashSet::new();
    let mut results = Vec::new();

    for hit in hits {
        let (id, _score) = hit.into_hit();
        if !seen_ids.insert(id.clone()) {
            continue;
        }

        let Some(doc) = storage.get_document(STEER_NAMESPACE, &id).await? else {
            continue;
        };

        if !metadata_matches(&doc.metadata, filter) {
            continue;
        }

        results.push(doc.metadata);
        if results.len() >= limit {
            break;
        }
    }

    Ok(results)
}
