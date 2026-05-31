use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet, hash_map::Entry};
use std::path::{Path, PathBuf};

use crate::sanitize;

use super::{read_store_dir, sidecar::load_sidecar_from_path};

pub(super) fn content_sha256(content: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(content.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Stream-hash a file's bytes into a hex SHA-256, with an explicit
/// 64 KiB read buffer and a hard 8 MiB cap (matching
/// `sanitize::MAX_VALIDATED_BYTES`).
///
/// This is a one-time-per-orphan cost outside the hot dedup path, so it
/// is intentionally uncached.
pub(super) fn sha256_of_file(path: &Path) -> Result<String> {
    use std::io::Read;

    let display_path = path.display().to_string();
    let file = sanitize::open_file_validated(path)
        .with_context(|| format!("Failed to open orphan chunk {}", display_path))?;
    let mut reader = std::io::BufReader::new(file);
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    let mut total: usize = 0;
    loop {
        let n = reader
            .read(&mut buf)
            .with_context(|| format!("Failed to read orphan chunk {}", display_path))?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n);
        if total > sanitize::MAX_VALIDATED_BYTES {
            anyhow::bail!(
                "Orphan chunk {} exceeds the {} byte read cap",
                display_path,
                sanitize::MAX_VALIDATED_BYTES
            );
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

pub fn content_sha256_exists_in_dir(dir: &Path, content_sha256: &str) -> Result<bool> {
    Ok(content_sha256s_in_dir(dir)?.contains(content_sha256))
}

fn content_sha256s_in_dir(dir: &Path) -> Result<HashSet<String>> {
    let mut hashes = HashSet::new();
    if !dir.exists() {
        return Ok(hashes);
    }
    for entry in read_store_dir(dir)?.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_none_or(|name| !name.ends_with(".meta.json"))
        {
            continue;
        }
        let Some(sidecar) = load_sidecar_from_path(&path) else {
            continue;
        };
        if let Some(content_sha256) = sidecar.content_sha256 {
            hashes.insert(content_sha256);
        }
    }
    Ok(hashes)
}

#[derive(Debug, Default)]
pub(super) struct DirShaCache {
    by_dir: HashMap<PathBuf, HashSet<String>>,
}

impl DirShaCache {
    pub(super) fn contains(&mut self, dir: &Path, sha: &str) -> Result<bool> {
        let hashes = match self.by_dir.entry(dir.to_path_buf()) {
            Entry::Occupied(entry) => entry.into_mut(),
            Entry::Vacant(entry) => entry.insert(content_sha256s_in_dir(dir)?),
        };
        Ok(hashes.contains(sha))
    }

    pub(super) fn insert(&mut self, dir: &Path, sha: String) {
        self.by_dir
            .entry(dir.to_path_buf())
            .or_default()
            .insert(sha);
    }
}
