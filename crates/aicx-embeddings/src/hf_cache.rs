use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

/// Lookup verdict used when the cache directory exists but the snapshot it
/// contains is unusable (D-8). Returned via the verbose lookup so callers
/// can emit an actionable message instead of the bland "model not hydrated"
/// generic. Only the `gguf` backend consumes the `Partial` payload today;
/// the public surface stays compiled in the default build so embedder
/// callers can match on it without flipping features.
#[derive(Debug)]
#[allow(dead_code)] // fields consumed by feature-gated backends (gguf)
pub enum HfCacheMiss {
    /// No HF cache base on this host (or no matching repo). Operator must
    /// run `hf download` (or equivalent) first.
    NotPresent,
    /// At least one repo dir exists, but the file is missing, empty, or
    /// not a regular file. `path` is the exact location that failed the
    /// completeness check; `reason` describes what was wrong.
    Partial { path: PathBuf, reason: String },
}

impl HfCacheMiss {
    #[allow(dead_code)] // consumed by feature-gated backends (gguf)
    pub fn into_error(self, repo: &str, filename: &str) -> anyhow::Error {
        match self {
            HfCacheMiss::NotPresent => anyhow::anyhow!(
                "HF cache lookup for {repo} ({filename}) found no snapshot. \
                 Run `hf download {repo} {filename}`, or set AICX_EMBEDDER_PATH \
                 to a local file."
            ),
            HfCacheMiss::Partial { path, reason } => anyhow::anyhow!(
                "HF cache for {repo} is partially hydrated: {reason} at {}. \
                 Re-run `hf download {repo} {filename}` to repair, or delete \
                 the partial snapshot and retry.",
                path.display()
            ),
        }
    }
}

pub fn find_snapshot_with_file(repo: &str, filename: &str) -> Option<PathBuf> {
    find_snapshot_with_file_verbose(repo, filename).ok()
}

/// Like [`find_snapshot_with_file`] but reports *why* a lookup failed when
/// any candidate repo dir exists on disk. Lets the embedder bootstrap
/// surface a precise error path instead of a bland "not hydrated".
pub fn find_snapshot_with_file_verbose(repo: &str, filename: &str) -> Result<PathBuf, HfCacheMiss> {
    let mut partial: Option<HfCacheMiss> = None;
    for base in cache_bases() {
        match find_snapshot_in_base(&base, repo, filename) {
            Ok(snapshot) => return Ok(snapshot),
            Err(miss @ HfCacheMiss::Partial { .. }) if partial.is_none() => {
                partial = Some(miss);
            }
            _ => {}
        }
    }
    Err(partial.unwrap_or(HfCacheMiss::NotPresent))
}

fn cache_bases() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(path) = env::var("AICX_HF_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HUGGINGFACE_HUB_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HF_HUB_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HF_HOME") {
        out.push(PathBuf::from(path).join("hub"));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".cache").join("huggingface").join("hub"));
        out.push(home.join(".aicx").join("embeddings"));
        out.push(home.join(".aicx").join("embeddings").join("hub"));
    }
    out.sort();
    out.dedup();
    out
}

fn find_snapshot_in_base(base: &Path, repo: &str, filename: &str) -> Result<PathBuf, HfCacheMiss> {
    let repo_dir = base.join(format!("models--{}", repo.replace('/', "--")));
    let snapshots_dir = repo_dir.join("snapshots");

    let snapshots_dir = if snapshots_dir.exists() {
        snapshots_dir
    } else {
        let target = repo.to_ascii_lowercase();
        let mut matched: Option<PathBuf> = None;
        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("models--") {
                    continue;
                }
                let repo_id = name
                    .strip_prefix("models--")
                    .unwrap_or("")
                    .replace("--", "/");
                if repo_id.to_ascii_lowercase() == target {
                    matched = Some(entry.path().join("snapshots"));
                    break;
                }
            }
        }
        match matched {
            Some(dir) => dir,
            None => return Err(HfCacheMiss::NotPresent),
        }
    };

    let entries = match fs::read_dir(&snapshots_dir) {
        Ok(entries) => entries,
        Err(_) => return Err(HfCacheMiss::NotPresent),
    };
    let mut best: Option<(SystemTime, PathBuf)> = None;
    let mut partial_signal: Option<HfCacheMiss> = None;

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let file_path = path.join(filename);
        match validate_cache_file(&file_path) {
            Ok(()) => {}
            Err(reason) => {
                if partial_signal.is_none() && file_path.exists() {
                    partial_signal = Some(HfCacheMiss::Partial {
                        path: file_path,
                        reason,
                    });
                }
                continue;
            }
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        match &best {
            Some((best_time, _)) if *best_time >= modified => {}
            _ => best = Some((modified, path)),
        }
    }

    match best {
        Some((_, p)) => Ok(p),
        None => Err(partial_signal.unwrap_or(HfCacheMiss::NotPresent)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "aicx-hf-cache-test-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn validate_cache_file_rejects_zero_byte_file() {
        let dir = tempdir();
        let path = dir.join("model.gguf");
        std::fs::write(&path, b"").unwrap();
        let err = validate_cache_file(&path).expect_err("empty file must be rejected");
        assert!(err.contains("0 bytes"), "error must mention size: {err}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_cache_file_accepts_non_empty_file() {
        let dir = tempdir();
        let path = dir.join("model.gguf");
        std::fs::write(&path, b"gguf-magic-and-payload").unwrap();
        assert!(validate_cache_file(&path).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn validate_cache_file_rejects_directory() {
        let dir = tempdir();
        let path = dir.join("not_a_file");
        std::fs::create_dir(&path).unwrap();
        let err = validate_cache_file(&path).expect_err("directory must be rejected");
        assert!(err.contains("not a regular file"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn find_snapshot_with_file_verbose_returns_not_present_on_empty_cache() {
        // No env vars + temp HOME = no candidate base contains the repo.
        // We can't fully isolate cache_bases here (it reads env+home), but
        // we can call the API on a repo no sane host will have and assert
        // we surface a structured miss.
        let miss = find_snapshot_with_file_verbose(
            "nonexistent-org/nonexistent-repo-aicx-d8-test",
            "model.gguf",
        );
        assert!(miss.is_err());
        match miss.unwrap_err() {
            HfCacheMiss::NotPresent => {}
            HfCacheMiss::Partial { path, reason } => {
                panic!("unexpected Partial({path:?}, {reason})");
            }
        }
    }
}

/// Verify that `path` is a non-empty regular file. Returns the failure
/// reason so callers can build a precise partial-cache error.
fn validate_cache_file(path: &Path) -> Result<(), String> {
    let metadata = match fs::metadata(path) {
        Ok(meta) => meta,
        Err(err) => {
            return Err(format!("cannot stat cache file: {err}"));
        }
    };
    if !metadata.is_file() {
        return Err("entry is not a regular file".to_string());
    }
    if metadata.len() == 0 {
        return Err("cache file is 0 bytes (likely truncated download)".to_string());
    }
    Ok(())
}
