use chrono::{DateTime, Utc};
use regex::Regex;
use serde_json::Value;
use std::path::Path;
use std::sync::OnceLock;
use std::time::SystemTime;

use crate::sanitize;

pub(super) fn infer_agent(path: &Path, content: &str) -> String {
    let haystack = format!(
        "{} {}",
        path.display(),
        content.lines().take(20).collect::<String>()
    )
    .to_ascii_lowercase();
    for agent in [
        "claude",
        "codex",
        "gemini",
        "junie",
        "codescribe",
        "operator-md",
    ] {
        if haystack.contains(agent) {
            return agent.to_string();
        }
    }
    "unknown".to_string()
}

pub(super) fn infer_frame_kind(path: &Path, content: &str) -> Option<String> {
    if let Some(value) = content.lines().find_map(|line| {
        line.strip_prefix("frame_kind:")
            .map(str::trim)
            .filter(|value| !value.is_empty())
    }) {
        return Some(value.to_string());
    }
    sanitize::read_to_string_validated(&path.with_extension("meta.json"))
        .ok()
        .and_then(|raw| serde_json::from_str::<Value>(&raw).ok())
        .and_then(|value| {
            value
                .get("frame_kind")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
}

pub(super) fn infer_path_date(path: &Path) -> Option<String> {
    static DATE_RE: OnceLock<Regex> = OnceLock::new();
    let re = DATE_RE.get_or_init(|| Regex::new(r"(20\d{2})[-_]?([01]\d)[-_]?([0-3]\d)").unwrap());
    let text = path.display().to_string();
    re.captures(&text)
        .map(|captures| format!("{}-{}-{}", &captures[1], &captures[2], &captures[3]))
}

pub(super) fn infer_session_id(path: &Path) -> String {
    let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
        return "unknown".to_string();
    };
    let parts: Vec<&str> = stem.split('_').collect();
    if parts.len() >= 5 {
        parts[3].to_string()
    } else {
        parts.get(2).copied().unwrap_or("unknown").to_string()
    }
}

pub(super) fn infer_project(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .ok()
        .and_then(|relative| {
            let mut components = relative.components().filter_map(|c| c.as_os_str().to_str());
            match components.next() {
                Some("store") => Some(format!(
                    "{}/{}",
                    components.next().unwrap_or("unknown"),
                    components.next().unwrap_or("unknown")
                )),
                Some(other) => Some(other.to_string()),
                None => None,
            }
        })
        .unwrap_or_else(|| "unknown".to_string())
}

pub(super) fn system_timestamp(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).to_rfc3339()
}

pub(super) fn system_date(time: SystemTime) -> String {
    DateTime::<Utc>::from(time).format("%Y-%m-%d").to_string()
}

pub(super) fn derived_markdown_hash(content: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in content.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("fnv1a64:{hash:016x}")
}
