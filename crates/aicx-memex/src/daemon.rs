//! Background indexer daemon for canonical-store refresh, steer repair, and memex sync.
//!
//! The daemon owns the "don't make me think about indexing" loop:
//! - refresh canonical store via `aicx all --incremental`
//! - repair/rebuild the steer metadata index when needed
//! - materialize canonical chunks into memex incrementally
//!
//! Control is exposed over a Unix socket so CLIs and MCP surfaces can query
//! status, enqueue an immediate sync, or stop the daemon cleanly.

use anyhow::{Context, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

#[cfg(unix)]
use std::os::unix::net::{UnixListener, UnixStream};
#[cfg(unix)]
use std::os::unix::process::CommandExt;

use crate::memex::{self, MemexConfig, SyncProgress, SyncProgressPhase};
use crate::store;

const DEFAULT_SOCKET_FILENAME: &str = "aicx-memex.sock";
const DEFAULT_STATUS_FILENAME: &str = "aicx-memex.status.json";
const DEFAULT_POLL_SECONDS: u64 = 300;
const DEFAULT_REFRESH_HOURS: u64 = 720;
const SOCKET_READY_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_millis(750);
const LOOP_SLEEP: Duration = Duration::from_millis(250);

#[derive(Debug, Clone)]
pub struct DaemonConfig {
    pub socket_path: Option<PathBuf>,
    pub poll_seconds: u64,
    pub refresh_hours: u64,
    pub projects: Vec<String>,
    pub namespace: String,
    pub db_path: Option<PathBuf>,
    pub per_chunk: bool,
    pub bootstrap: bool,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            socket_path: None,
            poll_seconds: DEFAULT_POLL_SECONDS,
            refresh_hours: DEFAULT_REFRESH_HOURS,
            projects: Vec::new(),
            namespace: "ai-contexts".to_string(),
            db_path: None,
            per_chunk: false,
            bootstrap: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonPhase {
    Starting,
    RefreshingSources,
    RepairingSteer,
    SyncingMemex,
    Idle,
    Stopping,
}

impl fmt::Display for DaemonPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Starting => f.write_str("starting"),
            Self::RefreshingSources => f.write_str("refreshing_sources"),
            Self::RepairingSteer => f.write_str("repairing_steer"),
            Self::SyncingMemex => f.write_str("syncing_memex"),
            Self::Idle => f.write_str("idle"),
            Self::Stopping => f.write_str("stopping"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DaemonStatusSnapshot {
    pub pid: u32,
    pub socket_path: String,
    pub phase: DaemonPhase,
    pub phase_detail: String,
    pub started_at: DateTime<Utc>,
    pub last_cycle_started_at: Option<DateTime<Utc>>,
    pub last_cycle_completed_at: Option<DateTime<Utc>>,
    pub last_cycle_reason: Option<String>,
    pub last_cycle_summary: Option<String>,
    pub last_error: Option<String>,
    pub successful_cycles: u64,
    pub failed_cycles: u64,
    pub bootstrap_completed: bool,
    pub poll_seconds: u64,
    pub refresh_hours: u64,
    pub projects: Vec<String>,
    pub namespace: String,
    pub db_path: Option<String>,
    pub per_chunk: bool,
}

impl DaemonStatusSnapshot {
    fn new(config: &DaemonConfig, socket_path: &Path) -> Self {
        Self {
            pid: std::process::id(),
            socket_path: socket_path.display().to_string(),
            phase: DaemonPhase::Starting,
            phase_detail: "Binding control socket".to_string(),
            started_at: Utc::now(),
            last_cycle_started_at: None,
            last_cycle_completed_at: None,
            last_cycle_reason: None,
            last_cycle_summary: None,
            last_error: None,
            successful_cycles: 0,
            failed_cycles: 0,
            bootstrap_completed: false,
            poll_seconds: config.poll_seconds.max(1),
            refresh_hours: config.refresh_hours,
            projects: config.projects.clone(),
            namespace: config.namespace.clone(),
            db_path: config
                .db_path
                .as_ref()
                .map(|path| path.display().to_string()),
            per_chunk: config.per_chunk,
        }
    }
}

#[derive(Debug, Clone)]
pub enum ControlOutcome {
    Status(DaemonStatusSnapshot),
    SyncQueued(DaemonStatusSnapshot),
    SyncAlreadyQueued(DaemonStatusSnapshot),
    SyncAlreadyRunning(DaemonStatusSnapshot),
    StopQueued(DaemonStatusSnapshot),
}

impl ControlOutcome {
    pub fn snapshot(&self) -> &DaemonStatusSnapshot {
        match self {
            Self::Status(status)
            | Self::SyncQueued(status)
            | Self::SyncAlreadyQueued(status)
            | Self::SyncAlreadyRunning(status)
            | Self::StopQueued(status) => status,
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::Status(_) => "ok",
            Self::SyncQueued(_) => "sync queued",
            Self::SyncAlreadyQueued(_) => "sync already queued",
            Self::SyncAlreadyRunning(_) => "sync already running",
            Self::StopQueued(_) => "stop queued",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum DaemonRequest {
    Status,
    Sync { reason: Option<String> },
    Stop,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonResponse {
    ok: bool,
    message: String,
    status: DaemonStatusSnapshot,
}

struct SharedState {
    snapshot: DaemonStatusSnapshot,
    pending_sync: Option<String>,
    stop_requested: bool,
}

fn daemon_dir() -> Result<PathBuf> {
    let dir = store::store_base_dir()?.join("daemon");
    fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create daemon dir {}", dir.display()))?;
    Ok(dir)
}

pub fn default_socket_path() -> Result<PathBuf> {
    Ok(daemon_dir()?.join(DEFAULT_SOCKET_FILENAME))
}

pub fn default_status_path() -> Result<PathBuf> {
    Ok(daemon_dir()?.join(DEFAULT_STATUS_FILENAME))
}

fn socket_path_for(config: &DaemonConfig) -> Result<PathBuf> {
    match config.socket_path.as_ref() {
        Some(path) => Ok(path.clone()),
        None => default_socket_path(),
    }
}

fn status_path_for(config: &DaemonConfig) -> Result<PathBuf> {
    let socket_path = socket_path_for(config)?;
    if config.socket_path.is_some() {
        Ok(socket_path.with_extension("status.json"))
    } else {
        default_status_path()
    }
}

pub fn find_aicx_binary() -> PathBuf {
    if let Some(path) = std::env::var_os("AICX_BIN") {
        return PathBuf::from(path);
    }

    if let Ok(current) = std::env::current_exe() {
        if current
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name == "aicx")
        {
            return current;
        }

        if let Some(parent) = current.parent() {
            let sibling = parent.join("aicx");
            if sibling.exists() {
                return sibling;
            }
        }
    }

    PathBuf::from("aicx")
}

#[cfg(not(unix))]
pub fn spawn_detached(_config: &DaemonConfig) -> Result<()> {
    bail!("aicx-memex daemon requires Unix domain sockets")
}

#[cfg(unix)]
pub fn spawn_detached(config: &DaemonConfig) -> Result<()> {
    let socket_path = socket_path_for(config)?;
    let mut child = Command::new(find_aicx_binary());
    child
        .arg("daemon-run")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detached mode must survive the parent CLI process exiting, otherwise
    // the command appears to work but the daemon dies with the parent.
    unsafe {
        child.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            libc::signal(libc::SIGHUP, libc::SIG_IGN);
            Ok(())
        });
    }
    append_daemon_args(&mut child, config);

    let mut child = child.spawn().context("Failed to start daemon process")?;
    let start = Instant::now();

    while start.elapsed() < SOCKET_READY_TIMEOUT {
        if request_status_at(&socket_path).is_ok() {
            return Ok(());
        }

        if let Some(status) = child.try_wait().context("Failed to poll daemon child")? {
            bail!("Daemon exited early with status {status}");
        }

        thread::sleep(Duration::from_millis(100));
    }

    bail!(
        "Daemon start timed out waiting for socket {}",
        socket_path.display()
    )
}

fn append_daemon_args(command: &mut Command, config: &DaemonConfig) {
    if let Some(path) = config.socket_path.as_ref() {
        command.arg("--socket-path").arg(path);
    }
    command
        .arg("--poll-seconds")
        .arg(config.poll_seconds.to_string());
    command
        .arg("--refresh-hours")
        .arg(config.refresh_hours.to_string());
    command.arg("--namespace").arg(&config.namespace);
    if let Some(path) = config.db_path.as_ref() {
        command.arg("--db-path").arg(path);
    }
    if config.per_chunk {
        command.arg("--per-chunk");
    }
    if !config.bootstrap {
        command.arg("--no-bootstrap");
    }
    for project in &config.projects {
        command.arg("--project").arg(project);
    }
}

#[cfg(not(unix))]
pub fn run_foreground(_config: DaemonConfig) -> Result<()> {
    bail!("aicx-memex daemon requires Unix domain sockets")
}

#[cfg(unix)]
pub fn run_foreground(config: DaemonConfig) -> Result<()> {
    let socket_path = socket_path_for(&config)?;
    let status_path = status_path_for(&config)?;
    ensure_socket_parent(&socket_path)?;
    ensure_socket_parent(&status_path)?;

    if socket_path.exists() {
        match request_status_at(&socket_path) {
            Ok(_) => {
                bail!("Daemon already running at {}", socket_path.display());
            }
            Err(_) => {
                let _ = fs::remove_file(&socket_path);
            }
        }
    }

    let listener = UnixListener::bind(&socket_path)
        .with_context(|| format!("Failed to bind daemon socket at {}", socket_path.display()))?;
    listener
        .set_nonblocking(true)
        .context("Failed to mark daemon socket non-blocking")?;

    let shared = Arc::new(Mutex::new(SharedState {
        snapshot: DaemonStatusSnapshot::new(&config, &socket_path),
        pending_sync: config.bootstrap.then(|| "startup bootstrap".to_string()),
        stop_requested: false,
    }));

    update_snapshot(&shared, &status_path, |snapshot| {
        snapshot.phase = DaemonPhase::Idle;
        snapshot.phase_detail = if config.bootstrap {
            "Bootstrap queued".to_string()
        } else {
            "Idle".to_string()
        };
    })?;
    tracing::info!("aicx-memex daemon listening on {}", socket_path.display());

    let poll_interval = Duration::from_secs(config.poll_seconds.max(1));
    let mut next_poll = Instant::now() + poll_interval;

    loop {
        accept_pending_requests(&listener, &shared, &status_path)?;

        let (should_stop, reason) = {
            let mut guard = shared
                .lock()
                .expect("daemon shared state lock should not be poisoned");
            let should_stop = guard.stop_requested;
            let reason = if should_stop {
                None
            } else if let Some(reason) = guard.pending_sync.take() {
                Some(reason)
            } else if Instant::now() >= next_poll {
                Some("poll interval".to_string())
            } else {
                None
            };
            (should_stop, reason)
        };

        if should_stop {
            set_phase(
                &shared,
                &status_path,
                DaemonPhase::Stopping,
                "Shutdown requested".to_string(),
            )?;
            break;
        }

        if let Some(reason) = reason {
            run_cycle(&config, &shared, &status_path, &reason);
            next_poll = Instant::now() + poll_interval;
        } else {
            thread::sleep(LOOP_SLEEP);
        }
    }

    if socket_path.exists() {
        let _ = fs::remove_file(&socket_path);
    }
    persist_status(&status_path, &shared)?;
    Ok(())
}

#[cfg(unix)]
fn ensure_socket_parent(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("No parent directory for {}", path.display()))?;
    fs::create_dir_all(parent).with_context(|| format!("Failed to create {}", parent.display()))?;
    Ok(())
}

#[cfg(unix)]
fn accept_pending_requests(
    listener: &UnixListener,
    shared: &Arc<Mutex<SharedState>>,
    status_path: &Path,
) -> Result<()> {
    loop {
        match listener.accept() {
            Ok((stream, _addr)) => {
                if let Err(err) = handle_client(stream, shared, status_path) {
                    tracing::warn!("aicx-memex client handling failed: {err:#}");
                }
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(()),
            Err(err) => return Err(err.into()),
        }
    }
}

#[cfg(unix)]
fn handle_client(
    mut stream: UnixStream,
    shared: &Arc<Mutex<SharedState>>,
    status_path: &Path,
) -> Result<()> {
    let reader_stream = stream
        .try_clone()
        .context("Failed to clone daemon socket stream")?;
    let mut reader = BufReader::new(reader_stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .context("Failed to read daemon request")?;

    let request: DaemonRequest =
        serde_json::from_str(line.trim()).context("Failed to parse daemon request JSON")?;

    let response = {
        let mut guard = shared
            .lock()
            .expect("daemon shared state lock should not be poisoned");
        match request {
            DaemonRequest::Status => DaemonResponse {
                ok: true,
                message: "ok".to_string(),
                status: guard.snapshot.clone(),
            },
            DaemonRequest::Sync { reason } => {
                let message = if guard.snapshot.phase != DaemonPhase::Idle {
                    "sync already running".to_string()
                } else if guard.pending_sync.is_some() {
                    "sync already queued".to_string()
                } else {
                    guard.pending_sync =
                        Some(reason.unwrap_or_else(|| "manual request".to_string()));
                    "sync queued".to_string()
                };
                DaemonResponse {
                    ok: true,
                    message,
                    status: guard.snapshot.clone(),
                }
            }
            DaemonRequest::Stop => {
                guard.stop_requested = true;
                DaemonResponse {
                    ok: true,
                    message: "stop queued".to_string(),
                    status: guard.snapshot.clone(),
                }
            }
        }
    };

    persist_status(status_path, shared)?;
    writeln!(stream, "{}", serde_json::to_string(&response)?)
        .context("Failed to write daemon response")?;
    Ok(())
}

fn update_snapshot<F>(shared: &Arc<Mutex<SharedState>>, status_path: &Path, update: F) -> Result<()>
where
    F: FnOnce(&mut DaemonStatusSnapshot),
{
    {
        let mut guard = shared
            .lock()
            .expect("daemon shared state lock should not be poisoned");
        update(&mut guard.snapshot);
    }
    persist_status(status_path, shared)
}

fn set_phase(
    shared: &Arc<Mutex<SharedState>>,
    status_path: &Path,
    phase: DaemonPhase,
    detail: String,
) -> Result<()> {
    update_snapshot(shared, status_path, |snapshot| {
        snapshot.phase = phase;
        snapshot.phase_detail = detail;
    })
}

fn persist_status(status_path: &Path, shared: &Arc<Mutex<SharedState>>) -> Result<()> {
    let snapshot = {
        let guard = shared
            .lock()
            .expect("daemon shared state lock should not be poisoned");
        guard.snapshot.clone()
    };
    fs::write(status_path, serde_json::to_vec_pretty(&snapshot)?).with_context(|| {
        format!(
            "Failed to write daemon status file {}",
            status_path.display()
        )
    })?;
    Ok(())
}

fn run_cycle(
    config: &DaemonConfig,
    shared: &Arc<Mutex<SharedState>>,
    status_path: &Path,
    reason: &str,
) {
    let cycle_started = Utc::now();
    let cycle_reason = reason.to_string();

    if let Err(err) = update_snapshot(shared, status_path, |snapshot| {
        snapshot.phase = DaemonPhase::RefreshingSources;
        snapshot.phase_detail = format!("Refreshing canonical store ({reason})");
        snapshot.last_cycle_started_at = Some(cycle_started);
        snapshot.last_cycle_reason = Some(cycle_reason.clone());
        snapshot.last_error = None;
    }) {
        tracing::warn!("Failed to update daemon status before cycle: {err:#}");
    }

    let cycle_result = (|| -> Result<String> {
        let refresh_summary = refresh_canonical_store(config)?;

        set_phase(
            shared,
            status_path,
            DaemonPhase::RepairingSteer,
            "Repairing steer index".to_string(),
        )?;
        let rt = tokio::runtime::Runtime::new()
            .context("Failed to start Tokio runtime for steer repair")?;
        rt.block_on(crate::steer_index::rebuild_steer_index_if_needed())
            .context("Failed to rebuild steer index")?;

        set_phase(
            shared,
            status_path,
            DaemonPhase::SyncingMemex,
            "Scanning canonical store for memex sync".to_string(),
        )?;
        let chunk_paths: Vec<PathBuf> = store::scan_context_files()?
            .into_iter()
            .map(|file| file.path)
            .collect();
        let memex_config = MemexConfig {
            namespace: config.namespace.clone(),
            db_path: config.db_path.clone(),
            batch_mode: !config.per_chunk,
            preprocess: true,
        };
        let (sync_result, reindexed) = sync_with_auto_repair(
            || sync_memex_paths_with_status(&chunk_paths, &memex_config, shared, status_path),
            || {
                set_phase(
                    shared,
                    status_path,
                    DaemonPhase::SyncingMemex,
                    format!(
                        "Reindexing memex namespace '{}' after compatibility drift",
                        memex_config.namespace
                    ),
                )
            },
            || {
                memex::reset_semantic_index(
                    &memex_config.namespace,
                    memex_config.db_path.as_deref(),
                )
                .map(|_| ())
            },
            memex::is_compatibility_error,
        )?;

        Ok(format!(
            "{}{} | memex: {} pushed, {} skipped, {} ignored across {} canonical chunks",
            refresh_summary,
            if reindexed {
                " | memex reindexed for runtime truth"
            } else {
                ""
            },
            sync_result.chunks_pushed,
            sync_result.chunks_skipped,
            sync_result.chunks_ignored,
            chunk_paths.len()
        ))
    })();

    let completed_at = Utc::now();
    match cycle_result {
        Ok(summary) => {
            tracing::info!("aicx-memex cycle complete: {summary}");
            let _ = update_snapshot(shared, status_path, |snapshot| {
                snapshot.phase = DaemonPhase::Idle;
                snapshot.phase_detail = "Idle".to_string();
                snapshot.last_cycle_completed_at = Some(completed_at);
                snapshot.last_cycle_summary = Some(summary);
                snapshot.successful_cycles += 1;
                snapshot.bootstrap_completed = true;
                snapshot.last_error = None;
            });
        }
        Err(err) => {
            tracing::warn!("aicx-memex cycle failed: {err:#}");
            let _ = update_snapshot(shared, status_path, |snapshot| {
                snapshot.phase = DaemonPhase::Idle;
                snapshot.phase_detail = "Idle after failed cycle".to_string();
                snapshot.last_cycle_completed_at = Some(completed_at);
                snapshot.failed_cycles += 1;
                snapshot.last_error = Some(format!("{err:#}"));
            });
        }
    }
}

fn sync_memex_paths_with_status(
    chunk_paths: &[PathBuf],
    memex_config: &MemexConfig,
    shared: &Arc<Mutex<SharedState>>,
    status_path: &Path,
) -> Result<memex::SyncResult> {
    let shared_for_progress = Arc::clone(shared);
    let status_for_progress = status_path.to_path_buf();
    memex::sync_new_chunk_paths_with_progress(chunk_paths, memex_config, move |progress| {
        let detail = format_memex_progress(&progress);
        let _ = update_snapshot(&shared_for_progress, &status_for_progress, |snapshot| {
            snapshot.phase = DaemonPhase::SyncingMemex;
            snapshot.phase_detail = detail;
        });
    })
}

fn sync_with_auto_repair<S, B, R, P>(
    mut sync_once: S,
    mut before_retry: B,
    mut reset_index: R,
    is_repairable: P,
) -> Result<(memex::SyncResult, bool)>
where
    S: FnMut() -> Result<memex::SyncResult>,
    B: FnMut() -> Result<()>,
    R: FnMut() -> Result<()>,
    P: Fn(&anyhow::Error) -> bool,
{
    match sync_once() {
        Ok(result) => Ok((result, false)),
        Err(err) if is_repairable(&err) => {
            before_retry()?;
            reset_index()?;
            let retried =
                sync_once().context("Failed memex sync after daemon-triggered reindex")?;
            Ok((retried, true))
        }
        Err(err) => Err(err),
    }
}

fn refresh_canonical_store(config: &DaemonConfig) -> Result<String> {
    let mut command = Command::new(find_aicx_binary());
    command
        .arg("all")
        .arg("-H")
        .arg(config.refresh_hours.to_string())
        .arg("--incremental")
        .arg("--emit")
        .arg("none")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    for project in &config.projects {
        command.arg("--project").arg(project);
    }

    let output = command
        .output()
        .context("Failed to run `aicx all --incremental` for daemon refresh")?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let combined = format!("{}\n{}", stdout.trim(), stderr.trim());

    if !output.status.success() {
        bail!(
            "Canonical refresh failed: {}",
            summarize_command_output(&combined)
        );
    }

    let summary = summarize_command_output(&combined);
    if summary.is_empty() {
        Ok(format!(
            "canonical refresh ok ({}h{} window)",
            config.refresh_hours,
            render_project_scope(&config.projects)
        ))
    } else {
        Ok(format!(
            "canonical refresh ok ({}h{} window): {}",
            config.refresh_hours,
            render_project_scope(&config.projects),
            summary
        ))
    }
}

fn render_project_scope(projects: &[String]) -> String {
    if projects.is_empty() {
        String::new()
    } else {
        format!(", projects: {}", projects.join(", "))
    }
}

fn summarize_command_output(output: &str) -> String {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .rev()
        .take(6)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join(" | ")
}

fn format_memex_progress(progress: &SyncProgress) -> String {
    match progress.phase {
        SyncProgressPhase::Discovering => {
            format!(
                "Memex scan {}/{}",
                progress.done.max(1),
                progress.total.max(1)
            )
        }
        SyncProgressPhase::Embedding => {
            format!(
                "Memex embed {}/{}",
                progress.done.max(1),
                progress.total.max(1)
            )
        }
        SyncProgressPhase::Writing => {
            format!(
                "Memex index {}/{}",
                progress.done.max(1),
                progress.total.max(1)
            )
        }
        SyncProgressPhase::Completed => progress.detail.clone(),
    }
}

#[cfg(not(unix))]
pub fn request_status() -> Result<ControlOutcome> {
    bail!("aicx-memex daemon requires Unix domain sockets")
}

#[cfg(unix)]
pub fn request_status() -> Result<ControlOutcome> {
    let socket_path = default_socket_path()?;
    request_status_at(&socket_path)
}

#[cfg(unix)]
pub fn request_status_at(socket_path: &Path) -> Result<ControlOutcome> {
    let response = send_request(socket_path, &DaemonRequest::Status)?;
    Ok(ControlOutcome::Status(response.status))
}

#[cfg(not(unix))]
pub fn request_sync(
    _socket_path: Option<&Path>,
    _reason: Option<String>,
) -> Result<ControlOutcome> {
    bail!("aicx-memex daemon requires Unix domain sockets")
}

#[cfg(unix)]
pub fn request_sync(socket_path: Option<&Path>, reason: Option<String>) -> Result<ControlOutcome> {
    let socket_path = socket_path
        .map(Path::to_path_buf)
        .unwrap_or(default_socket_path()?);
    let response = send_request(&socket_path, &DaemonRequest::Sync { reason })?;
    let outcome = match response.message.as_str() {
        "sync already queued" => ControlOutcome::SyncAlreadyQueued(response.status),
        "sync already running" => ControlOutcome::SyncAlreadyRunning(response.status),
        _ => ControlOutcome::SyncQueued(response.status),
    };
    Ok(outcome)
}

#[cfg(not(unix))]
pub fn request_stop(_socket_path: Option<&Path>) -> Result<ControlOutcome> {
    bail!("aicx-memex daemon requires Unix domain sockets")
}

#[cfg(unix)]
pub fn request_stop(socket_path: Option<&Path>) -> Result<ControlOutcome> {
    let socket_path = socket_path
        .map(Path::to_path_buf)
        .unwrap_or(default_socket_path()?);
    let response = send_request(&socket_path, &DaemonRequest::Stop)?;
    Ok(ControlOutcome::StopQueued(response.status))
}

#[cfg(unix)]
fn send_request(socket_path: &Path, request: &DaemonRequest) -> Result<DaemonResponse> {
    let mut stream = UnixStream::connect(socket_path)
        .with_context(|| format!("Failed to connect to {}", socket_path.display()))?;
    stream
        .set_read_timeout(Some(CONTROL_REQUEST_TIMEOUT))
        .context("Failed to set daemon read timeout")?;
    stream
        .set_write_timeout(Some(CONTROL_REQUEST_TIMEOUT))
        .context("Failed to set daemon write timeout")?;
    writeln!(stream, "{}", serde_json::to_string(request)?)
        .context("Failed to write daemon request")?;
    stream.flush().context("Failed to flush daemon request")?;

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .context("Failed to read daemon response")?;
    serde_json::from_str(line.trim()).context("Failed to parse daemon response JSON")
}

pub fn load_last_known_status(socket_path: Option<&Path>) -> Result<Option<DaemonStatusSnapshot>> {
    let status_path = match socket_path {
        Some(path) => path.with_extension("status.json"),
        None => default_status_path()?,
    };
    if !status_path.exists() {
        return Ok(None);
    }
    let raw = fs::read_to_string(&status_path)
        .with_context(|| format!("Failed to read {}", status_path.display()))?;
    let snapshot = serde_json::from_str(&raw)
        .with_context(|| format!("Failed to parse {}", status_path.display()))?;
    Ok(Some(snapshot))
}

pub fn ensure_running_and_kick(reason: Option<String>) -> Result<ControlOutcome> {
    let request_reason = reason.unwrap_or_else(|| "background refresh".to_string());

    match request_sync(None, Some(request_reason.clone())) {
        Ok(outcome) => Ok(outcome),
        Err(err) if is_control_plane_timeout(&err) => {
            if let Some(snapshot) = load_last_known_status(None)? {
                Ok(ControlOutcome::SyncAlreadyRunning(snapshot))
            } else {
                Err(err)
            }
        }
        Err(err) => {
            let config = DaemonConfig::default();
            spawn_detached(&config)?;

            if config.bootstrap {
                let socket_path = socket_path_for(&config)?;
                let mut snapshot = load_last_known_status(Some(&socket_path))?
                    .unwrap_or_else(|| DaemonStatusSnapshot::new(&config, &socket_path));
                if snapshot.last_error.is_none() {
                    snapshot.last_error = Some(format!("{err:#}"));
                }
                Ok(ControlOutcome::SyncQueued(snapshot))
            } else {
                request_sync(
                    None,
                    Some(format!("{request_reason} (after autostart): {err:#}")),
                )
            }
        }
    }
}

pub fn is_control_plane_timeout(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<std::io::Error>().is_some_and(|io| {
            matches!(
                io.kind(),
                std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
            )
        })
    })
}

pub fn ensure_running_and_kick_default() -> Result<ControlOutcome> {
    ensure_running_and_kick(Some("mcp search refresh".to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_socket_path(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time should be after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!(
            "aicx-daemon-{label}-{}-{nanos}.sock",
            std::process::id()
        ))
    }

    #[test]
    fn summarize_command_output_prefers_recent_non_empty_lines() {
        let summary = summarize_command_output("\nalpha\n\nbeta\ngamma\n");
        assert_eq!(summary, "alpha | beta | gamma");
    }

    #[test]
    fn format_memex_progress_tracks_completed_detail() {
        let detail = format_memex_progress(&SyncProgress {
            phase: SyncProgressPhase::Completed,
            done: 3,
            total: 3,
            detail: "Completed: 1 pushed, 2 skipped, 0 ignored".to_string(),
        });
        assert_eq!(detail, "Completed: 1 pushed, 2 skipped, 0 ignored");
    }

    #[test]
    fn sync_with_auto_repair_retries_once_after_repairable_failure() {
        let mut attempts = 0;
        let mut before_retry_calls = 0;
        let mut reset_calls = 0;

        let (result, repaired) = sync_with_auto_repair(
            || {
                attempts += 1;
                if attempts == 1 {
                    Err(anyhow!("compatibility drift"))
                } else {
                    Ok(memex::SyncResult {
                        chunks_pushed: 4,
                        chunks_skipped: 2,
                        chunks_ignored: 1,
                        errors: vec![],
                    })
                }
            },
            || {
                before_retry_calls += 1;
                Ok(())
            },
            || {
                reset_calls += 1;
                Ok(())
            },
            |err| err.to_string().contains("compatibility drift"),
        )
        .expect("repairable sync should recover");

        assert!(repaired);
        assert_eq!(attempts, 2);
        assert_eq!(before_retry_calls, 1);
        assert_eq!(reset_calls, 1);
        assert_eq!(result.chunks_pushed, 4);
        assert_eq!(result.chunks_skipped, 2);
        assert_eq!(result.chunks_ignored, 1);
    }

    #[test]
    fn sync_with_auto_repair_leaves_non_repairable_failure_alone() {
        let mut attempts = 0;
        let mut before_retry_calls = 0;
        let mut reset_calls = 0;

        let err = sync_with_auto_repair(
            || {
                attempts += 1;
                Err(anyhow!("network exploded"))
            },
            || {
                before_retry_calls += 1;
                Ok(())
            },
            || {
                reset_calls += 1;
                Ok(())
            },
            |repair_err| repair_err.to_string().contains("compatibility drift"),
        )
        .expect_err("non-repairable sync should bubble up");

        assert_eq!(err.to_string(), "network exploded");
        assert_eq!(attempts, 1);
        assert_eq!(before_retry_calls, 0);
        assert_eq!(reset_calls, 0);
    }

    #[cfg(unix)]
    #[test]
    fn daemon_control_plane_serves_status_and_stop() {
        let socket_path = unique_socket_path("control");
        let status_path = socket_path.with_extension("status.json");

        let config = DaemonConfig {
            socket_path: Some(socket_path.clone()),
            poll_seconds: 3600,
            refresh_hours: 24,
            projects: vec!["ai-contexters".to_string()],
            namespace: "ai-contexts".to_string(),
            db_path: None,
            per_chunk: false,
            bootstrap: false,
        };

        let handle = thread::spawn(move || run_foreground(config));

        let start = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            if socket_path.exists() && request_status_at(&socket_path).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        let status = request_status_at(&socket_path).expect("daemon status should work");
        let snapshot = status.snapshot().clone();
        assert_eq!(snapshot.phase, DaemonPhase::Idle);
        assert_eq!(snapshot.projects, vec!["ai-contexters".to_string()]);

        let stop = request_stop(Some(&socket_path)).expect("daemon stop should work");
        assert_eq!(stop.message(), "stop queued");

        handle
            .join()
            .expect("daemon thread should join")
            .expect("daemon should shut down cleanly");

        assert!(
            status_path.exists(),
            "daemon status snapshot should be persisted"
        );
        let persisted = load_last_known_status(Some(&socket_path))
            .expect("read status")
            .expect("status exists");
        assert_eq!(persisted.pid, snapshot.pid);
    }

    #[cfg(unix)]
    #[test]
    fn daemon_sync_is_queueable_immediately_after_no_bootstrap_start() {
        let socket_path = unique_socket_path("queueable");
        let status_path = socket_path.with_extension("status.json");

        let config = DaemonConfig {
            socket_path: Some(socket_path.clone()),
            poll_seconds: 3600,
            refresh_hours: 24,
            projects: Vec::new(),
            namespace: "ai-contexts".to_string(),
            db_path: None,
            per_chunk: false,
            bootstrap: false,
        };

        let mut snapshot = DaemonStatusSnapshot::new(&config, &socket_path);
        snapshot.phase = DaemonPhase::Idle;
        snapshot.phase_detail = "Idle".to_string();
        let shared = Arc::new(Mutex::new(SharedState {
            snapshot,
            pending_sync: None,
            stop_requested: false,
        }));

        let (mut client, server) =
            std::os::unix::net::UnixStream::pair().expect("unix stream pair");
        writeln!(
            client,
            "{}",
            serde_json::to_string(&DaemonRequest::Sync {
                reason: Some("test queue".to_string()),
            })
            .expect("request json")
        )
        .expect("write sync request");
        client.flush().expect("flush sync request");

        handle_client(server, &shared, &status_path).expect("handle sync request");

        let mut reader = BufReader::new(client);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read daemon response");
        let response: DaemonResponse =
            serde_json::from_str(line.trim()).expect("parse daemon response");

        assert!(response.ok);
        assert_eq!(response.message, "sync queued");
        let guard = shared.lock().expect("shared state lock");
        assert_eq!(guard.pending_sync.as_deref(), Some("test queue"));
    }
}
