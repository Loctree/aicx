//! One-time explicit project-identity migration (O2, problem-log
//! 2026-07-17 15:18 UTC).
//!
//! The immutable-identity doctrine is correct: query surfaces read the
//! persisted identity and never silently re-derive it. That same doctrine
//! faithfully preserves a wrong historical casing — `index.json` still
//! carries pre-rename keys like `VetCoders/CodeScribe` while fresh
//! derivation (`canonical_repo_label`) emits `vetcoders/codescribe`, and a
//! case-insensitive filesystem (APFS) masks the store-directory split that
//! WILL materialize on a case-sensitive one. The remedy class is therefore
//! an explicit, operator-triggered, one-time migration op — never a silent
//! re-derivation.
//!
//! Scope:
//! - rename persisted `index.json` project keys to the GitHub
//!   nameWithOwner canon (lowercase segments, the fresh-derivation form),
//!   merging entries when both casings coexist (case-sensitive split);
//! - normalize `store/` org/repo directory names, with a two-step rename
//!   through a temporary name for case-only renames on case-insensitive
//!   filesystems;
//! - historical canonical-projection cards: ANNOTATE, never rewrite —
//!   cards are immutable provenance artifacts (doctrine 1b6f5df); the
//!   manifest records an alias map `old -> new` and query matching is
//!   case-insensitive, so reads keep working;
//! - typo-twin buckets (edit distance <= 1 within one org, e.g.
//!   `ai-contexers`/`ai-contexters`): DETECTED and REPORTED only — a
//!   bucket merge is destructive and stays an operator decision.
//!
//! Dry-run is the default. `execute` mutates only what the manifest
//! plans: index keys and directory names. Doctor never deletes store
//! contents; directory merges move files and record conflicts instead of
//! overwriting.

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::sanitize;
use crate::store::atomic_write::atomic_write;
use crate::store::paths::{identity_migration_manifest_path, identity_migration_report_path};

/// Directories under `store/` that are reserved surfaces, not project
/// buckets — never candidates for identity renames.
const RESERVED_STORE_DIRS: &[&str] = &["canonical-projection-v1"];

const IDENTITY_MIGRATION_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IndexKeyRename {
    pub from: String,
    pub to: String,
    /// True when the canonical key already exists in `index.json`
    /// (case-sensitive split) and the migration merges both entries.
    pub merge_into_existing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreDirRename {
    /// Path relative to the canonical store root (`store/`).
    pub from: String,
    pub to: String,
    /// True when source and target differ only by ASCII case — on a
    /// case-insensitive filesystem this needs a two-step rename through
    /// a temporary name.
    pub case_only: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CardAlias {
    /// Card file name under `store/canonical-projection-v1/cards/`.
    pub card: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TypoTwinPair {
    pub organization: String,
    pub repo_a: String,
    pub repo_b: String,
    pub recommendation: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MigrationStep {
    pub source_identity: String,
    pub target_identity: String,
    pub source_hash: String,
    pub operation: String, // "rename_dir", "rename_index_key", "annotate_card", "quarantine_deprecated"
    pub precondition: String,
    pub result: String, // "pending", "success", "failed: ...", "conflicted"
    pub recovery_reference: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdentityMigrationManifest {
    pub schema_version: u32,
    pub generated_at: String,
    /// `"dry-run"` or `"apply"`.
    pub mode: String,
    pub index_key_renames: Vec<IndexKeyRename>,
    pub dir_renames: Vec<StoreDirRename>,
    /// ANNOTATE policy: alias map only; card payloads stay byte-identical.
    pub card_aliases: Vec<CardAlias>,
    /// Report-only: merging twin buckets is destructive → operator button.
    pub typo_twins: Vec<TypoTwinPair>,
    /// Non-fatal problems hit during apply (e.g. merge file conflicts).
    pub conflicts: Vec<String>,
    #[serde(default)]
    pub steps: Vec<MigrationStep>,
}

#[derive(Debug)]
pub struct IdentityMigrationOutcome {
    pub manifest: IdentityMigrationManifest,
    pub manifest_path: PathBuf,
    pub report_path: PathBuf,
    pub applied: bool,
}

fn is_deprecated_checkout(name: &str) -> bool {
    let lowercase = name.to_ascii_lowercase();
    lowercase.ends_with("-deprecated")
        || lowercase.ends_with("_deprecated")
        || lowercase.ends_with(".deprecated")
        || lowercase.ends_with(" deprecated")
        || lowercase.ends_with("-depr")
        || lowercase.ends_with("_depr")
        || lowercase.ends_with(".depr")
        || lowercase.ends_with(" depr")
}

fn safe_timestamp(generated_at: &str) -> String {
    generated_at
        .chars()
        .filter(|c| c.is_ascii_digit())
        .take(14)
        .collect::<String>()
}

fn compute_dir_hash(dir: &Path) -> Result<String> {
    if !dir.is_dir() {
        return Ok("empty-or-missing".to_string());
    }
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        if !current.exists() {
            continue;
        }
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();

    let mut hasher = Sha256::new();
    for file in files {
        let relative_path = file.strip_prefix(dir)?;
        hasher.update(relative_path.to_string_lossy().as_bytes());
        if let Ok(content) = fs::read(&file) {
            hasher.update(&content);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

#[allow(dead_code)]
pub fn compute_store_recursive_hash(store_root: &Path) -> Result<String> {
    if !store_root.is_dir() {
        return Ok("empty-or-missing".to_string());
    }
    let mut files = Vec::new();
    let mut stack = vec![store_root.to_path_buf()];
    while let Some(current) = stack.pop() {
        if !current.exists() {
            continue;
        }
        let dir_name = current.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if RESERVED_STORE_DIRS.contains(&dir_name)
            || dir_name == "migration"
            || dir_name == "quarantine"
        {
            continue;
        }
        for entry in fs::read_dir(&current)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();

    let mut hasher = Sha256::new();
    for file in files {
        let relative_path = file.strip_prefix(store_root)?;
        hasher.update(relative_path.to_string_lossy().as_bytes());
        if let Ok(content) = fs::read(&file) {
            hasher.update(&content);
        }
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn compute_index_key_hash(index: &crate::store::StoreIndex, key: &str) -> String {
    if let Some(project) = index.projects.get(key) {
        let json = serde_json::to_string(project).unwrap_or_default();
        let mut hasher = Sha256::new();
        hasher.update(json.as_bytes());
        format!("{:x}", hasher.finalize())
    } else {
        "missing".to_string()
    }
}

/// Lowercase-segment canon — the same shape `canonical_repo_label`
/// derives for fresh ingests and GitHub uses for nameWithOwner matching.
fn identity_canon(slug: &str) -> String {
    slug.split('/')
        .map(|segment| segment.trim().to_ascii_lowercase())
        .collect::<Vec<_>>()
        .join("/")
}

/// Compact store date dirs (`2026_0717`) live at the same tree depth as
/// repo dirs in single-segment buckets; they are never identity segments.
fn is_compact_date_dir(name: &str) -> bool {
    name.len() == 9
        && name.as_bytes()[4] == b'_'
        && name[..4].bytes().all(|b| b.is_ascii_digit())
        && name[5..].bytes().all(|b| b.is_ascii_digit())
}

fn edit_distance_at_most_one(a: &str, b: &str) -> bool {
    if a == b {
        return false; // identical is not a twin pair
    }
    let (a, b) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    if b.len() - a.len() > 1 {
        return false;
    }
    let a_bytes = a.as_bytes();
    let b_bytes = b.as_bytes();
    if a.len() == b.len() {
        return a_bytes.iter().zip(b_bytes).filter(|(x, y)| x != y).count() == 1;
    }
    // One insertion: walk both, allow a single skip in the longer string.
    let (mut i, mut j, mut skipped) = (0usize, 0usize, false);
    while i < a_bytes.len() && j < b_bytes.len() {
        if a_bytes[i] == b_bytes[j] {
            i += 1;
            j += 1;
        } else if skipped {
            return false;
        } else {
            skipped = true;
            j += 1;
        }
    }
    true
}

fn list_dir_names(dir: &Path) -> Result<Vec<String>> {
    let mut names = Vec::new();
    if !dir.is_dir() {
        return Ok(names);
    }
    // Traversal-checked, allowed-base-enforced read (store.rs::read_store_dir
    // wraps sanitize::validate_dir_path before touching the filesystem).
    for entry in super::super::read_store_dir(dir)? {
        let entry = entry?;
        if entry.path().is_dir() {
            names.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    Ok(names)
}

/// Read-only planning pass over `index.json`, `store/` directories and
/// canonical-projection cards.
pub fn plan_identity_migration_at(base: &Path) -> Result<IdentityMigrationManifest> {
    let index = super::super::load_index_at(base)?;

    let mut index_key_renames = Vec::new();
    let mut keys: Vec<&String> = index.projects.keys().collect();
    keys.sort();
    for key in keys {
        let canon = identity_canon(key);
        if canon != *key {
            index_key_renames.push(IndexKeyRename {
                from: key.clone(),
                to: canon.clone(),
                merge_into_existing: index.projects.contains_key(&canon),
            });
        }
    }

    // Directory scan: level 1 (org or single-segment bucket) and level 2
    // (repo). Repo-level renames are listed first so apply can run them
    // under the still-original org path before the org itself is renamed.
    let store_root = base.join(super::super::CANONICAL_STORE_DIRNAME);
    let mut repo_renames = Vec::new();
    let mut org_renames = Vec::new();
    let mut slugs_by_org: BTreeMap<String, Vec<String>> = BTreeMap::new();

    let generated_at = Utc::now().to_rfc3339();
    let timestamp = safe_timestamp(&generated_at);

    for org in list_dir_names(&store_root)? {
        if RESERVED_STORE_DIRS.contains(&org.as_str()) || org == "migration" || org == "quarantine"
        {
            continue;
        }

        if is_deprecated_checkout(&org) {
            let relative_from = org.clone();
            let relative_to = format!("quarantine/identity-{}/{}", timestamp, org);
            org_renames.push(StoreDirRename {
                from: relative_from,
                to: relative_to,
                case_only: false,
            });
            continue;
        }

        let org_canon = identity_canon(&org);
        for repo in list_dir_names(&store_root.join(&org))? {
            if is_compact_date_dir(&repo) {
                continue;
            }
            if is_deprecated_checkout(&repo) {
                let relative_from = format!("{}/{}", org, repo);
                let relative_to = format!("quarantine/identity-{}/{}/{}", timestamp, org, repo);
                repo_renames.push(StoreDirRename {
                    from: relative_from,
                    to: relative_to,
                    case_only: false,
                });
                continue;
            }
            let repo_canon = identity_canon(&repo);
            slugs_by_org
                .entry(org_canon.clone())
                .or_default()
                .push(repo_canon.clone());
            if repo_canon != repo {
                repo_renames.push(StoreDirRename {
                    from: format!("{org}/{repo}"),
                    to: format!("{org}/{repo_canon}"),
                    case_only: repo.eq_ignore_ascii_case(&repo_canon),
                });
            }
        }
        if org_canon != org {
            org_renames.push(StoreDirRename {
                // Canon also trims segments, so an org name carrying
                // whitespace differs by more than casing — compute instead
                // of assuming lowercase-only drift.
                case_only: org.eq_ignore_ascii_case(&org_canon),
                from: org.clone(),
                to: org_canon,
            });
        }
    }
    let mut dir_renames = repo_renames;
    dir_renames.append(&mut org_renames);

    // Twin detection also sees index keys (a split can live only there).
    for key in index.projects.keys() {
        let canon = identity_canon(key);
        if let Some((org, repo)) = canon.split_once('/') {
            slugs_by_org
                .entry(org.to_string())
                .or_default()
                .push(repo.to_string());
        }
    }
    let mut typo_twins = Vec::new();
    for (org, mut repos) in slugs_by_org {
        repos.sort();
        repos.dedup();
        for i in 0..repos.len() {
            for j in (i + 1)..repos.len() {
                if edit_distance_at_most_one(&repos[i], &repos[j]) {
                    typo_twins.push(TypoTwinPair {
                        organization: org.clone(),
                        repo_a: repos[i].clone(),
                        repo_b: repos[j].clone(),
                        recommendation: format!(
                            "likely typo twins `{org}/{}` vs `{org}/{}` — merging buckets is destructive; operator decides after reviewing both",
                            repos[i], repos[j]
                        ),
                    });
                }
            }
        }
    }

    // Historical canonical-projection cards: annotate, never rewrite.
    let mut card_aliases = Vec::new();
    let cards_dir = store_root.join("canonical-projection-v1").join("cards");
    if cards_dir.is_dir() {
        let mut card_files: Vec<PathBuf> = super::super::read_store_dir(&cards_dir)?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
            .collect();
        card_files.sort();
        for card_path in card_files {
            let Ok(raw) = sanitize::read_to_string_validated(&card_path) else {
                continue;
            };
            let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
                continue;
            };
            // card3 schema carries `project` as an object with a `slug`
            // field; older shapes carried a bare string. Accept both.
            let project = value.get("project").and_then(|p| {
                p.as_str()
                    .or_else(|| p.get("slug").and_then(|slug| slug.as_str()))
            });
            if let Some(project) = project {
                let canon = identity_canon(project);
                if canon != project {
                    card_aliases.push(CardAlias {
                        card: card_path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        from: project.to_string(),
                        to: canon,
                    });
                }
            }
        }
    }

    // Generate steps for the state machine
    let mut steps = Vec::new();
    for rename in &dir_renames {
        let from_path = store_root.join(&rename.from);
        let source_hash = compute_dir_hash(&from_path).unwrap_or_else(|_| "missing".to_string());
        let operation = if rename.to.starts_with("quarantine/") {
            "quarantine_deprecated".to_string()
        } else {
            "rename_dir".to_string()
        };
        steps.push(MigrationStep {
            source_identity: rename.from.clone(),
            target_identity: rename.to.clone(),
            source_hash,
            operation,
            precondition: "source_exists_and_hash_matches".to_string(),
            result: "pending".to_string(),
            recovery_reference: Some(rename.from.clone()),
        });
    }

    for rename in &index_key_renames {
        let source_hash = compute_index_key_hash(&index, &rename.from);
        steps.push(MigrationStep {
            source_identity: rename.from.clone(),
            target_identity: rename.to.clone(),
            source_hash,
            operation: "rename_index_key".to_string(),
            precondition: "source_key_exists_and_hash_matches".to_string(),
            result: "pending".to_string(),
            recovery_reference: serde_json::to_string(index.projects.get(&rename.from).unwrap())
                .ok(),
        });
    }

    for alias in &card_aliases {
        steps.push(MigrationStep {
            source_identity: alias.card.clone(),
            target_identity: alias.to.clone(),
            source_hash: "".to_string(),
            operation: "annotate_card".to_string(),
            precondition: "card_exists".to_string(),
            result: "pending".to_string(),
            recovery_reference: Some(alias.from.clone()),
        });
    }

    Ok(IdentityMigrationManifest {
        schema_version: IDENTITY_MIGRATION_SCHEMA_VERSION,
        generated_at,
        mode: "dry-run".to_string(),
        index_key_renames,
        dir_renames,
        card_aliases,
        typo_twins,
        conflicts: Vec::new(),
        steps,
    })
}

/// Rename `from` to `to`, handling the two hostile filesystem cases:
/// case-only renames on case-insensitive filesystems (two-step through a
/// temporary sibling) and genuine splits on case-sensitive filesystems
/// (recursive merge-move that records conflicts instead of overwriting).
fn rename_dir_safely(from: &Path, to: &Path, conflicts: &mut Vec<String>) -> Result<()> {
    if !from.is_dir() {
        conflicts.push(format!(
            "dir rename skipped, source missing: {}",
            from.display()
        ));
        return Ok(());
    }
    let same_parent = from
        .parent()
        .zip(to.parent())
        .is_some_and(|(p1, p2)| p1 == p2);
    let same_fold = same_parent
        && from
            .file_name()
            .zip(to.file_name())
            .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b.to_string_lossy().as_ref()));

    // Ensure the parent directory of target exists
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent)?;
    }

    // Distinct target already exists → case-sensitive split → merge-move.
    if to.exists() && !same_fold_is_same_dir(from, to, same_fold) {
        merge_move(from, to, conflicts)?;
        return Ok(());
    }

    if same_fold {
        // Two-step rename: on a case-insensitive filesystem `from` and
        // `to` are the same directory entry, and a direct rename is
        // allowed to fail or no-op depending on the filesystem.
        let tmp_name = format!(
            ".identity-migration-tmp-{}",
            to.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default()
        );
        let tmp = to.with_file_name(tmp_name);
        if tmp.exists() {
            anyhow::bail!(
                "two-step rename temp path already exists: {}",
                tmp.display()
            );
        }
        fs::rename(from, &tmp)
            .with_context(|| format!("rename {} -> {}", from.display(), tmp.display()))?;
        fs::rename(&tmp, to)
            .with_context(|| format!("rename {} -> {}", tmp.display(), to.display()))?;
        return Ok(());
    }

    fs::rename(from, to).with_context(|| format!("rename {} -> {}", from.display(), to.display()))
}

/// On a case-insensitive filesystem `to.exists()` is true for a case-only
/// rename because it resolves to the SAME directory as `from`. Treat that
/// as "not a split" so the two-step path handles it.
fn same_fold_is_same_dir(from: &Path, to: &Path, same_fold: bool) -> bool {
    if !same_fold {
        return false;
    }
    // If the parent listing contains the source spelling but not a distinct
    // target spelling, `to` resolves to `from` (case-insensitive FS).
    let Some(parent) = from.parent() else {
        return false;
    };
    let Ok(names) = list_dir_names(parent) else {
        return false;
    };
    let from_name = from.file_name().map(|n| n.to_string_lossy().into_owned());
    let to_name = to.file_name().map(|n| n.to_string_lossy().into_owned());
    match (from_name, to_name) {
        (Some(from_name), Some(to_name)) => names.contains(&from_name) && !names.contains(&to_name),
        _ => false,
    }
}

/// Recursive merge-move: move every file from `from` into `to`, creating
/// directories as needed. Existing target files are conflicts — recorded,
/// never overwritten (doctor never deletes store contents). Source dirs
/// are removed only when emptied.
fn merge_move(from: &Path, to: &Path, conflicts: &mut Vec<String>) -> Result<()> {
    fs::create_dir_all(to).with_context(|| format!("create_dir_all {}", to.display()))?;
    for entry in super::super::read_store_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            merge_move(&src, &dst, conflicts)?;
        } else if dst.exists() {
            conflicts.push(format!(
                "merge conflict, target exists (kept both): {} vs {}",
                src.display(),
                dst.display()
            ));
        } else {
            fs::rename(&src, &dst)
                .with_context(|| format!("rename {} -> {}", src.display(), dst.display()))?;
        }
    }
    match fs::remove_dir(from) {
        Ok(()) => {}
        Err(_) => conflicts.push(format!(
            "source dir left in place (not empty after merge): {}",
            from.display()
        )),
    }
    Ok(())
}

fn execute_step(base: &Path, step: &mut MigrationStep, conflicts: &mut Vec<String>) -> Result<()> {
    if step.result == "success" {
        return Ok(());
    }

    let store_root = base.join(super::super::CANONICAL_STORE_DIRNAME);

    match step.operation.as_str() {
        "rename_dir" | "quarantine_deprecated" => {
            let from_path = store_root.join(&step.source_identity);
            let to_path = if step.target_identity.starts_with("quarantine/") {
                base.join(&step.target_identity)
            } else {
                store_root.join(&step.target_identity)
            };

            // Check precondition: source exists
            if !from_path.is_dir() {
                step.result = "failed: source missing".to_string();
                conflicts.push(format!(
                    "Precondition failed: source missing at {}",
                    from_path.display()
                ));
                anyhow::bail!(
                    "Precondition failed: source missing at {}",
                    from_path.display()
                );
            }

            // Check precondition: source hash matches (only for repo-level directories)
            if step.source_identity.contains('/') {
                let current_hash = compute_dir_hash(&from_path)?;
                if current_hash != step.source_hash {
                    step.result = "failed: source hash changed".to_string();
                    conflicts.push(format!(
                        "Precondition failed: source hash changed for {}",
                        step.source_identity
                    ));
                    anyhow::bail!(
                        "Precondition failed: source hash changed for {}",
                        step.source_identity
                    );
                }
            }

            // Run the operation
            let same_fold = from_path
                .file_name()
                .zip(to_path.file_name())
                .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b.to_string_lossy().as_ref()));

            if to_path.exists() && !same_fold_is_same_dir(&from_path, &to_path, same_fold) {
                merge_move(&from_path, &to_path, conflicts)?;
                step.result = "conflicted".to_string();
            } else {
                rename_dir_safely(&from_path, &to_path, conflicts)?;
                step.result = "success".to_string();
            }
        }
        "rename_index_key" => {
            let mut index = super::super::load_index_at(base)?;
            if !index.projects.contains_key(&step.source_identity) {
                step.result = "failed: source key missing".to_string();
                conflicts.push(format!(
                    "Precondition failed: source index key missing: {}",
                    step.source_identity
                ));
                anyhow::bail!(
                    "Precondition failed: source index key missing: {}",
                    step.source_identity
                );
            }

            let current_hash = compute_index_key_hash(&index, &step.source_identity);
            if current_hash != step.source_hash {
                step.result = "failed: source hash changed".to_string();
                conflicts.push(format!(
                    "Precondition failed: source index key hash changed: {}",
                    step.source_identity
                ));
                anyhow::bail!(
                    "Precondition failed: source index key hash changed: {}",
                    step.source_identity
                );
            }

            let source = index.projects.remove(&step.source_identity).unwrap();
            let target = index
                .projects
                .entry(step.target_identity.clone())
                .or_default();
            for (agent, agent_index) in source.agents {
                let merged = target.agents.entry(agent).or_default();
                for date in agent_index.dates {
                    if !merged.dates.contains(&date) {
                        merged.dates.push(date);
                    }
                }
                merged.dates.sort();
                merged.total_entries += agent_index.total_entries;
                if agent_index.last_updated > merged.last_updated {
                    merged.last_updated = agent_index.last_updated;
                }
            }
            index.last_updated = Utc::now();
            super::super::save_index_at(base, &index)?;
            step.result = "success".to_string();
        }
        "annotate_card" => {
            let card_path = store_root
                .join("canonical-projection-v1/cards")
                .join(&step.source_identity);
            if !card_path.is_file() {
                step.result = "failed: card missing".to_string();
                conflicts.push(format!(
                    "Precondition failed: card file missing at {}",
                    card_path.display()
                ));
                anyhow::bail!(
                    "Precondition failed: card file missing at {}",
                    card_path.display()
                );
            }
            step.result = "success".to_string();
        }
        _ => {
            anyhow::bail!("Unknown operation: {}", step.operation);
        }
    }

    Ok(())
}

/// Execute the planned renames: store directories first (repo level, then
/// org level, exactly in manifest order), then the `index.json` keys.
pub fn execute_identity_migration_at(
    base: &Path,
    manifest: &mut IdentityMigrationManifest,
) -> Result<()> {
    let mut conflicts = std::mem::take(&mut manifest.conflicts);

    let manifest_path = identity_migration_manifest_path(base);
    let save_manifest = |manifest: &IdentityMigrationManifest| -> Result<()> {
        let manifest_json = serde_json::to_string_pretty(manifest)?;
        atomic_write(&manifest_path, manifest_json.as_bytes())?;
        Ok(())
    };

    save_manifest(manifest)?;

    for i in 0..manifest.steps.len() {
        if manifest.steps[i].result == "success" {
            continue;
        }
        let mut step = manifest.steps[i].clone();
        let res = execute_step(base, &mut step, &mut conflicts);
        manifest.steps[i] = step;
        save_manifest(manifest)?;

        if let Err(e) = res {
            manifest.conflicts = conflicts;
            manifest.mode = "apply-failed".to_string();
            let _ = save_manifest(manifest);
            return Err(e);
        }
    }

    manifest.conflicts = conflicts;
    manifest.mode = "apply".to_string();
    save_manifest(manifest)?;

    Ok(())
}

fn render_identity_report(manifest: &IdentityMigrationManifest) -> String {
    let mut out = String::new();
    out.push_str("# Project-identity migration report\n\n");
    out.push_str(&format!(
        "- mode: **{}**\n- generated_at: {}\n- schema_version: {}\n\n",
        manifest.mode, manifest.generated_at, manifest.schema_version
    ));

    if !manifest.steps.is_empty() {
        out.push_str("## Migration Steps (State Machine)\n\n");
        out.push_str("| Operation | Source | Target | Hash | Precondition | Status |\n");
        out.push_str("|---|---|---|---|---|---|\n");
        for step in &manifest.steps {
            out.push_str(&format!(
                "| `{}` | `{}` | `{}` | `{}` | `{}` | **{}** |\n",
                step.operation,
                step.source_identity,
                step.target_identity,
                step.source_hash,
                step.precondition,
                step.result
            ));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "## Index key renames ({})\n\n",
        manifest.index_key_renames.len()
    ));
    for rename in &manifest.index_key_renames {
        out.push_str(&format!(
            "- `{}` -> `{}`{}\n",
            rename.from,
            rename.to,
            if rename.merge_into_existing {
                " (merges into existing canonical key)"
            } else {
                ""
            }
        ));
    }

    out.push_str(&format!(
        "\n## Store directory renames ({})\n\n",
        manifest.dir_renames.len()
    ));
    for rename in &manifest.dir_renames {
        out.push_str(&format!(
            "- `store/{}` -> `store/{}`{}\n",
            rename.from,
            rename.to,
            if rename.case_only {
                " (case-only; two-step rename)"
            } else {
                ""
            }
        ));
    }

    out.push_str(&format!(
        "\n## Historical cards — ANNOTATED, not rewritten ({})\n\n",
        manifest.card_aliases.len()
    ));
    out.push_str(
        "Cards are immutable provenance artifacts; queries match identities case-insensitively, so reads keep working. Alias map:\n\n",
    );
    for alias in &manifest.card_aliases {
        out.push_str(&format!(
            "- `{}`: `{}` -> `{}`\n",
            alias.card, alias.from, alias.to
        ));
    }

    out.push_str(&format!(
        "\n## Typo-twin buckets — report only ({})\n\n",
        manifest.typo_twins.len()
    ));
    for twins in &manifest.typo_twins {
        out.push_str(&format!("- {}\n", twins.recommendation));
    }

    if !manifest.conflicts.is_empty() {
        out.push_str(&format!(
            "\n## Conflicts ({})\n\n",
            manifest.conflicts.len()
        ));
        for conflict in &manifest.conflicts {
            out.push_str(&format!("- {conflict}\n"));
        }
    }

    if manifest.mode == "dry-run" || manifest.mode == "apply-failed" {
        out.push_str(
            "\n## Operator buttons\n\n- `aicx doctor --migrate-identities --apply` — execute the renames above\n- typo-twin merges: separate decision, not part of `--apply`\n\nNote: `--apply` re-plans against the store state at execution time; if the store changes between this dry-run and the apply, the executed plan may differ from this report.\n",
        );
    }
    out
}

#[allow(dead_code)]
pub fn rollback_identity_migration_at(base: &Path) -> Result<()> {
    let manifest_path = identity_migration_manifest_path(base);
    if !manifest_path.exists() {
        anyhow::bail!("No identity migration manifest found to rollback");
    }
    let raw = fs::read_to_string(&manifest_path)?;
    let mut manifest: IdentityMigrationManifest = serde_json::from_str(&raw)?;

    let store_root = base.join(super::super::CANONICAL_STORE_DIRNAME);
    let conflicts = Vec::new();

    for step in manifest.steps.iter_mut().rev() {
        if step.result != "success" && step.result != "conflicted" {
            continue;
        }

        match step.operation.as_str() {
            "rename_dir" | "quarantine_deprecated" => {
                let current_path = if step.target_identity.starts_with("quarantine/") {
                    base.join(&step.target_identity)
                } else {
                    store_root.join(&step.target_identity)
                };
                let original_path = store_root.join(&step.source_identity);

                if current_path.exists() {
                    if let Some(parent) = original_path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let same_fold = current_path
                        .file_name()
                        .zip(original_path.file_name())
                        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b.to_string_lossy().as_ref()));

                    if same_fold {
                        let tmp_name = format!(
                            ".identity-migration-rollback-tmp-{}",
                            original_path
                                .file_name()
                                .map(|n| n.to_string_lossy().into_owned())
                                .unwrap_or_default()
                        );
                        let tmp = original_path.with_file_name(tmp_name);
                        fs::rename(&current_path, &tmp)?;
                        fs::rename(&tmp, &original_path)?;
                    } else {
                        fs::rename(&current_path, &original_path)?;
                    }
                }
                step.result = "pending".to_string();
            }
            "rename_index_key" => {
                let mut index = super::super::load_index_at(base)?;
                index.projects.remove(&step.target_identity);
                if let Some(project_index) = step
                    .recovery_reference
                    .as_ref()
                    .and_then(|r| serde_json::from_str(r).ok())
                {
                    index
                        .projects
                        .insert(step.source_identity.clone(), project_index);
                }
                index.last_updated = Utc::now();
                super::super::save_index_at(base, &index)?;
                step.result = "pending".to_string();
            }
            "annotate_card" => {
                step.result = "pending".to_string();
            }
            _ => {}
        }
    }

    manifest.mode = "dry-run".to_string();
    manifest.conflicts = conflicts;

    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    atomic_write(&manifest_path, manifest_json.as_bytes())?;

    let report_path = identity_migration_report_path(base);
    atomic_write(&report_path, render_identity_report(&manifest).as_bytes())?;

    Ok(())
}

/// Plan (always) + execute (only with `apply`) + persist manifest/report
/// under `<base>/migration/`. Dry-run never touches `index.json` or any
/// `store/` path.
pub fn run_identity_migration_at(base: &Path, apply: bool) -> Result<IdentityMigrationOutcome> {
    let manifest_path = identity_migration_manifest_path(base);
    let mut manifest = if apply && manifest_path.exists() {
        let raw = fs::read_to_string(&manifest_path)?;
        if let Ok(m) = serde_json::from_str::<IdentityMigrationManifest>(&raw) {
            if !m.steps.is_empty() && (m.mode == "apply-failed" || m.mode == "dry-run") {
                m
            } else {
                plan_identity_migration_at(base)?
            }
        } else {
            plan_identity_migration_at(base)?
        }
    } else {
        plan_identity_migration_at(base)?
    };

    if apply {
        execute_identity_migration_at(base, &mut manifest)?;
    }

    let report_path = identity_migration_report_path(base);
    if let Some(parent) = manifest_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create_dir_all {}", parent.display()))?;
    }
    let manifest_json =
        serde_json::to_string_pretty(&manifest).context("serialize identity migration manifest")?;
    atomic_write(&manifest_path, manifest_json.as_bytes())?;
    atomic_write(&report_path, render_identity_report(&manifest).as_bytes())?;

    Ok(IdentityMigrationOutcome {
        applied: apply,
        manifest,
        manifest_path,
        report_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{AgentIndex, ProjectIndex, StoreIndex};
    use std::collections::HashMap;

    // SYNTHETIC fixture store, shaped after the live drift documented in
    // ~/.aicx/aicx-problems.md (2026-07-17 15:18 UTC): pre-rename cased
    // index key + store dirs, plus the ai-contexers/ai-contexters twins.
    fn fixture_base(label: &str) -> PathBuf {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let base = std::env::temp_dir().join(format!(
            "aicx-identity-migration-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn agent_index(entries: usize) -> AgentIndex {
        AgentIndex {
            dates: vec!["2026-07-17".to_string()],
            total_entries: entries,
            last_updated: Utc::now(),
        }
    }

    fn write_index(base: &Path, keys: &[(&str, usize)]) {
        let mut index = StoreIndex::default();
        for (key, entries) in keys {
            let mut agents = HashMap::new();
            agents.insert("claude".to_string(), agent_index(*entries));
            index
                .projects
                .insert((*key).to_string(), ProjectIndex { agents });
        }
        let json = serde_json::to_string_pretty(&index).unwrap();
        fs::write(base.join("index.json"), json).unwrap();
    }

    fn seed_store(base: &Path) {
        let store = base.join("store");
        fs::create_dir_all(store.join("VetCoders/CodeScribe/2026_0717/context/claude")).unwrap();
        fs::write(
            store.join("VetCoders/CodeScribe/2026_0717/context/claude/chunk1.md"),
            "chunk payload",
        )
        .unwrap();
        fs::create_dir_all(store.join("VetCoders/ai-contexers")).unwrap();
        fs::create_dir_all(store.join("VetCoders/ai-contexters")).unwrap();
        let cards = store.join("canonical-projection-v1/cards");
        fs::create_dir_all(&cards).unwrap();
        // card3 shape: `project` is an object carrying the persisted slug.
        fs::write(
            cards.join("card3_76b8e606.json"),
            r#"{"session_id":"s1","project":{"slug":"VetCoders/CodeScribe","attribution":{"kind":"inferred","version":"project-bucket-v1"}}}"#,
        )
        .unwrap();
        // Legacy shape: bare string project — must also be annotated.
        fs::write(
            cards.join("card2_legacy.json"),
            r#"{"session_id":"s2","project":"VetCoders/CodeScribe"}"#,
        )
        .unwrap();
    }

    fn snapshot(dir: &Path) -> Vec<String> {
        let mut acc = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(current) = stack.pop() {
            let Ok(entries) = fs::read_dir(&current) else {
                continue;
            };
            for entry in entries.filter_map(Result::ok) {
                let path = entry.path();
                acc.push(path.strip_prefix(dir).unwrap().display().to_string());
                if path.is_dir() {
                    stack.push(path);
                }
            }
        }
        acc.sort();
        acc
    }

    #[test]
    fn plan_detects_casing_drift_cards_and_typo_twins() {
        let base = fixture_base("plan");
        write_index(&base, &[("VetCoders/CodeScribe", 42)]);
        seed_store(&base);

        let manifest = plan_identity_migration_at(&base).unwrap();

        assert_eq!(
            manifest.index_key_renames,
            vec![IndexKeyRename {
                from: "VetCoders/CodeScribe".to_string(),
                to: "vetcoders/codescribe".to_string(),
                merge_into_existing: false,
            }]
        );
        // Repo-level rename precedes the org-level rename.
        assert_eq!(
            manifest.dir_renames,
            vec![
                StoreDirRename {
                    from: "VetCoders/CodeScribe".to_string(),
                    to: "VetCoders/codescribe".to_string(),
                    case_only: true,
                },
                StoreDirRename {
                    from: "VetCoders".to_string(),
                    to: "vetcoders".to_string(),
                    case_only: true,
                },
            ]
        );
        assert_eq!(
            manifest.card_aliases.len(),
            2,
            "{:?}",
            manifest.card_aliases
        );
        assert!(manifest.card_aliases.iter().all(|alias| {
            alias.from == "VetCoders/CodeScribe" && alias.to == "vetcoders/codescribe"
        }));
        assert_eq!(manifest.typo_twins.len(), 1);
        assert_eq!(manifest.typo_twins[0].repo_a, "ai-contexers");
        assert_eq!(manifest.typo_twins[0].repo_b, "ai-contexters");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn plan_merges_case_sensitive_index_split() {
        let base = fixture_base("plan-split");
        write_index(
            &base,
            &[("VetCoders/CodeScribe", 40), ("vetcoders/codescribe", 2)],
        );

        let manifest = plan_identity_migration_at(&base).unwrap();
        assert_eq!(manifest.index_key_renames.len(), 1);
        assert!(manifest.index_key_renames[0].merge_into_existing);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn dry_run_leaves_store_and_index_untouched() {
        let base = fixture_base("dry-run");
        write_index(&base, &[("VetCoders/CodeScribe", 42)]);
        seed_store(&base);
        let index_before = fs::read_to_string(base.join("index.json")).unwrap();
        let store_before = snapshot(&base.join("store"));

        let outcome = run_identity_migration_at(&base, false).unwrap();

        assert!(!outcome.applied);
        assert_eq!(outcome.manifest.mode, "dry-run");
        assert!(outcome.manifest_path.is_file());
        assert!(outcome.report_path.is_file());
        assert_eq!(
            fs::read_to_string(base.join("index.json")).unwrap(),
            index_before,
            "dry-run must not touch index.json"
        );
        assert_eq!(
            snapshot(&base.join("store")),
            store_before,
            "dry-run must not touch store/"
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn apply_normalizes_dirs_and_index_and_annotates_cards() {
        let base = fixture_base("apply");
        write_index(
            &base,
            &[("VetCoders/CodeScribe", 40), ("vetcoders/codescribe", 2)],
        );
        seed_store(&base);
        let card_path = base.join("store/canonical-projection-v1/cards/card3_76b8e606.json");
        let card_before = fs::read_to_string(&card_path).unwrap();

        let outcome = run_identity_migration_at(&base, true).unwrap();
        assert!(outcome.applied);
        assert_eq!(outcome.manifest.mode, "apply");

        // Directory names are physically lowercase now (listing-level check
        // works on both case-sensitive and case-insensitive filesystems).
        let store_names = list_dir_names(&base.join("store")).unwrap();
        assert!(
            store_names.contains(&"vetcoders".to_string()),
            "{store_names:?}"
        );
        assert!(
            !store_names.contains(&"VetCoders".to_string()),
            "{store_names:?}"
        );
        let repo_names = list_dir_names(&base.join("store/vetcoders")).unwrap();
        assert!(
            repo_names.contains(&"codescribe".to_string()),
            "{repo_names:?}"
        );
        assert!(
            !repo_names.contains(&"CodeScribe".to_string()),
            "{repo_names:?}"
        );
        assert!(
            base.join("store/vetcoders/codescribe/2026_0717/context/claude/chunk1.md")
                .is_file(),
            "payload must survive the rename"
        );

        // Index: one merged canonical key with summed totals.
        let index = super::super::super::load_index_at(&base).unwrap();
        assert!(index.projects.contains_key("vetcoders/codescribe"));
        assert!(!index.projects.contains_key("VetCoders/CodeScribe"));
        let merged = &index.projects["vetcoders/codescribe"].agents["claude"];
        assert_eq!(merged.total_entries, 42);

        // ANNOTATE policy: card payload byte-identical, alias recorded.
        assert_eq!(fs::read_to_string(&card_path).unwrap(), card_before);
        assert_eq!(outcome.manifest.card_aliases.len(), 2);

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn edit_distance_gate_is_tight() {
        assert!(edit_distance_at_most_one("ai-contexers", "ai-contexters"));
        assert!(edit_distance_at_most_one("vista", "vistas"));
        assert!(!edit_distance_at_most_one("vista", "vista"));
        assert!(!edit_distance_at_most_one("vista", "vista-portal"));
        assert!(!edit_distance_at_most_one("aicx", "loctree"));
    }

    // merge_move's logic is casing-independent, so exercising it on two
    // differently-named directories covers the case-sensitive split branch
    // on every filesystem — an actual `A/` vs `a/` pair cannot coexist on
    // APFS, which is why the dir-level branch had no coverage before.
    #[test]
    fn merge_move_merges_recursively_and_records_conflicts() {
        let base = fixture_base("merge-move");
        let from = base.join("VetCoders-split");
        let to = base.join("vetcoders");
        fs::create_dir_all(from.join("nested")).unwrap();
        fs::create_dir_all(&to).unwrap();
        fs::write(from.join("unique.md"), "moved").unwrap();
        fs::write(from.join("clash.md"), "source version").unwrap();
        fs::write(from.join("nested").join("deep.md"), "moved too").unwrap();
        fs::write(to.join("clash.md"), "target version").unwrap();

        let mut conflicts = Vec::new();
        merge_move(&from, &to, &mut conflicts).unwrap();

        assert_eq!(fs::read_to_string(to.join("unique.md")).unwrap(), "moved");
        assert_eq!(
            fs::read_to_string(to.join("nested").join("deep.md")).unwrap(),
            "moved too"
        );
        // Conflict path: target wins in place, source copy is kept, one
        // conflict line is recorded — doctor never overwrites store data.
        assert_eq!(
            fs::read_to_string(to.join("clash.md")).unwrap(),
            "target version"
        );
        assert_eq!(
            fs::read_to_string(from.join("clash.md")).unwrap(),
            "source version"
        );
        assert_eq!(
            conflicts
                .iter()
                .filter(|c| c.contains("clash.md") && c.contains("kept both"))
                .count(),
            1
        );
        // Emptied nested source dir is removed; the non-empty root stays
        // and that fact is surfaced as a conflict entry.
        assert!(!from.join("nested").exists());
        assert!(from.is_dir());
        assert!(
            conflicts
                .iter()
                .any(|c| c.contains("source dir left in place"))
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn merge_move_removes_fully_emptied_source_dir() {
        let base = fixture_base("merge-move-clean");
        let from = base.join("VetCoders-split");
        let to = base.join("vetcoders");
        fs::create_dir_all(&from).unwrap();
        fs::write(from.join("chunk.md"), "payload").unwrap();

        let mut conflicts = Vec::new();
        merge_move(&from, &to, &mut conflicts).unwrap();

        assert_eq!(fs::read_to_string(to.join("chunk.md")).unwrap(), "payload");
        assert!(!from.exists(), "emptied source dir must be removed");
        assert!(conflicts.is_empty(), "clean merge records no conflicts");

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_precondition_hash_mismatch() {
        let base = fixture_base("precondition-mismatch");
        write_index(&base, &[("VetCoders/CodeScribe", 42)]);
        seed_store(&base);

        // 1. Dry run / planning phase
        let outcome = run_identity_migration_at(&base, false).unwrap();
        assert_eq!(outcome.manifest.steps.len(), 5); // 2 dir renames, 1 index key rename, 2 card aliases

        // 2. Change source: write a new file to cased dir
        fs::write(
            base.join("store/VetCoders/CodeScribe/2026_0717/context/claude/chunk2.md"),
            "changed source data",
        )
        .unwrap();

        // 3. Try to apply using the manifest.
        // It should load the manifest and fail because of the precondition source hash mismatch!
        let res = run_identity_migration_at(&base, true);
        assert!(res.is_err());

        // Let's verify manifest mode becomes apply-failed
        let raw = fs::read_to_string(outcome.manifest_path).unwrap();
        let manifest: IdentityMigrationManifest = serde_json::from_str(&raw).unwrap();
        assert_eq!(manifest.mode, "apply-failed");
        assert!(
            manifest
                .steps
                .iter()
                .any(|s| s.result.starts_with("failed:"))
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_ownerless_retention() {
        let base = fixture_base("ownerless");
        write_index(&base, &[("local", 42)]);
        let store = base.join("store");
        fs::create_dir_all(store.join("local/2026_0717/context/claude")).unwrap();
        fs::write(
            store.join("local/2026_0717/context/claude/chunk1.md"),
            "payload",
        )
        .unwrap();

        let outcome = run_identity_migration_at(&base, true).unwrap();
        // Since local is lowercase single-segment, it should have no renames and remain untouched.
        assert!(outcome.manifest.dir_renames.is_empty());
        assert!(outcome.manifest.index_key_renames.is_empty());
        assert!(
            store
                .join("local/2026_0717/context/claude/chunk1.md")
                .is_file()
        );

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_deprecated_checkout_quarantine() {
        let base = fixture_base("deprecated-quarantine");
        write_index(&base, &[("VetCoders/CodeScribe-deprecated", 42)]);
        let store = base.join("store");
        fs::create_dir_all(store.join("VetCoders/CodeScribe-deprecated/2026_0717/context/claude"))
            .unwrap();
        fs::write(
            store.join("VetCoders/CodeScribe-deprecated/2026_0717/context/claude/chunk1.md"),
            "deprecated payload",
        )
        .unwrap();

        let outcome = run_identity_migration_at(&base, true).unwrap();
        // Check that it planned as quarantine_deprecated
        let step = outcome
            .manifest
            .steps
            .iter()
            .find(|s| s.operation == "quarantine_deprecated")
            .unwrap();
        assert_eq!(step.result, "success");
        assert!(step.target_identity.starts_with("quarantine/"));

        // Verify it was moved to base quarantine folder
        let quarantine_path = base.join(&step.target_identity);
        assert!(
            quarantine_path
                .join("2026_0717/context/claude/chunk1.md")
                .is_file()
        );
        // Verify source dir is gone
        assert!(!store.join("VetCoders/CodeScribe-deprecated").exists());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_interrupted_apply_and_resume() {
        let base = fixture_base("resume");
        write_index(&base, &[("VetCoders/CodeScribe", 42)]);
        seed_store(&base);

        // 1. Dry run to plan
        let outcome = run_identity_migration_at(&base, false).unwrap();

        // 2. Mark the index key rename step as success in manifest file manually to simulate partial success
        let mut manifest = outcome.manifest.clone();
        for step in &mut manifest.steps {
            if step.operation == "rename_index_key" {
                step.result = "success".to_string();
            }
        }
        let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
        fs::write(&outcome.manifest_path, manifest_json).unwrap();

        // 3. Apply: should resume and only run the directory renames, skipping the index key rename
        let apply_outcome = run_identity_migration_at(&base, true).unwrap();
        assert_eq!(apply_outcome.manifest.mode, "apply");

        // The cased project index shouldn't be deleted since it was simulated success, but index key renames
        // loop is skipped so index is actually not mutated (which is fine since we simulated success on index).
        assert!(base.join("store/vetcoders/codescribe").is_dir());

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_rollback_behavior() {
        let base = fixture_base("rollback");
        write_index(&base, &[("VetCoders/CodeScribe", 42)]);
        seed_store(&base);

        // Apply migration
        let outcome = run_identity_migration_at(&base, true).unwrap();
        assert_eq!(outcome.manifest.mode, "apply");

        let store_names = list_dir_names(&base.join("store")).unwrap();
        assert!(store_names.contains(&"vetcoders".to_string()));
        assert!(!store_names.contains(&"VetCoders".to_string()));

        let repo_names = list_dir_names(&base.join("store/vetcoders")).unwrap();
        assert!(repo_names.contains(&"codescribe".to_string()));
        assert!(!repo_names.contains(&"CodeScribe".to_string()));

        let index = crate::store::load_index_at(&base).unwrap();
        assert!(index.projects.contains_key("vetcoders/codescribe"));
        assert!(!index.projects.contains_key("VetCoders/CodeScribe"));

        // Rollback
        rollback_identity_migration_at(&base).unwrap();

        // Check original state is restored
        let store_names2 = list_dir_names(&base.join("store")).unwrap();
        assert!(store_names2.contains(&"VetCoders".to_string()));
        assert!(!store_names2.contains(&"vetcoders".to_string()));

        let repo_names2 = list_dir_names(&base.join("store/VetCoders")).unwrap();
        assert!(repo_names2.contains(&"CodeScribe".to_string()));
        assert!(!repo_names2.contains(&"codescribe".to_string()));

        let index = crate::store::load_index_at(&base).unwrap();
        assert!(!index.projects.contains_key("vetcoders/codescribe"));
        assert!(index.projects.contains_key("VetCoders/CodeScribe"));

        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn test_dry_run_leaves_recursive_hash_unchanged() {
        let base = fixture_base("dry-run-hash");
        write_index(&base, &[("VetCoders/CodeScribe", 42)]);
        seed_store(&base);

        let hash_before = compute_store_recursive_hash(&base.join("store")).unwrap();

        run_identity_migration_at(&base, false).unwrap();

        let hash_after = compute_store_recursive_hash(&base.join("store")).unwrap();
        assert_eq!(hash_before, hash_after, "dry-run must not mutate any file");

        let _ = fs::remove_dir_all(&base);
    }
}
