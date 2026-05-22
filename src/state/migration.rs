use super::{SeenHashSet, StateManager};
use anyhow::{Context, Result};
use siphasher::sip::SipHasher13;
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

pub const SIPHASH13_ALGORITHM: &str = "siphash13-v1";
pub const BLAKE3_128_ALGORITHM: &str = "blake3-128-v2";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StateMigrationReport {
    pub hash_algorithm_changed: bool,
    pub cleared_seen_hashes: usize,
    pub lowercased_seen_hash_buckets: usize,
    pub merged_seen_hash_buckets: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketCaseMerge {
    pub canonical_bucket: String,
    pub source_buckets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BucketCaseMergeScript {
    pub merges: Vec<BucketCaseMerge>,
    pub script: String,
}

pub fn stable_siphasher13() -> SipHasher13 {
    SipHasher13::new()
}

pub fn stable_blake3_128(input: &[u8]) -> String {
    let hash = blake3::hash(input);
    hex::encode(&hash.as_bytes()[..16])
}

pub fn is_legacy_siphash13_algorithm(hash_algorithm: &str) -> bool {
    hash_algorithm.trim() == SIPHASH13_ALGORITHM
}

pub fn canonical_state_bucket(project: &str) -> String {
    project
        .split('/')
        .map(|part| part.trim().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

pub fn migrate_loaded_state(state: &mut StateManager) -> StateMigrationReport {
    let mut report = StateMigrationReport::default();

    if state.hash_algorithm.trim() != BLAKE3_128_ALGORITHM {
        report.hash_algorithm_changed = true;
        report.cleared_seen_hashes = state.total_hashes();
        state.seen_hashes.clear();
        state.hash_algorithm = BLAKE3_128_ALGORITHM.to_string();
        return report;
    }

    let mut canonicalized: HashMap<String, SeenHashSet> = HashMap::new();
    for (bucket, hashes) in std::mem::take(&mut state.seen_hashes) {
        let canonical = canonical_state_bucket(&bucket);
        if canonical != bucket {
            report.lowercased_seen_hash_buckets += 1;
        }
        let existed = canonicalized.contains_key(&canonical);
        canonicalized
            .entry(canonical)
            .or_default()
            .extend_from(hashes);
        if existed {
            report.merged_seen_hash_buckets += 1;
        }
    }
    state.seen_hashes = canonicalized;
    report
}

pub fn generate_case_bucket_merge_script(store_root: &Path) -> Result<BucketCaseMergeScript> {
    let merges = plan_case_bucket_merges(store_root)?;
    let mut script = String::from(
        "#!/usr/bin/env bash\nset -euo pipefail\n\n# Review before running. This script only merges case variants into lowercase buckets.\n",
    );
    script.push_str(&format!(
        "STORE_ROOT={:?}\n",
        store_root.display().to_string()
    ));
    script.push_str("shopt -s dotglob nullglob\n\n");

    for merge in &merges {
        let canonical = shlex_quote_bucket(&merge.canonical_bucket)?;
        script.push_str(&format!("mkdir -p \"$STORE_ROOT\"/{canonical}\n"));
        for source in &merge.source_buckets {
            if source == &merge.canonical_bucket {
                continue;
            }
            let src = shlex_quote_bucket(source)?;
            script.push_str(&format!("if [[ -d \"$STORE_ROOT\"/{src} ]]; then\n"));
            script.push_str(&format!(
                "  mv -n \"$STORE_ROOT\"/{src}/* \"$STORE_ROOT\"/{canonical}/\n"
            ));
            script.push_str(&format!(
                "  rmdir \"$STORE_ROOT\"/{src} 2>/dev/null || true\n"
            ));
            script.push_str("fi\n");
        }
        script.push('\n');
    }

    Ok(BucketCaseMergeScript { merges, script })
}

/// Shell-quote a bucket name for safe embedding in the generated migration
/// script. Uses `shlex::try_quote` (single-quote-based, defangs `$(...)`,
/// backticks, `${...}`, `!`, all shell metacharacters). NUL byte in a bucket
/// name (impossible on POSIX filesystems) is the only `try_quote` failure
/// mode — surface it as an error so the script is not emitted at all.
fn shlex_quote_bucket(bucket: &str) -> Result<String> {
    shlex::try_quote(bucket)
        .map(|cow| cow.into_owned())
        .map_err(|e| anyhow::anyhow!("bucket {bucket:?} cannot be safely shell-quoted: {e}"))
}

fn plan_case_bucket_merges(store_root: &Path) -> Result<Vec<BucketCaseMerge>> {
    if !store_root.exists() {
        return Ok(Vec::new());
    }

    let mut grouped: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for bucket in discover_store_buckets(store_root)? {
        let canonical = canonical_state_bucket(&bucket);
        if canonical != bucket {
            grouped.entry(canonical).or_default().push(bucket);
        }
    }

    Ok(grouped
        .into_iter()
        .map(|(canonical_bucket, mut source_buckets)| {
            source_buckets.sort();
            source_buckets.dedup();
            BucketCaseMerge {
                canonical_bucket,
                source_buckets,
            }
        })
        .collect())
}

fn discover_store_buckets(store_root: &Path) -> Result<Vec<String>> {
    let mut buckets = Vec::new();
    for org_entry in crate::sanitize::read_dir_validated(store_root)
        .with_context(|| format!("read {}", store_root.display()))?
    {
        let org_entry = org_entry?;
        let org_path = org_entry.path();
        if !org_path.is_dir() {
            continue;
        }
        let org = org_entry.file_name().to_string_lossy().to_string();
        buckets.push(org.clone());

        let Ok(repo_entries) = crate::sanitize::read_dir_validated(&org_path) else {
            continue;
        };
        for repo_entry in repo_entries.filter_map(|entry| entry.ok()) {
            let repo_path = repo_entry.path();
            if !repo_path.is_dir() || looks_like_date_dir(&repo_path) {
                continue;
            }
            let repo = repo_entry.file_name().to_string_lossy().to_string();
            buckets.push(format!("{org}/{repo}"));
        }
    }
    Ok(buckets)
}

fn looks_like_date_dir(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            name.len() == 9
                && name.as_bytes().get(4) == Some(&b'_')
                && name.chars().filter(|ch| ch.is_ascii_digit()).count() == 8
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::StateManager;

    #[test]
    fn test_migration_clears_state_on_algorithm_bump() {
        let mut state = StateManager {
            hash_algorithm: SIPHASH13_ALGORITHM.to_string(),
            ..Default::default()
        };
        state
            .seen_hashes
            .insert("test".to_string(), crate::state::SeenHashSet::default());
        state
            .seen_hashes
            .get_mut("test")
            .unwrap()
            .insert("somehash".to_string());

        let report = migrate_loaded_state(&mut state);
        assert!(report.hash_algorithm_changed);
        assert_eq!(report.cleared_seen_hashes, 1);
        assert!(state.seen_hashes.is_empty());
        assert_eq!(state.hash_algorithm, BLAKE3_128_ALGORITHM);
    }

    #[test]
    fn test_blake3_v1_migration_clears_state_on_v2_bump() {
        let mut state = StateManager {
            hash_algorithm: "blake3-128-v1".to_string(),
            ..Default::default()
        };
        state
            .seen_hashes
            .entry("test".to_string())
            .or_default()
            .insert("old-blake3-v1-hash".to_string());

        let report = migrate_loaded_state(&mut state);

        assert!(report.hash_algorithm_changed);
        assert_eq!(report.cleared_seen_hashes, 1);
        assert!(state.seen_hashes.is_empty());
        assert_eq!(state.hash_algorithm, BLAKE3_128_ALGORITHM);
    }

    #[test]
    fn test_blake3_128_collision_resistance() {
        let mut hashes = std::collections::HashSet::new();
        for i in 0..1000 {
            let input = format!("test input {i}");
            let hash = stable_blake3_128(input.as_bytes());
            assert!(hashes.insert(hash));
        }
    }

    #[test]
    fn test_blake3_128_matches_hex_prefix_contract() {
        let input = b"x";
        let old_prefix_contract = blake3::hash(input).to_hex()[..32].to_string();

        let hash = stable_blake3_128(input);

        assert_eq!(hash, old_prefix_contract);
        assert_eq!(hash.len(), 32);
    }
}
