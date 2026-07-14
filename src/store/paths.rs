use anyhow::{Context, Result};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use crate::validation::is_valid_repo_project_slug;

pub const NON_REPOSITORY_CONTEXTS: &str = "non-repository-contexts";
pub const CANONICAL_STORE_DIRNAME: &str = "store";
pub const CONTEXT_CORPUS_DIRNAME: &str = "context-corpus";
pub const LOCT_CONTEXT_PACK_FAMILY: &str = "loct-context-pack";
pub const CONTEXT_CORPUS_SCHEMA_VERSION: &str = "context_corpus.v1";
pub const LEGACY_SALVAGE_DIRNAME: &str = "legacy-store";

const MIGRATION_DIRNAME: &str = "migration";
const MIGRATION_MANIFEST_FILENAME: &str = "manifest.json";
const MIGRATION_REPORT_FILENAME: &str = "report.md";
const CONFIG_FILENAME: &str = "config.toml";
const DEFAULT_AICX_DIRNAME: &str = ".aicx";

pub(crate) fn canonical_project_slug(project: &str) -> String {
    project
        .split('/')
        .map(canonical_bucket_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// Trim whitespace from a bucket segment. Case is preserved; dot-prefix and
/// underscore-prefix bucket names (`.aicx`, `.codescribe`, `.github`,
/// `_internal`, `.scripts`) are accepted as-is by `is_valid_repo_bucket_name`
/// (relaxed 2026-05-12 from prior lowercase-only + leading-char-restricted
/// schema). Mid-segment garbage from extractor bugs (newlines, shell
/// metacharacters, leading `$`/`{`/`<`) is intentionally not sanitized so the
/// validator surfaces it instead of silently normalizing junk into a
/// filesystem path.
fn canonical_bucket_segment(segment: &str) -> String {
    segment.trim().to_string()
}

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

/// Resolve the canonical AICX home directory.
///
/// Precedence:
/// 1. `$AICX_HOME` when set and non-empty.
/// 2. `[storage].home` in the bootstrap config at `$HOME/.aicx/config.toml`.
/// 3. `$HOME/.aicx`.
///
/// The bootstrap config is fixed at `$HOME/.aicx/config.toml` on purpose:
/// without a stable first read location, a config-driven home would be
/// circular. Use `AICX_HOME` for one-shot overrides.
/// Pure: no filesystem side effects, no directory creation. Use
/// [`store_base_dir`] for the side-effecting public variant.
pub fn resolve_aicx_home() -> Result<PathBuf> {
    let home = crate::os_user_home().context("No home directory")?;
    resolve_aicx_home_from(std::env::var_os("AICX_HOME"), &home)
}

pub(crate) fn resolve_aicx_home_from(
    env_value: Option<OsString>,
    home_dir: &Path,
) -> Result<PathBuf> {
    if let Some(value) = env_value.filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(value));
    }
    let default_home = home_dir.join(DEFAULT_AICX_DIRNAME);
    if let Some(configured) = configured_home_from_bootstrap_config(home_dir, &default_home)? {
        return Ok(configured);
    }
    Ok(default_home)
}

fn configured_home_from_bootstrap_config(
    home_dir: &Path,
    default_home: &Path,
) -> Result<Option<PathBuf>> {
    let config_path = default_home.join(CONFIG_FILENAME);
    if !config_path.exists() {
        return Ok(None);
    }
    // Size-capped, traversal-checked, allowlist-enforced read (semgrep:
    // tainted-path). The bootstrap config sits at a fixed location under
    // `$HOME/.aicx/`, but it is still operator-writable input.
    let raw = crate::sanitize::read_to_string_validated(&config_path)
        .with_context(|| format!("failed to read bootstrap config {}", config_path.display()))?;
    let parsed: toml::Value = toml::from_str(&raw)
        .with_context(|| format!("failed to parse bootstrap config {}", config_path.display()))?;
    let Some(value) = parsed
        .get("storage")
        .and_then(|storage| storage.get("home"))
        .and_then(|home| home.as_str())
    else {
        return Ok(None);
    };
    // Operator-writable input: reject control characters and `..`
    // traversal before trusting the value as the AICX home, and accept
    // only absolute paths (or `~`/`~/...` expanded against $HOME).
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if trimmed.chars().any(char::is_control) {
        anyhow::bail!(
            "invalid [storage].home in {}: control characters are not allowed",
            config_path.display()
        );
    }
    let path = if trimmed == "~" {
        home_dir.to_path_buf()
    } else if let Some(rest) = trimmed.strip_prefix("~/") {
        home_dir.join(rest)
    } else {
        PathBuf::from(trimmed)
    };
    if path
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        anyhow::bail!(
            "invalid [storage].home in {}: parent-directory traversal (`..`) is not allowed, got {:?}",
            config_path.display(),
            value
        );
    }
    if !path.is_absolute() {
        anyhow::bail!(
            "invalid [storage].home in {}: expected an absolute path or ~/..., got {:?}",
            config_path.display(),
            value
        );
    }
    Ok(Some(path))
}

/// Pure: builds the AICX base directory under an explicit `home`.
///
/// No env reads, no filesystem creation. Use in tests to assert path-shape
/// invariants without depending on `$AICX_HOME` or `$HOME`.
pub fn store_base_dir_for(home: &Path) -> PathBuf {
    home.to_path_buf()
}

/// Returns the AICX base directory: `$AICX_HOME` or `$HOME/.aicx/`.
///
/// Creates the directory if it does not exist.
pub fn store_base_dir() -> Result<PathBuf> {
    let dir = store_base_dir_for(&resolve_aicx_home()?);
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create store dir: {}", dir.display()))?;
    Ok(dir)
}

/// Returns the canonical repo-centric store root: `$AICX_HOME/store/`.
pub fn canonical_store_dir() -> Result<PathBuf> {
    let dir = store_base_dir()?.join(CANONICAL_STORE_DIRNAME);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the immutable context-corpus root: `$AICX_HOME/context-corpus/`.
pub fn context_corpus_root_dir() -> Result<PathBuf> {
    let dir = context_corpus_root_dir_for(&store_base_dir()?);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Pure: builds the immutable context-corpus root under an explicit `home`.
///
/// No env reads, no filesystem creation. Used by tests that must exercise
/// context-corpus ingest behavior without racing on process-global env vars.
pub(crate) fn context_corpus_root_dir_for(home: &Path) -> PathBuf {
    store_base_dir_for(home).join(CONTEXT_CORPUS_DIRNAME)
}

pub fn aicx_context_corpus_dir(org: &str, repo: &str, date: &str, batch: &str) -> Result<PathBuf> {
    aicx_context_corpus_dir_for(&store_base_dir()?, org, repo, date, batch)
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
    let dir = store_base_dir()?.join(NON_REPOSITORY_CONTEXTS);
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

/// Returns the project directory: `$AICX_HOME/store/<project>/`.
pub fn project_dir(project: &str) -> Result<PathBuf> {
    let dir = validated_store_project_dir(&canonical_store_dir()?, project)?;
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Pure: builds the chunks directory under an explicit `home`.
///
/// No env reads, no filesystem creation. Used in tests to verify chunks-dir
/// shape without depending on `$AICX_HOME`.
pub fn chunks_dir_for(home: &Path) -> PathBuf {
    store_base_dir_for(home).join("chunks")
}

/// Returns the chunks directory: `<base>/chunks/`.
pub fn chunks_dir() -> Result<PathBuf> {
    let dir = chunks_dir_for(&store_base_dir()?);
    fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Full path for a specific context markdown file.
///
/// Layout: `$AICX_HOME/store/<project>/<date>/<time>_<agent>-context.md`
pub fn get_context_path(project: &str, agent: &str, date: &str, time: &str) -> Result<PathBuf> {
    let dir = validated_store_project_dir(&canonical_store_dir()?, project)?.join(date);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}_{}-context.md", time, agent)))
}

/// Full path for a specific context JSON file.
///
/// Layout: `$AICX_HOME/store/<project>/<date>/<time>_<agent>-context.json`
pub fn get_context_json_path(
    project: &str,
    agent: &str,
    date: &str,
    time: &str,
) -> Result<PathBuf> {
    let dir = validated_store_project_dir(&canonical_store_dir()?, project)?.join(date);
    fs::create_dir_all(&dir)?;
    Ok(dir.join(format!("{}_{}-context.json", time, agent)))
}

pub(super) fn validated_store_project_dir(root: &Path, project: &str) -> Result<PathBuf> {
    let canonical = canonical_project_slug(project);
    if !is_valid_repo_project_slug(&canonical) {
        anyhow::bail!(
            "invalid canonical store project bucket {:?}; expected lowercase <bucket> or <org>/<repo> with [a-z0-9][a-z0-9._-]{{0,99}} segments",
            project
        );
    }
    Ok(root.join(canonical))
}
