//! Cross-process advisory locks for the shared `~/.aicx` store.
//!
//! The lock files live in `~/.aicx/locks/` and use POSIX fcntl record locks
//! so separate CLI/MCP processes serialize writes to shared state.

use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(60);
const STALE_AFTER: Duration = Duration::from_secs(60);
const RETRY_DELAY: Duration = Duration::from_millis(25);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Debug)]
pub struct LockHandle {
    file: File,
    local_guard: Option<LocalGuard>,
}

impl Drop for LockHandle {
    fn drop(&mut self) {
        let _ = fcntl_unlock(&self.file);
        self.local_guard.take();
    }
}

pub fn acquire_exclusive(path: impl AsRef<Path>) -> Result<LockHandle> {
    acquire_with_timeout(path.as_ref(), LockMode::Exclusive, DEFAULT_TIMEOUT)
}

pub fn acquire_shared(path: impl AsRef<Path>) -> Result<LockHandle> {
    acquire_with_timeout(path.as_ref(), LockMode::Shared, DEFAULT_TIMEOUT)
}

pub fn acquire_exclusive_with_timeout(
    path: impl AsRef<Path>,
    timeout: Duration,
) -> Result<LockHandle> {
    acquire_with_timeout(path.as_ref(), LockMode::Exclusive, timeout)
}

pub fn acquire_shared_with_timeout(
    path: impl AsRef<Path>,
    timeout: Duration,
) -> Result<LockHandle> {
    acquire_with_timeout(path.as_ref(), LockMode::Shared, timeout)
}

pub fn release(handle: LockHandle) {
    drop(handle);
}

pub fn state_lock_path() -> Result<PathBuf> {
    resource_lock_path("state.lock")
}

pub fn steer_lock_path() -> Result<PathBuf> {
    resource_lock_path("steer.lock")
}

pub fn lance_lock_path() -> Result<PathBuf> {
    resource_lock_path("lance.lock")
}

pub fn index_lock_path() -> Result<PathBuf> {
    resource_lock_path("index.lock")
}

fn resource_lock_path(name: &str) -> Result<PathBuf> {
    Ok(crate::store::store_base_dir()?.join("locks").join(name))
}

fn acquire_with_timeout(path: &Path, mode: LockMode, timeout: Duration) -> Result<LockHandle> {
    let deadline = Instant::now() + timeout;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create lock dir: {}", parent.display()))?;
    }

    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .with_context(|| format!("failed to open lock file: {}", path.display()))?;

    warn_if_stale_holder(&mut file, path)?;
    let local_guard = acquire_local(path.to_path_buf(), mode, deadline)?;

    loop {
        match fcntl_try_lock(&file, mode) {
            Ok(()) => {
                write_holder(&mut file)?;
                return Ok(LockHandle {
                    file,
                    local_guard: Some(local_guard),
                });
            }
            Err(err) if lock_would_block(&err) => {
                if Instant::now() >= deadline {
                    return Err(anyhow!(
                        "timed out acquiring {} lock: {}",
                        mode.label(),
                        path.display()
                    ));
                }
                warn_if_stale_holder(&mut file, path)?;
                std::thread::sleep(RETRY_DELAY);
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to acquire lock: {}", path.display()));
            }
        }
    }
}

fn write_holder(file: &mut File) -> Result<()> {
    let pid = std::process::id();
    let timestamp = epoch_seconds(SystemTime::now())?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "pid={pid}")?;
    writeln!(file, "timestamp={timestamp}")?;
    file.sync_all()?;
    Ok(())
}

fn warn_if_stale_holder(file: &mut File, path: &Path) -> Result<()> {
    let Some(holder) = read_holder(file)? else {
        return Ok(());
    };
    let age = SystemTime::now()
        .duration_since(UNIX_EPOCH + Duration::from_secs(holder.timestamp))
        .unwrap_or_default();
    if age > STALE_AFTER && !pid_is_alive(holder.pid) {
        tracing::warn!(
            "Recovering stale aicx lock {} held by dead pid {} for {:?}",
            path.display(),
            holder.pid,
            age
        );
    }
    Ok(())
}

#[derive(Debug)]
struct Holder {
    pid: u32,
    timestamp: u64,
}

fn read_holder(file: &mut File) -> Result<Option<Holder>> {
    let mut contents = String::new();
    file.seek(SeekFrom::Start(0))?;
    file.read_to_string(&mut contents)?;

    let mut pid = None;
    let mut timestamp = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid = value.trim().parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("timestamp=") {
            timestamp = value.trim().parse::<u64>().ok();
        }
    }

    Ok(match (pid, timestamp) {
        (Some(pid), Some(timestamp)) => Some(Holder { pid, timestamp }),
        _ => None,
    })
}

fn epoch_seconds(time: SystemTime) -> Result<u64> {
    Ok(time
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: kill(pid, 0) performs existence/permission probing and does not
    // deliver a signal.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return true;
    }
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

fn fcntl_try_lock(file: &File, mode: LockMode) -> Result<()> {
    let lock_type = match mode {
        LockMode::Shared => libc::F_RDLCK,
        LockMode::Exclusive => libc::F_WRLCK,
    };
    fcntl_set_lock(file, lock_type as libc::c_short)
}

fn fcntl_unlock(file: &File) -> Result<()> {
    fcntl_set_lock(file, libc::F_UNLCK as libc::c_short)
}

fn fcntl_set_lock(file: &File, lock_type: libc::c_short) -> Result<()> {
    let mut lock = libc::flock {
        l_type: lock_type,
        l_whence: libc::SEEK_SET as libc::c_short,
        l_start: 0,
        l_len: 0,
        l_pid: 0,
    };
    // SAFETY: `lock` is a valid flock structure for the lifetime of the call
    // and `file` is an open descriptor.
    let result = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_SETLK, &mut lock) };
    if result == -1 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

fn lock_would_block(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        .is_some_and(|code| code == libc::EACCES || code == libc::EAGAIN)
}

impl LockMode {
    fn label(self) -> &'static str {
        match self {
            LockMode::Shared => "shared",
            LockMode::Exclusive => "exclusive",
        }
    }
}

#[derive(Debug)]
struct LocalLock {
    state: Mutex<LocalState>,
    ready: Condvar,
}

#[derive(Debug, Default)]
struct LocalState {
    readers: usize,
    writer: bool,
}

#[derive(Debug)]
struct LocalGuard {
    lock: Arc<LocalLock>,
    mode: LockMode,
}

impl Drop for LocalGuard {
    fn drop(&mut self) {
        let mut state = self.lock.state.lock().expect("local lock poisoned");
        match self.mode {
            LockMode::Shared => state.readers = state.readers.saturating_sub(1),
            LockMode::Exclusive => state.writer = false,
        }
        self.lock.ready.notify_all();
    }
}

static LOCAL_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<LocalLock>>>> = OnceLock::new();

fn acquire_local(path: PathBuf, mode: LockMode, deadline: Instant) -> Result<LocalGuard> {
    let lock = {
        let mut locks = LOCAL_LOCKS
            .get_or_init(|| Mutex::new(HashMap::new()))
            .lock()
            .expect("local lock registry poisoned");
        locks.retain(|_, weak| weak.strong_count() > 0);
        match locks.entry(path) {
            Entry::Occupied(mut entry) => entry.get().upgrade().unwrap_or_else(|| {
                let lock = Arc::new(LocalLock {
                    state: Mutex::new(LocalState::default()),
                    ready: Condvar::new(),
                });
                entry.insert(Arc::downgrade(&lock));
                lock
            }),
            Entry::Vacant(entry) => {
                let lock = Arc::new(LocalLock {
                    state: Mutex::new(LocalState::default()),
                    ready: Condvar::new(),
                });
                entry.insert(Arc::downgrade(&lock));
                lock
            }
        }
    };

    let mut state = lock.state.lock().expect("local lock poisoned");
    loop {
        let can_acquire = match mode {
            LockMode::Shared => !state.writer,
            LockMode::Exclusive => !state.writer && state.readers == 0,
        };
        if can_acquire {
            match mode {
                LockMode::Shared => state.readers += 1,
                LockMode::Exclusive => state.writer = true,
            }
            drop(state);
            return Ok(LocalGuard { lock, mode });
        }

        let now = Instant::now();
        if now >= deadline {
            return Err(anyhow!("timed out acquiring local {} lock", mode.label()));
        }
        let wait = (deadline - now).min(RETRY_DELAY);
        let (next_state, _) = lock
            .ready
            .wait_timeout(state, wait)
            .map_err(|_| anyhow!("local lock poisoned"))?;
        state = next_state;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    fn temp_lock(name: &str) -> PathBuf {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "aicx-locks-test-{}-{}-{}.lock",
            std::process::id(),
            name,
            epoch_seconds(SystemTime::now()).unwrap()
        ));
        let _ = fs::remove_file(&path);
        path
    }

    #[test]
    fn exclusive_acquire_release_roundtrip() {
        let path = temp_lock("roundtrip");
        let handle = acquire_exclusive(&path).expect("acquire lock");
        assert!(path.exists());
        release(handle);
        let _second = acquire_exclusive(&path).expect("reacquire lock");
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exclusive_contention_blocks_then_succeeds() {
        let path = temp_lock("contention");
        let first = acquire_exclusive(&path).expect("first lock");
        let thread_path = path.clone();
        let started = Instant::now();
        let worker = thread::spawn(move || acquire_exclusive(&thread_path).expect("second lock"));
        thread::sleep(Duration::from_millis(150));
        release(first);
        let second = worker.join().expect("worker");
        assert!(started.elapsed() >= Duration::from_millis(100));
        release(second);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn shared_locks_can_overlap() {
        let path = temp_lock("shared");
        let first = acquire_shared(&path).expect("first shared");
        let second = acquire_shared(&path).expect("second shared");
        release(first);
        release(second);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn stale_holder_is_recovered() {
        let path = temp_lock("stale");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        let stale_timestamp = epoch_seconds(SystemTime::now() - Duration::from_secs(120)).unwrap();
        fs::write(
            &path,
            format!("pid=99999999\ntimestamp={stale_timestamp}\n"),
        )
        .unwrap();
        let handle = acquire_exclusive(&path).expect("recover stale holder");
        let contents = fs::read_to_string(&path).unwrap();
        assert!(contents.contains(&format!("pid={}", std::process::id())));
        release(handle);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn exclusive_timeout_fails() {
        let path = temp_lock("timeout");
        let first = acquire_exclusive(&path).expect("first lock");
        let err = acquire_exclusive_with_timeout(&path, Duration::from_millis(75))
            .expect_err("second lock should time out");
        assert!(err.to_string().contains("timed out"));
        release(first);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn test_default_timeout_is_60_seconds() {
        assert_eq!(DEFAULT_TIMEOUT, Duration::from_secs(60));
    }

    #[test]
    fn test_with_timeout_override_respects_arg() {
        let path = temp_lock("override_timeout");
        let first = acquire_exclusive(&path).expect("first lock");

        let started = Instant::now();
        let err = acquire_exclusive_with_timeout(&path, Duration::from_secs(1))
            .expect_err("should time out");

        let elapsed = started.elapsed();
        assert!(err.to_string().contains("timed out"));
        assert!(elapsed >= Duration::from_secs(1));
        assert!(elapsed < Duration::from_secs(2));

        release(first);
        let _ = fs::remove_file(path);
    }
}
