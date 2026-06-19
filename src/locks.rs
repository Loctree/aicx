//! Cross-process advisory locks for the shared `~/.aicx` store.
//!
//! The lock files live in `~/.aicx/locks/` and use platform-specific advisory
//! file locks (POSIX fcntl on Unix, LockFileEx on Windows) so separate
//! CLI/MCP processes serialize writes to shared state.

use anyhow::{Context, Result, anyhow};
use chrono::{DateTime, SecondsFormat, Utc};
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
#[cfg(unix)]
use std::os::fd::AsRawFd;
#[cfg(windows)]
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock, Weak};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// Default lock wait budget. Pass-2 raised this from 5s to 60s to avoid false
/// failures during slow store/index operations, accepting a longer visible
/// freeze when a lock holder is genuinely stuck.
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
    holder_sidecar: Option<HolderSidecar>,
    local_guard: Option<LocalGuard>,
}

impl Drop for LockHandle {
    fn drop(&mut self) {
        if let Some(sidecar) = &self.holder_sidecar
            && holder_sidecar_belongs_to_handle(sidecar)
        {
            let _ = fs::remove_file(&sidecar.path);
        }
        let _ = os_unlock(&self.file);
        self.local_guard.take();
    }
}

#[derive(Debug)]
struct HolderSidecar {
    path: PathBuf,
    token: String,
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

    let local_guard = acquire_local(path.to_path_buf(), mode, deadline)?;

    loop {
        match os_try_lock(&file, path, mode) {
            Ok(()) => {
                let holder_sidecar = write_holder(&mut file, path, mode)?;
                return Ok(LockHandle {
                    file,
                    holder_sidecar,
                    local_guard: Some(local_guard),
                });
            }
            Err(err) if lock_would_block(&err) => {
                if Instant::now() >= deadline {
                    match handle_lock_timeout(path, mode)? {
                        TimeoutAction::Retry => continue,
                        TimeoutAction::Fail(err) => return Err(err),
                    }
                }
                std::thread::sleep(RETRY_DELAY);
            }
            Err(err) => {
                return Err(err)
                    .with_context(|| format!("failed to acquire lock: {}", path.display()));
            }
        }
    }
}

fn write_holder(file: &mut File, path: &Path, mode: LockMode) -> Result<Option<HolderSidecar>> {
    let holder = Holder::new(
        std::process::id(),
        SystemTime::now(),
        holder_run_kind(path, mode),
        mode,
    );
    let should_write_sidecar = should_write_holder_sidecar(path, mode);
    if should_write_sidecar {
        warn_if_recovering_stale_dead_holder(path)?;
    }

    let timestamp = epoch_seconds(holder.timestamp.into())?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    writeln!(file, "pid={}", holder.pid)?;
    writeln!(file, "timestamp={timestamp}")?;
    file.sync_all()?;

    if should_write_sidecar {
        return write_holder_sidecar(path, &holder);
    }
    Ok(None)
}

fn warn_if_recovering_stale_dead_holder(path: &Path) -> Result<()> {
    if let Some(holder) = read_holder_sidecar(path)? {
        warn_if_stale_dead_holder(path, &holder, SystemTime::now());
    }
    Ok(())
}

fn warn_if_stale_dead_holder(path: &Path, holder: &Holder, now: SystemTime) {
    let age = holder.age(now);
    if age > STALE_AFTER && !pid_is_alive(holder.pid) {
        tracing::warn!(
            "{} at {} (run_kind={}, age={} minutes)",
            recovered_stale_dead_message(holder),
            path.display(),
            holder.run_kind,
            age_minutes(age)
        );
    }
}

#[derive(Debug)]
struct Holder {
    pid: u32,
    timestamp: DateTime<Utc>,
    run_kind: String,
    mode: LockMode,
    token: String,
}

impl Holder {
    fn new(pid: u32, timestamp: SystemTime, run_kind: &str, mode: LockMode) -> Self {
        Self {
            pid,
            timestamp: DateTime::<Utc>::from(timestamp),
            run_kind: run_kind.to_string(),
            mode,
            token: new_holder_token(pid, timestamp, mode),
        }
    }

    fn age(&self, now: SystemTime) -> Duration {
        let timestamp: SystemTime = self.timestamp.into();
        now.duration_since(timestamp).unwrap_or_default()
    }
}

fn read_holder_sidecar(path: &Path) -> Result<Option<Holder>> {
    let Some(sidecar) = holder_sidecar_path(path) else {
        return Ok(None);
    };
    let contents = match fs::read_to_string(&sidecar) {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => {
            return Err(err).with_context(|| format!("read lock holder: {}", sidecar.display()));
        }
    };
    parse_holder_sidecar(&contents)
}

fn parse_holder_sidecar(contents: &str) -> Result<Option<Holder>> {
    let mut pid = None;
    let mut timestamp = None;
    let mut run_kind = None;
    let mut mode = None;
    let mut token = None;
    for line in contents.lines() {
        if let Some(value) = line.strip_prefix("pid=") {
            pid = value.trim().parse::<u32>().ok();
        } else if let Some(value) = line.strip_prefix("timestamp=") {
            timestamp = DateTime::parse_from_rfc3339(value.trim())
                .ok()
                .map(|ts| ts.with_timezone(&Utc));
        } else if let Some(value) = line.strip_prefix("run_kind=") {
            run_kind = Some(value.trim().to_string());
        } else if let Some(value) = line.strip_prefix("mode=") {
            mode = LockMode::from_sidecar_value(value.trim());
        } else if let Some(value) = line.strip_prefix("token=") {
            token = Some(value.trim().to_string());
        }
    }

    Ok(match (pid, timestamp) {
        (Some(pid), Some(timestamp)) => Some(Holder {
            pid,
            timestamp,
            run_kind: run_kind.unwrap_or_else(|| "unknown".to_string()),
            mode: mode.unwrap_or(LockMode::Exclusive),
            token: token.unwrap_or_default(),
        }),
        _ => None,
    })
}

fn write_holder_sidecar(path: &Path, holder: &Holder) -> Result<Option<HolderSidecar>> {
    let Some(sidecar) = holder_sidecar_path(path) else {
        return Ok(None);
    };
    let content = format!(
        "pid={}\ntimestamp={}\nrun_kind={}\nmode={}\ntoken={}\n",
        holder.pid,
        holder.timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
        holder.run_kind,
        holder.mode.sidecar_value(),
        holder.token
    );
    crate::store::atomic_write::atomic_write(&sidecar, content.as_bytes())
        .with_context(|| format!("write lock holder sidecar: {}", sidecar.display()))?;
    Ok(Some(HolderSidecar {
        path: sidecar,
        token: holder.token.clone(),
    }))
}

fn holder_sidecar_path(path: &Path) -> Option<PathBuf> {
    let mut file_name = path.file_name()?.to_os_string();
    file_name.push(".holder");
    Some(path.with_file_name(file_name))
}

fn should_write_holder_sidecar(path: &Path, mode: LockMode) -> bool {
    // Sidecar identifies the BLOCKING holder for timeout-waiting acquirers.
    // Multiple shared holders racing on a single sidecar token would have
    // one cleanup wipe the file while siblings still hold — making
    // diagnostics unreliable. Exclusive-only keeps the contract honest.
    matches!(mode, LockMode::Exclusive) && path.file_name().is_some()
}

fn holder_run_kind(path: &Path, mode: LockMode) -> &'static str {
    let _ = mode;
    match path.file_name().and_then(|name| name.to_str()) {
        Some("lance.lock" | "index.lock") => "aicx index",
        Some("state.lock") => "aicx state",
        Some("steer.lock") => "aicx steer",
        Some("mcp.lock") => "aicx mcp",
        _ => "unknown",
    }
}

enum TimeoutAction {
    Retry,
    Fail(anyhow::Error),
}

fn handle_lock_timeout(path: &Path, mode: LockMode) -> Result<TimeoutAction> {
    let Some(holder) = read_holder_sidecar(path)? else {
        return Ok(TimeoutAction::Fail(lock_timeout_error(path, mode)));
    };

    if !pid_is_alive(holder.pid) {
        tracing::warn!(
            "{} at {} (run_kind={})",
            recovered_stale_dead_message(&holder),
            path.display(),
            holder.run_kind
        );
        if let Some(sidecar) = holder_sidecar_path(path) {
            let _ = fs::remove_file(sidecar);
        }
        return Ok(TimeoutAction::Retry);
    }

    let minutes = age_minutes(holder.age(SystemTime::now()));
    Ok(TimeoutAction::Fail(anyhow!(
        "timed out acquiring {} lock: {}; lock held by PID {} (run_kind={}) for {} minutes; consider killing manually with `kill {}`",
        mode.label(),
        path.display(),
        holder.pid,
        holder.run_kind,
        minutes,
        holder.pid
    )))
}

fn lock_timeout_error(path: &Path, mode: LockMode) -> anyhow::Error {
    anyhow!(
        "timed out acquiring {} lock: {}",
        mode.label(),
        path.display()
    )
}

fn recovered_stale_dead_message(holder: &Holder) -> String {
    format!("recovered stale lock from dead PID {}", holder.pid)
}

static HOLDER_COUNTER: AtomicU64 = AtomicU64::new(0);

fn new_holder_token(pid: u32, timestamp: SystemTime, mode: LockMode) -> String {
    let nanos = timestamp
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let counter = HOLDER_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{}-{}-{}-{}", pid, mode.sidecar_value(), nanos, counter)
}

fn holder_sidecar_belongs_to_handle(sidecar: &HolderSidecar) -> bool {
    let Ok(contents) = fs::read_to_string(&sidecar.path) else {
        return false;
    };
    let Ok(Some(holder)) = parse_holder_sidecar(&contents) else {
        return false;
    };
    holder.token == sidecar.token
}

fn age_minutes(age: Duration) -> u64 {
    age.as_secs() / 60
}

fn epoch_seconds(time: SystemTime) -> Result<u64> {
    Ok(time
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs())
}

#[cfg(unix)]
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

#[cfg(windows)]
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // PROCESS_QUERY_LIMITED_INFORMATION is sufficient to probe existence.
    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    // ERROR_ACCESS_DENIED means the process exists but we lack permission.
    const ERROR_ACCESS_DENIED: i32 = 5;
    // SAFETY: FFI call; returned handle is valid when non-null.
    let handle = unsafe { windows_ffi::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return std::io::Error::last_os_error().raw_os_error() == Some(ERROR_ACCESS_DENIED);
    }
    unsafe { windows_ffi::CloseHandle(handle) };
    true
}

#[cfg(unix)]
fn os_try_lock(file: &File, path: &Path, mode: LockMode) -> Result<()> {
    #[cfg(not(test))]
    let _ = path;

    #[cfg(test)]
    if forced_would_block(path) {
        return Err(std::io::Error::from_raw_os_error(libc::EAGAIN).into());
    }

    let lock_type = match mode {
        LockMode::Shared => libc::F_RDLCK,
        LockMode::Exclusive => libc::F_WRLCK,
    };
    fcntl_set_lock(file, lock_type as libc::c_short)
}

#[cfg(unix)]
fn os_unlock(file: &File) -> Result<()> {
    fcntl_set_lock(file, libc::F_UNLCK as libc::c_short)
}

#[cfg(unix)]
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

#[cfg(windows)]
fn os_try_lock(file: &File, path: &Path, mode: LockMode) -> Result<()> {
    #[cfg(not(test))]
    let _ = path;

    #[cfg(test)]
    if forced_would_block(path) {
        // Simulate a lock-violation error so lock_would_block detects it.
        return Err(std::io::Error::from_raw_os_error(windows_ffi::ERROR_LOCK_VIOLATION).into());
    }

    const LOCKFILE_FAIL_IMMEDIATELY: u32 = 0x00000001;
    const LOCKFILE_EXCLUSIVE_LOCK: u32 = 0x00000002;

    let flags = LOCKFILE_FAIL_IMMEDIATELY
        | match mode {
            LockMode::Exclusive => LOCKFILE_EXCLUSIVE_LOCK,
            LockMode::Shared => 0,
        };

    let mut overlapped = windows_ffi::OVERLAPPED {
        internal: 0,
        internal_high: 0,
        offset: 0,
        offset_high: 0,
        h_event: std::ptr::null_mut(),
    };

    // SAFETY: `file` is a valid open handle; `overlapped` is a properly
    // zeroed structure valid for the lifetime of the call.
    let result = unsafe {
        windows_ffi::LockFileEx(
            file.as_raw_handle().cast(),
            flags,
            0,
            u32::MAX,
            u32::MAX,
            &mut overlapped,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn os_unlock(file: &File) -> Result<()> {
    // SAFETY: `file` is a valid open handle.
    let result =
        unsafe { windows_ffi::UnlockFile(file.as_raw_handle().cast(), 0, 0, u32::MAX, u32::MAX) };
    if result == 0 {
        Err(std::io::Error::last_os_error().into())
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn lock_would_block(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        .is_some_and(|code| code == libc::EACCES || code == libc::EAGAIN)
}

#[cfg(windows)]
fn lock_would_block(err: &anyhow::Error) -> bool {
    err.downcast_ref::<std::io::Error>()
        .and_then(std::io::Error::raw_os_error)
        .is_some_and(|code| code == windows_ffi::ERROR_LOCK_VIOLATION)
}

/// Windows-only FFI declarations for file-locking and process-existence APIs.
#[cfg(windows)]
mod windows_ffi {
    use std::ffi::c_void;

    pub const ERROR_LOCK_VIOLATION: i32 = 33;

    /// Mirror of the Win32 `OVERLAPPED` structure used by `LockFileEx`.
    /// Only the `Offset`/`OffsetHigh` fields (union first member) and
    /// `hEvent` matter for our non-async usage; all others must be zero.
    #[repr(C)]
    pub struct OVERLAPPED {
        pub internal: usize,
        pub internal_high: usize,
        pub offset: u32,
        pub offset_high: u32,
        pub h_event: *mut c_void,
    }

    // SAFETY: OVERLAPPED contains only plain integer fields and a raw pointer.
    // It is never shared across threads without synchronisation.
    unsafe impl Send for OVERLAPPED {}

    extern "system" {
        pub fn LockFileEx(
            h_file: *mut c_void,
            dw_flags: u32,
            dw_reserved: u32,
            n_number_of_bytes_to_lock_low: u32,
            n_number_of_bytes_to_lock_high: u32,
            lp_overlapped: *mut OVERLAPPED,
        ) -> i32;

        pub fn UnlockFile(
            h_file: *mut c_void,
            dw_file_offset_low: u32,
            dw_file_offset_high: u32,
            n_number_of_bytes_to_unlock_low: u32,
            n_number_of_bytes_to_unlock_high: u32,
        ) -> i32;

        pub fn OpenProcess(
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_process_id: u32,
        ) -> *mut c_void;

        pub fn CloseHandle(h_object: *mut c_void) -> i32;
    }
}

impl LockMode {
    fn label(self) -> &'static str {
        match self {
            LockMode::Shared => "shared",
            LockMode::Exclusive => "exclusive",
        }
    }

    fn sidecar_value(self) -> &'static str {
        match self {
            LockMode::Shared => "shared",
            LockMode::Exclusive => "exclusive",
        }
    }

    fn from_sidecar_value(value: &str) -> Option<Self> {
        match value {
            "shared" => Some(LockMode::Shared),
            "exclusive" => Some(LockMode::Exclusive),
            _ => None,
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

#[cfg(test)]
static FORCED_WOULD_BLOCK_PATH: OnceLock<Mutex<Option<PathBuf>>> = OnceLock::new();

#[cfg(test)]
fn set_forced_would_block_path(path: Option<PathBuf>) {
    *FORCED_WOULD_BLOCK_PATH
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("forced lock path poisoned") = path;
}

#[cfg(test)]
fn forced_would_block(path: &Path) -> bool {
    FORCED_WOULD_BLOCK_PATH
        .get_or_init(|| Mutex::new(None))
        .lock()
        .expect("forced lock path poisoned")
        .as_ref()
        .is_some_and(|forced| forced == path)
}

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

#[cfg(all(test, feature = "app"))]
mod tests {
    use super::*;
    use crate::test_support::capture_logs;
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

    fn temp_lance_lock(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aicx-lance-lock-test-{}-{}-{}",
            std::process::id(),
            name,
            epoch_seconds(SystemTime::now()).unwrap()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("lance.lock");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(holder_sidecar_path(&path).unwrap());
        path
    }

    fn temp_named_lock(name: &str, file_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aicx-named-lock-test-{}-{}-{}",
            std::process::id(),
            name,
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join(file_name);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(holder_sidecar_path(&path).unwrap());
        path
    }

    struct ForcedWouldBlockGuard;

    impl ForcedWouldBlockGuard {
        fn new(path: &Path) -> Self {
            set_forced_would_block_path(Some(path.to_path_buf()));
            Self
        }
    }

    impl Drop for ForcedWouldBlockGuard {
        fn drop(&mut self) {
            set_forced_would_block_path(None);
        }
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
    fn lance_lock_writes_sidecar_and_cleans_up_on_release() {
        let path = temp_lance_lock("sidecar");
        let sidecar = holder_sidecar_path(&path).unwrap();
        let handle = acquire_exclusive(&path).expect("acquire lance lock");
        let contents = fs::read_to_string(&sidecar).expect("holder sidecar");
        assert!(contents.contains(&format!("pid={}", std::process::id())));
        assert!(contents.contains("timestamp="));
        assert!(contents.contains("T"));
        assert!(contents.contains("run_kind=aicx index"));
        assert!(contents.contains("mode=exclusive"));
        assert!(contents.contains("token="));
        release(handle);
        assert!(!sidecar.exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn non_lance_exclusive_lock_writes_sidecar_and_cleans_up_on_release() {
        let path = temp_named_lock("state-sidecar", "state.lock");
        let sidecar = holder_sidecar_path(&path).unwrap();
        let handle = acquire_exclusive(&path).expect("acquire state lock");

        let contents = fs::read_to_string(&sidecar).expect("holder sidecar");
        assert!(contents.contains(&format!("pid={}", std::process::id())));
        assert!(contents.contains("run_kind=aicx state"));
        assert!(contents.contains("mode=exclusive"));

        release(handle);
        assert!(!sidecar.exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn shared_lock_does_not_write_sidecar() {
        // Per PR #6 review (Copilot): shared locks do NOT write holder
        // sidecars. Concurrent shared acquirers would race on the same
        // sidecar token, letting one releaser wipe the file while siblings
        // still hold — making timeout diagnostics for waiting exclusive
        // acquirers unreliable. Sidecars are exclusive-only.
        let path = temp_named_lock("mcp-shared-no-sidecar", "mcp.lock");
        let sidecar = holder_sidecar_path(&path).unwrap();
        let handle = acquire_shared(&path).expect("acquire shared mcp lock");

        assert!(
            !sidecar.exists(),
            "shared lock unexpectedly wrote holder sidecar at {}",
            sidecar.display()
        );

        release(handle);
        assert!(!sidecar.exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn stale_dead_lance_holder_is_recovered_with_warning() {
        let path = temp_lance_lock("stale-dead");
        let dead_pid = 99_999_999;
        let holder = Holder::new(
            dead_pid,
            SystemTime::now() - Duration::from_secs(125),
            "aicx index",
            LockMode::Exclusive,
        );
        write_holder_sidecar(&path, &holder).expect("write stale holder");

        let (handle, logs) =
            capture_logs(|| acquire_exclusive(&path).expect("recover stale holder"));
        assert!(
            logs.contains(&format!("recovered stale lock from dead PID {dead_pid}")),
            "logs: {logs}"
        );
        let contents = fs::read_to_string(holder_sidecar_path(&path).unwrap()).unwrap();
        assert!(contents.contains(&format!("pid={}", std::process::id())));
        release(handle);
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn stale_alive_lance_holder_timeout_warns_without_auto_kill() {
        let path = temp_lance_lock("stale-alive");
        let sidecar = holder_sidecar_path(&path).unwrap();
        let holder = Holder::new(
            std::process::id(),
            SystemTime::now() - Duration::from_secs(125),
            "aicx index",
            LockMode::Exclusive,
        );
        write_holder_sidecar(&path, &holder).expect("write alive holder");

        let _forced = ForcedWouldBlockGuard::new(&path);
        let err = acquire_exclusive_with_timeout(&path, Duration::from_millis(75))
            .expect_err("alive holder should time out");
        let message = err.to_string();
        assert!(message.contains(&format!("lock held by PID {}", std::process::id())));
        assert!(message.contains("(run_kind=aicx index)"));
        assert!(message.contains("for 2 minutes; consider killing manually"));
        assert!(message.contains(&format!("kill {}", std::process::id())));
        assert!(sidecar.exists());
        let _ = fs::remove_dir_all(path.parent().unwrap());
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
