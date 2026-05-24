use rmcp_memex::storage::ChromaDocument;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use super::paths::{STEER_NAMESPACE, STEER_SENTINEL_DIMENSION};

pub(super) fn chunk_id_for_path(file: &Path) -> String {
    file.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn push_unique_term(terms: &mut Vec<String>, term: String) {
    if !term.is_empty() && !terms.iter().any(|existing| existing == &term) {
        terms.push(term);
    }
}

fn searchable_terms(value: &str) -> Vec<String> {
    let mut terms = Vec::new();
    let lower = value.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return terms;
    }

    push_unique_term(&mut terms, lower.clone());

    let compact: String = lower
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .collect();
    if !compact.is_empty() {
        push_unique_term(&mut terms, compact);
    }

    for token in lower
        .split(|ch: char| !ch.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
    {
        push_unique_term(&mut terms, token.to_string());
    }

    terms
}

fn add_searchable_value(terms: &mut Vec<String>, label: &str, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };

    for term in searchable_terms(value) {
        push_unique_term(terms, term.clone());
        push_unique_term(terms, format!("{label}:{term}"));
    }
}

pub(super) fn add_query_value(terms: &mut Vec<String>, value: Option<&str>) {
    let Some(value) = value else {
        return;
    };

    for term in searchable_terms(value) {
        push_unique_term(terms, term);
    }
}

fn build_steer_metadata(file: &Path) -> serde_json::Value {
    let sidecar = crate::store::load_sidecar(file);

    let mut meta = serde_json::Map::new();
    meta.insert(
        "path".to_string(),
        serde_json::Value::String(file.display().to_string()),
    );
    if let Some(s) = sidecar
        && let Ok(val) = serde_json::to_value(s)
        && let Some(obj) = val.as_object()
    {
        for (k, v) in obj {
            meta.insert(k.clone(), v.clone());
        }
    }

    serde_json::Value::Object(meta)
}

fn build_steer_search_text(meta: &serde_json::Map<String, serde_json::Value>) -> String {
    let mut terms = Vec::new();

    add_searchable_value(
        &mut terms,
        "project",
        meta.get("project").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "agent",
        meta.get("agent").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "kind",
        meta.get("kind").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "frame_kind",
        meta.get("frame_kind").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "date",
        meta.get("date").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "run_id",
        meta.get("run_id").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "prompt_id",
        meta.get("prompt_id").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "session_id",
        meta.get("session_id").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "workflow_phase",
        meta.get("workflow_phase").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "mode",
        meta.get("mode").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "skill_code",
        meta.get("skill_code").and_then(|v| v.as_str()),
    );
    add_searchable_value(
        &mut terms,
        "framework_version",
        meta.get("framework_version").and_then(|v| v.as_str()),
    );

    terms.join(" ")
}

pub(super) fn build_steer_doc(file: &Path) -> ChromaDocument {
    let metadata = build_steer_metadata(file);
    let text = metadata
        .as_object()
        .map(build_steer_search_text)
        .unwrap_or_default();

    ChromaDocument::new_flat(
        chunk_id_for_path(file),
        STEER_NAMESPACE.to_string(),
        vec![0.0; STEER_SENTINEL_DIMENSION], // Explicit sentinel vector for metadata-only index
        metadata,
        text,
    )
}

pub(super) fn doc_ids(docs: &[ChromaDocument]) -> HashSet<String> {
    docs.iter().map(|doc| doc.id.clone()).collect()
}

pub(super) fn file_ids(files: &[crate::store::StoredContextFile]) -> HashSet<String> {
    files
        .iter()
        .map(|file| chunk_id_for_path(&file.path))
        .collect()
}

pub(super) fn steer_index_needs_rebuild(
    existing_ids: &HashSet<String>,
    store_ids: &HashSet<String>,
) -> bool {
    existing_ids != store_ids
}

pub(super) fn build_steer_docs(new_files: &[&PathBuf]) -> Vec<ChromaDocument> {
    new_files
        .iter()
        .map(|file| build_steer_doc(file.as_path()))
        .collect()
}
