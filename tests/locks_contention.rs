use std::fs;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn temp_lock(name: &str) -> PathBuf {
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    let mut path = std::env::temp_dir();
    path.push(format!(
        "aicx-integration-lock-{}-{}-{suffix}.lock",
        std::process::id(),
        name
    ));
    let _ = fs::remove_file(&path);
    path
}

#[test]
fn exclusive_contention_serializes_threads() {
    let path = temp_lock("exclusive");
    let first = aicx::locks::acquire_exclusive(&path).expect("first exclusive lock");
    let worker_path = path.clone();
    let started = Instant::now();

    let worker = thread::spawn(move || {
        aicx::locks::acquire_exclusive(&worker_path).expect("second exclusive lock")
    });

    thread::sleep(Duration::from_millis(150));
    aicx::locks::release(first);
    let second = worker.join().expect("worker thread");
    assert!(started.elapsed() >= Duration::from_millis(100));
    aicx::locks::release(second);
    let _ = fs::remove_file(path);
}
