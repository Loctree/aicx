// App-only integration surface: compiled to an empty target under the slim
// `loctree-consumer` profile (`--no-default-features`).
#![cfg(feature = "app")]

use std::fs;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aicx::state::StateManager;
use chrono::{TimeZone, Utc};

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

static AICX_HOME_LOCK: OnceLock<Mutex<()>> = OnceLock::new();

struct ScopedAicxHome {
    dir: PathBuf,
    previous: Option<std::ffi::OsString>,
}

impl ScopedAicxHome {
    fn new(name: &str) -> Self {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "aicx-state-lock-{}-{}-{suffix}",
            std::process::id(),
            name
        ));
        fs::create_dir_all(&dir).expect("temp AICX_HOME");
        let previous = std::env::var_os("AICX_HOME");
        // SAFETY: this test binary serializes AICX_HOME mutations with
        // AICX_HOME_LOCK and joins worker threads before the guard drops.
        unsafe {
            std::env::set_var("AICX_HOME", &dir);
        }
        Self { dir, previous }
    }
}

impl Drop for ScopedAicxHome {
    fn drop(&mut self) {
        // SAFETY: see ScopedAicxHome::new; the same mutex guard is held until
        // after this drop in tests that mutate AICX_HOME.
        unsafe {
            if let Some(previous) = &self.previous {
                std::env::set_var("AICX_HOME", previous);
            } else {
                std::env::remove_var("AICX_HOME");
            }
        }
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn ts(seconds: i64) -> chrono::DateTime<Utc> {
    Utc.timestamp_opt(seconds, 0).single().expect("timestamp")
}

fn locked_state_update(source: &str, project: &str, watermark: i64, hash: String, hold: Duration) {
    let _guard =
        aicx::locks::acquire_exclusive(aicx::locks::state_lock_path().expect("state lock path"))
            .expect("state lock");
    let mut state = StateManager::load().expect("load state");
    thread::sleep(hold);
    state.mark_seen(project, hash);
    state.update_watermark(source, ts(watermark));
    state.record_run(1, vec![source.to_string()]);
    state.save().expect("save state");
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

#[test]
fn test_concurrent_run_store_does_not_lose_state_updates() {
    let _env_lock = AICX_HOME_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .expect("AICX_HOME test lock");
    let _home = ScopedAicxHome::new("rmw");
    let source = "claude:test";
    let project = "Vetcoders/Vista";

    let first = thread::spawn(move || {
        locked_state_update(
            source,
            project,
            10,
            "101".to_string(),
            Duration::from_millis(150),
        );
    });
    thread::sleep(Duration::from_millis(25));
    let second = thread::spawn(move || {
        locked_state_update(
            source,
            project,
            20,
            "202".to_string(),
            Duration::from_millis(0),
        );
    });

    first.join().expect("first state update");
    second.join().expect("second state update");

    let final_state = StateManager::load().expect("final state");
    assert_eq!(final_state.get_watermark(source), Some(ts(20)));
    assert!(!final_state.is_new(project, "101"));
    assert!(!final_state.is_new(project, "202"));
    assert_eq!(final_state.runs.len(), 2);
}

#[test]
fn test_contention_succeeds_under_60s_timeout() {
    let path = temp_lock("timeout_success");
    let first = aicx::locks::acquire_exclusive(&path).expect("first lock");
    let worker_path = path.clone();

    let started = Instant::now();
    let worker = thread::spawn(move || {
        // Will block until first releases it, but won't timeout because default is 60s
        aicx::locks::acquire_exclusive(&worker_path).expect("second lock shouldn't timeout")
    });

    // Hold the lock for 6 seconds (longer than old 5s timeout)
    thread::sleep(Duration::from_secs(6));
    aicx::locks::release(first);

    let second = worker.join().expect("worker");
    assert!(started.elapsed() >= Duration::from_secs(6));
    aicx::locks::release(second);
}
