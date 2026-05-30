use anyhow::{Context, Result};
use globset::{Glob, GlobMatcher};
use std::path::{Path, PathBuf};

use crate::sanitize;

pub const AICX_IGNORE_FILENAME: &str = ".aicxignore";

#[derive(Debug, Clone)]
struct IgnoreRule {
    negate: bool,
    matcher: GlobMatcher,
}

#[derive(Debug, Clone, Default)]
pub struct StoreIgnoreMatcher {
    base: PathBuf,
    rules: Vec<IgnoreRule>,
}

impl StoreIgnoreMatcher {
    pub(crate) fn empty_at(base: &Path) -> Self {
        Self {
            base: base.to_path_buf(),
            rules: Vec::new(),
        }
    }

    fn load(base: &Path) -> Result<Self> {
        let path = base.join(AICX_IGNORE_FILENAME);
        if !path.exists() {
            return Ok(Self::empty_at(base));
        }

        let raw = sanitize::read_to_string_validated(&path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        let mut rules = Vec::new();

        for (line_no, line) in raw.lines().enumerate() {
            let trimmed = line.trim();
            if trimmed.is_empty() || trimmed.starts_with('#') {
                continue;
            }

            let negate = trimmed.starts_with('!');
            let pattern = trimmed.trim_start_matches('!').trim();
            if pattern.is_empty() {
                continue;
            }

            let normalized = normalize_aicx_ignore_pattern(pattern);
            let matcher = Glob::new(&normalized)
                .with_context(|| {
                    format!(
                        "Invalid {} pattern at line {}: {}",
                        path.display(),
                        line_no + 1,
                        trimmed
                    )
                })?
                .compile_matcher();

            rules.push(IgnoreRule { negate, matcher });
        }

        Ok(Self {
            base: base.to_path_buf(),
            rules,
        })
    }

    pub fn is_ignored(&self, path: &Path) -> bool {
        if self.rules.is_empty() {
            return false;
        }

        let Ok(relative) = path.strip_prefix(&self.base) else {
            return false;
        };
        let relative = normalize_relative_store_path(relative);
        if relative.is_empty() {
            return false;
        }

        let mut ignored = false;
        for rule in &self.rules {
            if rule.matcher.is_match(&relative) {
                ignored = !rule.negate;
            }
        }
        ignored
    }
}

fn normalize_relative_store_path(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

fn normalize_aicx_ignore_pattern(pattern: &str) -> String {
    let mut normalized = pattern
        .trim()
        .trim_start_matches("./")
        .trim_start_matches('/')
        .replace('\\', "/");

    while normalized.contains("//") {
        normalized = normalized.replace("//", "/");
    }

    if normalized.ends_with('/') {
        normalized.push_str("**");
    }

    normalized
}

pub fn load_ignore_matcher_at(base: &Path) -> Result<StoreIgnoreMatcher> {
    StoreIgnoreMatcher::load(base)
}

pub fn filter_ignored_paths_at<P>(base: &Path, paths: &[P]) -> Result<(Vec<PathBuf>, usize)>
where
    P: AsRef<Path>,
{
    let matcher = load_ignore_matcher_at(base)?;
    if matcher.rules.is_empty() {
        return Ok((
            paths
                .iter()
                .map(|path| path.as_ref().to_path_buf())
                .collect(),
            0,
        ));
    }

    let mut kept = Vec::with_capacity(paths.len());
    let mut ignored = 0usize;

    for path in paths {
        let path = path.as_ref();
        if matcher.is_ignored(path) {
            ignored += 1;
        } else {
            kept.push(path.to_path_buf());
        }
    }

    Ok((kept, ignored))
}
