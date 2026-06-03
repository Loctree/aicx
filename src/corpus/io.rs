use anyhow::{Context, Result};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::corpus::REPAIR_MANIFEST_DIR;
use crate::sanitize;

pub(super) fn markdown_files(root: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    for start in scan_start_dirs(root) {
        if start.exists() {
            collect_markdown_files(&start, &mut files)?;
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn scan_start_dirs(root: &Path) -> Vec<PathBuf> {
    let is_aicx_root = root
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == ".aicx");
    if is_aicx_root {
        let mut starts = vec![
            root.join("store"),
            root.join("non-repository-contexts"),
            root.join("chunks"),
        ];
        if root.extension().and_then(|s| s.to_str()) == Some("md") {
            starts.push(root.to_path_buf());
        }
        starts
    } else {
        vec![root.to_path_buf()]
    }
}

fn collect_markdown_files(dir: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let dir = sanitize::validate_read_path(dir)?;
    let metadata = fs::symlink_metadata(&dir)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        if dir.extension().and_then(|s| s.to_str()) == Some("md") {
            files.push(dir.to_path_buf());
        }
        return Ok(());
    }

    for entry in
        sanitize::read_dir_validated(&dir).with_context(|| format!("read dir {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if matches!(
            name,
            "target"
                | ".git"
                | REPAIR_MANIFEST_DIR
                | "lancedb"
                | "lance"
                | "steer"
                | "steer-index"
                | "bm25"
                | "indexes"
        ) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            collect_markdown_files(&path, files)?;
        } else if metadata.is_file() && path.extension().and_then(|s| s.to_str()) == Some("md") {
            files.push(path);
        }
    }
    Ok(())
}

pub(super) fn validate_optional_root(root: PathBuf) -> Result<PathBuf> {
    if root.exists() {
        sanitize::validate_read_path(&root)
    } else {
        Ok(root)
    }
}

pub(super) fn write_text_validated(path: &Path, content: &str) -> Result<()> {
    write_bytes_validated(path, content.as_bytes())
}

pub(super) fn write_bytes_validated(path: &Path, content: &[u8]) -> Result<()> {
    let mut file = sanitize::create_file_validated(path)?;
    file.write_all(content)?;
    Ok(())
}
