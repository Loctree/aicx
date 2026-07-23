//! Canonical AICX home resolution and initialization.
//!
//! This module owns only the runtime root. Legacy corpus layout and recovery
//! live under [`crate::legacy_archive`].

use anyhow::{Context, Result};
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

const CONFIG_FILENAME: &str = "config.toml";
const DEFAULT_AICX_DIRNAME: &str = ".aicx";

/// Resolve the canonical AICX home without creating it.
///
/// Precedence:
/// 1. non-empty `$AICX_HOME`;
/// 2. `[storage].home` in `$HOME/.aicx/config.toml`;
/// 3. `$HOME/.aicx`.
pub fn resolve() -> Result<PathBuf> {
    let home = crate::os_user_home().context("No home directory")?;
    resolve_from(std::env::var_os("AICX_HOME"), &home)
}

pub(crate) fn resolve_from(env_value: Option<OsString>, home_dir: &Path) -> Result<PathBuf> {
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

/// Return an explicit AICX runtime root without reading env or touching disk.
pub fn root_for(home: &Path) -> PathBuf {
    home.to_path_buf()
}

/// Resolve and create the AICX runtime root.
pub fn ensure() -> Result<PathBuf> {
    let dir = root_for(&resolve()?);
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create AICX home: {}", dir.display()))?;
    Ok(dir)
}
