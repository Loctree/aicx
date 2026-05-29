#![allow(unused_imports)]
use super::*;

pub(crate) const MAX_LINE_BYTES: usize = 8 * 1024 * 1024;

// Note: line reading goes through `aicx_parser::sanitize::read_line_capped`,
// which walks back past UTF-8 continuation bytes when an oversized line is
// truncated. The previously-private `read_line_limited` here truncated at
// the raw byte boundary and would surface `InvalidData` on any input where
// `max_bytes` landed inside a multi-byte codepoint — that follow-up was
// half-done (one call site already used the capped helper; the rest were
// still on the legacy truncator).

pub(crate) fn observe_oversized_line(
    count: &mut usize,
    samples: &mut Vec<String>,
    line_number: usize,
) {
    *count += 1;
    if samples.len() < 5 {
        samples.push(format!("line {line_number}"));
    }
}

pub(crate) fn parse_rfc3339_or_naive_utc(raw: &str) -> Result<DateTime<Utc>, chrono::ParseError> {
    if let Ok(timestamp) = DateTime::parse_from_rfc3339(raw) {
        return Ok(timestamp.with_timezone(&Utc));
    }

    NaiveDateTime::parse_from_str(raw, "%Y-%m-%dT%H:%M:%S%.f")
        .or_else(|_| NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S%.f"))
        .map(|timestamp| DateTime::<Utc>::from_naive_utc_and_offset(timestamp, Utc))
}

pub(crate) fn short_path_hash(path: &Path) -> String {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in path.to_string_lossy().as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}").chars().take(12).collect::<String>()
}

pub(crate) fn walk_jsonl_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk_jsonl_files(&path));
            } else if path.extension().is_some_and(|e| e == "jsonl") {
                files.push(path);
            }
        }
    }
    files
}

pub(crate) fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                files.extend(walk_files(&path));
            } else if path.is_file() {
                files.push(path);
            }
        }
    }
    files
}
