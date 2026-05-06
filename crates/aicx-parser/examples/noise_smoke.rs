//! Empirical smoke validator for `aicx_parser::noise::filter_noise_lines`.
//!
//! Reads stored chunk markdown files (or any text files), applies the
//! noise filter, and reports byte and line drop percentages so we can
//! verify the regex set against real session data.
//!
//! Usage:
//!   cargo run -p aicx-parser --example noise_smoke -- <file>...
//!
//! Default sample (no args) walks `~/.aicx/store/Loctree/aicx/` for `.md`
//! files and takes the first 50 it finds.

use aicx_parser::noise::filter_noise_lines;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let paths: Vec<PathBuf> = if args.is_empty() {
        let root = default_sample_root();
        let collected = collect_md_files(&root, 50);
        if collected.is_empty() {
            eprintln!("no .md files found under {}", root.display());
            std::process::exit(2);
        }
        eprintln!("sampling {} files from {}", collected.len(), root.display());
        collected
    } else {
        args.into_iter().map(PathBuf::from).collect()
    };

    let mut total_bytes_in = 0u64;
    let mut total_bytes_out = 0u64;
    let mut total_lines_in = 0u64;
    let mut total_lines_out = 0u64;
    let mut files_processed = 0u64;
    let mut per_file: Vec<(PathBuf, u64, u64, u64, u64)> = Vec::new();

    for path in &paths {
        let Ok(content) = fs::read_to_string(path) else {
            eprintln!("skip (read error): {}", path.display());
            continue;
        };
        let lines_in = content.lines().count() as u64;
        let bytes_in = content.len() as u64;
        let filtered = filter_noise_lines(&content);
        let bytes_out = filtered.len() as u64;
        let lines_out = filtered.lines().count() as u64;

        total_bytes_in += bytes_in;
        total_bytes_out += bytes_out;
        total_lines_in += lines_in;
        total_lines_out += lines_out;
        files_processed += 1;

        per_file.push((path.clone(), bytes_in, bytes_out, lines_in, lines_out));
    }

    if files_processed == 0 {
        eprintln!("no files processed");
        std::process::exit(2);
    }

    // Per-file detail sorted by drop percent descending — surfaces the
    // dirtiest files first so we can spot bad cases.
    per_file.sort_by(|a, b| {
        let da = drop_pct(a.1, a.2);
        let db = drop_pct(b.1, b.2);
        db.partial_cmp(&da).unwrap_or(std::cmp::Ordering::Equal)
    });

    println!(
        "{:<6} {:>10} {:>10} {:>7} {:>7} {:>7}  path",
        "rank", "bytes_in", "bytes_out", "drop%", "lines_i", "lines_o"
    );
    for (rank, (path, bi, bo, li, lo)) in per_file.iter().enumerate() {
        let pct = drop_pct(*bi, *bo);
        let display = path
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| path.display().to_string());
        println!(
            "{:<6} {:>10} {:>10} {:>6.1}% {:>7} {:>7}  {}",
            rank + 1,
            bi,
            bo,
            pct,
            li,
            lo,
            display
        );
    }

    println!();
    println!("=== aggregate ===");
    println!("files: {}", files_processed);
    println!(
        "bytes:  {} → {}  ({:.1}% drop)",
        total_bytes_in,
        total_bytes_out,
        drop_pct(total_bytes_in, total_bytes_out)
    );
    println!(
        "lines:  {} → {}  ({:.1}% drop)",
        total_lines_in,
        total_lines_out,
        drop_pct(total_lines_in, total_lines_out)
    );
    println!(
        "avg bytes/file: {} → {}",
        total_bytes_in / files_processed,
        total_bytes_out / files_processed
    );
}

fn drop_pct(input: u64, output: u64) -> f64 {
    if input == 0 {
        return 0.0;
    }
    100.0 * (1.0 - output as f64 / input as f64)
}

fn default_sample_root() -> PathBuf {
    let home = env::var("HOME").unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home).join(".aicx/store/Loctree/aicx")
}

fn collect_md_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if out.len() >= limit {
            break;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if out.len() >= limit {
                break;
            }
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "md") {
                out.push(path);
            }
        }
    }
    out
}
