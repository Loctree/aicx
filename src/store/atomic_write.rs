//! Atomic file write primitive.
//!
//! Writes content to a sibling tempfile, fsyncs the file, then renames into
//! place. A crash mid-write either leaves the destination unchanged or fully
//! written — never truncated. Parent directory fsync is best-effort.
//!
//! Two-phase variant (`stage_tempfile` + `commit_tempfile`) lets callers
//! coordinate ordered renames across multiple files (e.g. chunk `.md` plus
//! sidecar `.meta.json`).

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static TEMPFILE_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Atomically write `content` to `path`. Creates parent directories as needed.
pub fn atomic_write(path: &Path, content: &[u8]) -> io::Result<()> {
    let tmp = stage_tempfile(path, content)?;
    if let Err(err) = commit_tempfile(&tmp, path) {
        discard_tempfile(&tmp);
        return Err(err);
    }
    sync_parent_best_effort(path);
    Ok(())
}

/// Stage `content` in a sibling tempfile of `target`. Returns the tempfile
/// path. The caller MUST follow up with either `commit_tempfile` or
/// `discard_tempfile`; the staged file is otherwise leaked.
pub fn stage_tempfile(target: &Path, content: &[u8]) -> io::Result<PathBuf> {
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("atomic_write: path has no parent: {}", target.display()),
        )
    })?;
    if !parent.as_os_str().is_empty() {
        fs::create_dir_all(parent)?;
    }

    let file_name = target.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "atomic_write: missing or non-UTF8 filename: {}",
                target.display()
            ),
        )
    })?;

    // PID + nanos + process-local counter disambiguates concurrent writers; the
    // leading dot keeps the tempfile out of glob scans.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    let counter = TEMPFILE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let tmp_name = format!(
        ".{}.tmp.{}.{}.{}",
        file_name,
        std::process::id(),
        nanos,
        counter
    );
    let tmp = parent.join(tmp_name);

    let res = (|| -> io::Result<()> {
        let mut file = fs::File::create(&tmp)?; // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        file.write_all(content)?;
        file.flush()?;
        file.sync_all()
    })();

    if let Err(err) = res {
        discard_tempfile(&tmp);
        return Err(err);
    }
    Ok(tmp)
}

/// Atomically swap `tmp` into the destination at `target`.
pub fn commit_tempfile(tmp: &Path, target: &Path) -> io::Result<()> {
    fs::rename(tmp, target)
}

/// Best-effort tempfile removal. Errors are intentionally swallowed.
pub fn discard_tempfile(tmp: &Path) {
    let _ = fs::remove_file(tmp);
}

fn sync_parent_best_effort(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    if parent.as_os_str().is_empty() {
        return;
    }
    if let Ok(dir) = fs::File::open(parent) {
        let _ = dir.sync_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::env;
    use std::fs;
    use std::path::PathBuf;
    use std::thread;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_test_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let dir = env::temp_dir().join(format!(
            "aicx-atomic-write-{label}-{}-{nanos}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn test_atomic_write_creates_file() {
        let dir = unique_test_dir("creates");
        let path = dir.join("nested").join("hello.txt");
        atomic_write(&path, b"hello world").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"hello world");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_atomic_write_overwrites_existing() {
        let dir = unique_test_dir("overwrites");
        let path = dir.join("file.txt");
        fs::write(&path, b"original").unwrap();
        atomic_write(&path, b"replaced").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"replaced");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_atomic_write_tempfile_cleaned_up_on_error() {
        let dir = unique_test_dir("cleanup");
        // Parent is a regular file, not a directory — create_dir_all must fail
        // and atomic_write must not leave a tempfile sibling.
        let blocker = dir.join("blocker");
        fs::write(&blocker, b"sentinel").unwrap();
        let path = blocker.join("inner.txt");
        let res = atomic_write(&path, b"never lands");
        assert!(
            res.is_err(),
            "expected write under a file-as-parent to fail"
        );
        let entries: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("blocker")]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_atomic_write_handles_unicode_paths() {
        let dir = unique_test_dir("unicode");
        let path = dir.join("zażółć_gęślą_jaźń.md");
        atomic_write(&path, "łódź — naïve façade".as_bytes()).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "łódź — naïve façade");
        let stray: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| {
                let n = e.unwrap().file_name();
                let s = n.to_string_lossy().into_owned();
                if s.starts_with('.') && s.contains(".tmp.") {
                    Some(s)
                } else {
                    None
                }
            })
            .collect();
        assert!(stray.is_empty(), "stray tempfile: {:?}", stray);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_atomic_write_100_concurrent_writes_do_not_collide() {
        let dir = unique_test_dir("concurrent");
        let path = dir.join("shared.txt");

        let handles: Vec<_> = (0..100)
            .map(|idx| {
                let path = path.clone();
                thread::spawn(move || {
                    let content = format!("writer-{idx:03}:{}", "x".repeat(2048));
                    atomic_write(&path, content.as_bytes()).unwrap();
                    content
                })
            })
            .collect();

        let expected: HashSet<_> = handles
            .into_iter()
            .map(|handle| handle.join().expect("writer thread"))
            .collect();
        let final_contents = fs::read_to_string(&path).unwrap();
        assert!(
            expected.contains(&final_contents),
            "final contents must be one complete writer payload"
        );

        let stray: Vec<_> = fs::read_dir(&dir)
            .unwrap()
            .filter_map(|entry| {
                let name = entry.unwrap().file_name().to_string_lossy().into_owned();
                (name.starts_with('.') && name.contains(".tmp.")).then_some(name)
            })
            .collect();
        assert!(stray.is_empty(), "stray tempfile: {:?}", stray);
        let _ = fs::remove_dir_all(&dir);
    }
}
