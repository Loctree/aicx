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
}

#[derive(Debug)]
pub struct IdentityMigrationOutcome {
    pub manifest: IdentityMigrationManifest,
    pub manifest_path: PathBuf,
    pub report_path: PathBuf,
    pub applied: bool,
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
    for org in list_dir_names(&store_root)? {
        if RESERVED_STORE_DIRS.contains(&org.as_str()) {
            continue;
        }
        let org_canon = identity_canon(&org);
        for repo in list_dir_names(&store_root.join(&org))? {
            if is_compact_date_dir(&repo) {
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

    Ok(IdentityMigrationManifest {
        schema_version: IDENTITY_MIGRATION_SCHEMA_VERSION,
        generated_at: Utc::now().to_rfc3339(),
        mode: "dry-run".to_string(),
        index_key_renames,
        dir_renames,
        card_aliases,
        typo_twins,
        conflicts: Vec::new(),
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
    let same_fold = from
        .file_name()
        .zip(to.file_name())
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b.to_string_lossy().as_ref()));

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

/// Execute the planned renames: store directories first (repo level, then
/// org level, exactly in manifest order), then the `index.json` keys.
pub fn execute_identity_migration_at(
    base: &Path,
    manifest: &mut IdentityMigrationManifest,
) -> Result<()> {
    let store_root = base.join(super::super::CANONICAL_STORE_DIRNAME);
    let mut conflicts = std::mem::take(&mut manifest.conflicts);

    for rename in &manifest.dir_renames {
        let from = store_root.join(&rename.from);
        let to = store_root.join(&rename.to);
        rename_dir_safely(&from, &to, &mut conflicts)?;
    }

    if !manifest.index_key_renames.is_empty() {
        let mut index = super::super::load_index_at(base)?;
        for rename in &manifest.index_key_renames {
            let Some(source) = index.projects.remove(&rename.from) else {
                conflicts.push(format!(
                    "index key rename skipped, key missing: {}",
                    rename.from
                ));
                continue;
            };
            let target = index.projects.entry(rename.to.clone()).or_default();
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
        }
        index.last_updated = Utc::now();
        super::super::save_index_at(base, &index)?;
    }

    manifest.conflicts = conflicts;
    manifest.mode = "apply".to_string();
    Ok(())
}

fn render_identity_report(manifest: &IdentityMigrationManifest) -> String {
    let mut out = String::new();
    out.push_str("# Project-identity migration report\n\n");
    out.push_str(&format!(
        "- mode: **{}**\n- generated_at: {}\n- schema_version: {}\n\n",
        manifest.mode, manifest.generated_at, manifest.schema_version
    ));

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

    if manifest.mode == "dry-run" {
        out.push_str(
            "\n## Operator buttons\n\n- `aicx doctor --migrate-identities --apply` — execute the renames above\n- typo-twin merges: separate decision, not part of `--apply`\n\nNote: `--apply` re-plans against the store state at execution time; if the store changes between this dry-run and the apply, the executed plan may differ from this report.\n",
        );
    }
    out
}

/// Plan (always) + execute (only with `apply`) + persist manifest/report
/// under `<base>/migration/`. Dry-run never touches `index.json` or any
/// `store/` path.
pub fn run_identity_migration_at(base: &Path, apply: bool) -> Result<IdentityMigrationOutcome> {
    let mut manifest = plan_identity_migration_at(base)?;
    if apply {
        execute_identity_migration_at(base, &mut manifest)?;
    }

    let manifest_path = identity_migration_manifest_path(base);
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
}
