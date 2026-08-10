//! Metadata-only retrieval over the published hybrid CURRENT generation.
//!
//! `aicx steer`, MCP `aicx_steer`, and the dashboard all read the same
//! committed Tantivy documents as `aicx search`. There is no separate writer,
//! Lance database, BM25 side index, or store scan to drift from CURRENT.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use anyhow::{Context, Result, bail};

pub use crate::steer_index_contract::SteerFilter;

pub async fn query_steer_index_count() -> Result<usize> {
    Ok(open_current_adapter()?.doc_count)
}

pub async fn search_steer_index(
    filter: &SteerFilter<'_>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    let adapter = open_current_adapter()?;
    search_generation_metadata(&adapter, filter, limit)
}

fn open_current_adapter() -> Result<aicx_retrieve::TantivyAdapter> {
    let hybrid_root =
        crate::vector_index::hybrid_root_dir(None).context("resolve global hybrid index root")?;
    let generation = crate::vector_index::resolve_hybrid_generation_dir(&hybrid_root);
    let manifest_path = generation.join("manifest.json");
    if !manifest_path.is_file() {
        bail!(
            "published CURRENT index is missing at {}; run `aicx index`",
            manifest_path.display()
        );
    }
    let manifest = aicx_retrieve::Manifest::read_from_path(&manifest_path)
        .with_context(|| format!("read CURRENT manifest {}", manifest_path.display()))?;
    if !manifest
        .lexical_commit_id
        .starts_with(aicx_retrieve::TANTIVY_SCHEMA_VERSION)
    {
        bail!(
            "CURRENT lexical artifact is incompatible (commit {}); run `aicx index`",
            manifest.lexical_commit_id
        );
    }
    let tantivy_meta = generation
        .join(aicx_retrieve::TANTIVY_INDEX_DIR)
        .join("meta.json");
    if !tantivy_meta.is_file() {
        bail!(
            "CURRENT Tantivy metadata is missing at {}; run `aicx index`",
            tantivy_meta.display()
        );
    }
    aicx_retrieve::TantivyAdapter::new(generation).context("open CURRENT Tantivy metadata index")
}

fn search_generation_metadata(
    adapter: &aicx_retrieve::TantivyAdapter,
    filter: &SteerFilter<'_>,
    limit: usize,
) -> Result<Vec<serde_json::Value>> {
    if limit == 0 {
        return Ok(Vec::new());
    }
    let mut matches = adapter.scan_metadata(adapter.doc_count, |metadata| {
        metadata_matches(metadata, filter)
    })?;
    for metadata in &mut matches {
        if metadata.get("path").is_none()
            && let Some(source_path) = metadata.get("source_path").cloned()
            && let Some(object) = metadata.as_object_mut()
        {
            object.insert("path".to_string(), source_path);
        }
    }
    matches.sort_by(|left, right| {
        metadata_date(right)
            .cmp(metadata_date(left))
            .then_with(|| metadata_path(left).cmp(metadata_path(right)))
    });
    matches.truncate(limit);
    Ok(matches)
}

fn metadata_matches(metadata: &serde_json::Value, filter: &SteerFilter<'_>) -> bool {
    if let Some(project) = filter.project {
        let Some(stored) = metadata.get("project").and_then(|value| value.as_str()) else {
            return false;
        };
        let (organization, repository) = stored.split_once('/').unwrap_or(("", stored));
        if !crate::legacy_archive::project_filter_matches(organization, repository, project) {
            return false;
        }
    }
    exact_ci(metadata, "agent", filter.agent)
        && exact_ci(metadata, "kind", filter.kind)
        && exact(
            metadata,
            "frame_kind",
            filter.frame_kind.map(|kind| kind.as_str()),
        )
        && exact(metadata, "run_id", filter.run_id)
        && exact(metadata, "prompt_id", filter.prompt_id)
        && date_in_range(metadata, filter.date_lo, filter.date_hi)
}

fn exact(metadata: &serde_json::Value, key: &str, expected: Option<&str>) -> bool {
    expected
        .is_none_or(|expected| metadata.get(key).and_then(|value| value.as_str()) == Some(expected))
}

fn exact_ci(metadata: &serde_json::Value, key: &str, expected: Option<&str>) -> bool {
    expected.is_none_or(|expected| {
        metadata
            .get(key)
            .and_then(|value| value.as_str())
            .is_some_and(|actual| actual.eq_ignore_ascii_case(expected))
    })
}

fn date_in_range(
    metadata: &serde_json::Value,
    date_lo: Option<&str>,
    date_hi: Option<&str>,
) -> bool {
    if date_lo.is_none() && date_hi.is_none() {
        return true;
    }
    let Some(date) = metadata.get("date").and_then(|value| value.as_str()) else {
        return false;
    };
    date_lo.is_none_or(|lo| date >= lo) && date_hi.is_none_or(|hi| date <= hi)
}

fn metadata_date(metadata: &serde_json::Value) -> &str {
    metadata
        .get("timestamp")
        .and_then(|value| value.as_str())
        .or_else(|| metadata.get("date").and_then(|value| value.as_str()))
        .unwrap_or("")
}

fn metadata_path(metadata: &serde_json::Value) -> &str {
    metadata
        .get("path")
        .and_then(|value| value.as_str())
        .or_else(|| metadata.get("source_path").and_then(|value| value.as_str()))
        .unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use aicx_retrieve::{ChunkRef, LexicalIndex};
    use serde_json::json;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn current_metadata_filter_preserves_exact_ids_and_newest_order() {
        let temp = std::env::temp_dir().join(format!(
            "aicx-steer-current-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let mut adapter = aicx_retrieve::TantivyAdapter::new(temp.clone()).unwrap();
        let chunks = vec![
            chunk("older", "2026-07-20", "prompt-target"),
            chunk("newer", "2026-07-23", "prompt-target"),
            chunk("other", "2026-07-24", "prompt-other"),
        ];
        adapter.build(&chunks).unwrap();

        let filter = SteerFilter {
            prompt_id: Some("prompt-target"),
            project: Some("vetcoders/aicx"),
            ..SteerFilter::default()
        };
        let matches = search_generation_metadata(&adapter, &filter, 10).unwrap();

        assert_eq!(matches.len(), 2);
        assert_eq!(matches[0]["date"], "2026-07-23");
        assert_eq!(matches[0]["path"], "/tmp/newer.md");
        assert_eq!(matches[1]["date"], "2026-07-20");

        let _ = std::fs::remove_dir_all(temp);
    }

    fn chunk(id: &str, date: &str, prompt_id: &str) -> ChunkRef {
        ChunkRef {
            id: id.to_string(),
            source_path: format!("/tmp/{id}.md"),
            text: format!("body for {id}"),
            metadata: json!({
                "agent": "codex",
                "date": date,
                "project": "vetcoders/aicx",
                "prompt_id": prompt_id,
                "run_id": "run-1",
                "kind": "report",
            }),
        }
    }
}
