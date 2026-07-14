use std::path::{Path, PathBuf};

use crate::{chunker, sanitize};

use super::LOCT_CONTEXT_PACK_FAMILY;

/// Load the metadata sidecar for a context file, if it exists.
pub fn load_sidecar(chunk_path: &Path) -> Option<chunker::ChunkMetadataSidecar> {
    let sidecar_path = sidecar_path_for_chunk(chunk_path);
    load_sidecar_from_path(&sidecar_path)
}

pub fn sidecar_path_for_chunk(chunk_path: &Path) -> PathBuf {
    let adjacent = chunk_path.with_extension("meta.json");
    if adjacent.exists() {
        return adjacent;
    }
    if let (Some(parent), Some(stem)) = (chunk_path.parent(), chunk_path.file_stem()) {
        if parent.file_name().and_then(|name| name.to_str()) == Some("raw")
            && let Some(pack_dir) = parent.parent()
        {
            let sidecar = pack_dir
                .join("sidecars")
                .join(format!("{}.json", stem.to_string_lossy()));
            if sidecar.exists() {
                return sidecar;
            }
        }

        let sidecar = parent
            .join("sidecars")
            .join(format!("{}.json", stem.to_string_lossy()));
        if sidecar.exists() {
            return sidecar;
        }
    }
    adjacent
}

pub(super) fn load_sidecar_from_path(sidecar_path: &Path) -> Option<chunker::ChunkMetadataSidecar> {
    let sidecar_path = sanitize::validate_read_path(sidecar_path).ok()?;
    let content = sanitize::read_to_string_validated(&sidecar_path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn is_context_corpus_sidecar(sidecar: &chunker::ChunkMetadataSidecar) -> bool {
    sidecar.artifact_family.as_deref() == Some(LOCT_CONTEXT_PACK_FAMILY)
        || sidecar
            .truth_status
            .as_ref()
            .is_some_and(|status| status.role == chunker::TruthRole::Example)
}
