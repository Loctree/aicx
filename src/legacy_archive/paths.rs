use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

pub const NON_REPOSITORY_CONTEXTS: &str = "non-repository-contexts";
/// Virtual organization used to address legacy store buckets that have no
/// owner directory (`store/<repository>/...`). The sentinel is query-only:
/// reading an ownerless bucket never migrates or renames its on-disk path.
pub const OWNERLESS_PROJECT_ORGANIZATION: &str = "_";
pub const LEGACY_CARDS_DIRNAME: &str = "store";
pub const CANONICAL_STORE_DIRNAME: &str = LEGACY_CARDS_DIRNAME;
pub const CONTEXT_CORPUS_DIRNAME: &str = "context-corpus";
pub const LOCT_CONTEXT_PACK_FAMILY: &str = "loct-context-pack";
pub const CONTEXT_CORPUS_SCHEMA_VERSION: &str = "context_corpus.v1";
pub const LEGACY_SALVAGE_DIRNAME: &str = "legacy-store";

const MIGRATION_DIRNAME: &str = "migration";
const MIGRATION_MANIFEST_FILENAME: &str = "manifest.json";
const MIGRATION_REPORT_FILENAME: &str = "report.md";
const IDENTITY_MIGRATION_MANIFEST_FILENAME: &str = "identity-manifest.json";
const IDENTITY_MIGRATION_REPORT_FILENAME: &str = "identity-report.md";
pub use crate::aicx_home::resolve as resolve_aicx_home;
#[cfg(test)]
pub(crate) use crate::aicx_home::resolve_from as resolve_aicx_home_from;

fn canonical_path_segment(value: &str, label: &str) -> Result<String> {
    let cleaned = value.trim().to_ascii_lowercase();
    if cleaned.is_empty()
        || cleaned.contains('/')
        || cleaned.contains('\\')
        || cleaned.contains("..")
        || !cleaned
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' || ch == '.')
    {
        anyhow::bail!("invalid context corpus {label} segment: {value:?}");
    }
    Ok(cleaned)
}

/// Resolve the retired card archive path without creating it.
///
/// Recovery readers may inspect `$AICX_HOME/store/` when it already exists,
/// but no current path is allowed to recreate that directory.
pub fn legacy_cards_dir() -> Result<PathBuf> {
    Ok(legacy_cards_dir_for(&crate::aicx_home::resolve()?))
}

/// Pure form of [`legacy_cards_dir`] for explicit AICX homes.
pub fn legacy_cards_dir_for(home: &Path) -> PathBuf {
    crate::aicx_home::root_for(home).join(LEGACY_CARDS_DIRNAME)
}

/// Returns the immutable context-corpus root: `$AICX_HOME/context-corpus/`.
pub fn context_corpus_root_dir() -> Result<PathBuf> {
    let dir = context_corpus_root_dir_for(&crate::aicx_home::ensure()?);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Pure: builds the immutable context-corpus root under an explicit `home`.
///
/// No env reads, no filesystem creation. Used by tests that must exercise
/// context-corpus ingest behavior without racing on process-global env vars.
pub(crate) fn context_corpus_root_dir_for(home: &Path) -> PathBuf {
    crate::aicx_home::root_for(home).join(CONTEXT_CORPUS_DIRNAME)
}

pub fn aicx_context_corpus_dir(org: &str, repo: &str, date: &str, batch: &str) -> Result<PathBuf> {
    aicx_context_corpus_dir_for(&crate::aicx_home::ensure()?, org, repo, date, batch)
}

pub(crate) fn aicx_context_corpus_dir_for(
    home: &Path,
    org: &str,
    repo: &str,
    date: &str,
    batch: &str,
) -> Result<PathBuf> {
    let org = canonical_path_segment(org, "org")?;
    let repo = canonical_path_segment(repo, "repo")?;
    let date = super::compact_date(date);
    let batch = canonical_path_segment(batch, "batch")?;
    let dir = context_corpus_root_dir_for(home)
        .join(org)
        .join(repo)
        .join(date)
        .join(LOCT_CONTEXT_PACK_FAMILY)
        .join(batch);
    fs::create_dir_all(dir.join("raw"))?;
    fs::create_dir_all(dir.join("sidecars"))?;
    Ok(dir)
}

/// Returns the non-repository fallback root:
/// `$AICX_HOME/non-repository-contexts/`.
pub fn non_repository_contexts_dir() -> Result<PathBuf> {
    let dir = crate::aicx_home::ensure()?.join(NON_REPOSITORY_CONTEXTS);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the legacy input-store root used for truthful migration inventory.
pub fn legacy_store_base_dir() -> Result<PathBuf> {
    Ok(crate::os_user_home()
        .context("No home directory")?
        .join(".ai-contexters"))
}

pub(super) fn legacy_salvage_dir(base: &Path) -> PathBuf {
    base.join(LEGACY_SALVAGE_DIRNAME)
}

fn migration_dir(base: &Path) -> PathBuf {
    base.join(MIGRATION_DIRNAME)
}

pub(super) fn migration_manifest_path(base: &Path) -> PathBuf {
    migration_dir(base).join(MIGRATION_MANIFEST_FILENAME)
}

pub(super) fn migration_report_path(base: &Path) -> PathBuf {
    migration_dir(base).join(MIGRATION_REPORT_FILENAME)
}

pub(super) fn identity_migration_manifest_path(base: &Path) -> PathBuf {
    migration_dir(base).join(IDENTITY_MIGRATION_MANIFEST_FILENAME)
}

pub(super) fn identity_migration_report_path(base: &Path) -> PathBuf {
    migration_dir(base).join(IDENTITY_MIGRATION_REPORT_FILENAME)
}

/// Pure: builds the chunks directory under an explicit `home`.
///
/// No env reads, no filesystem creation. Used in tests to verify chunks-dir
/// shape without depending on `$AICX_HOME`.
pub fn chunks_dir_for(home: &Path) -> PathBuf {
    crate::aicx_home::root_for(home).join("chunks")
}
