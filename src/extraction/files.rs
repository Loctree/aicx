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
