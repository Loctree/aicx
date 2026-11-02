//! First `aicx` after install opens the dashboard on 127.0.0.1:8044.
//!
//! Onboarding finishes by writing the existing stores: `intent_phrases.toml`,
//! `[embedder]` in `config.toml`, the native loopback service, the reindex
//! schedule, and the `aicx` entry of each MCP client the person selected. It
//! does not run from npm. What's new is the current CHANGELOG section, the same
//! body `make release-prepare` writes to `dist/release-notes.md`.
//!
//! [`inspect_onboarding_readiness`] reads those stores back from this machine,
//! and [`discover_phrase_suggestions`] reads what this person actually typed.
//! Neither one invents a query, a session, or a default it cannot show.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashMap};
use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime};

use crate::session_catalog::AgentKind;

/// Bound for waiting until 127.0.0.1 accepts before opening the browser.
const LOOPBACK_READY_BUDGET: Duration = Duration::from_secs(5);

/// Carried inside the binary. npm install does not run this; a bare `aicx` does.
const EMBEDDED_MCP_SERVICE_INSTALLER: &str = include_str!("../tools/install-mcp-service.sh");
const EMBEDDED_LINUX_SERVICE_INSTALLER: &str =
    include_str!("../tools/install-mcp-service-linux.sh");
const EMBEDDED_WINDOWS_SERVICE_INSTALLER: &str = include_str!("../tools/install-mcp-service.ps1");
/// `tools/install-mcp-service.sh` finds its sibling through `$(dirname "$0")`,
/// which is the working directory under `bash -s`. The schedule has to travel
/// on its own for a binary installed without the `tools/` tree.
const EMBEDDED_REINDEX_SCHEDULE_INSTALLER: &str =
    include_str!("../tools/install-reindex-schedule.sh");
const CHANGELOG: &str = include_str!("../CHANGELOG.md");

pub const DASHBOARD_URL: &str = "http://127.0.0.1:8044/";
const SERVICE_ERROR_REL: &str = "state/service-install-error";
const WHATS_NEW_OFFERED_REL: &str = "state/whats-new-offered";
const WHATS_NEW_ACKED_REL: &str = "state/whats-new-acked";
const INDEX_ERROR_REL: &str = "state/onboarding-index-error";
const REINDEX_UNIT_REL: &str = "Library/LaunchAgents/com.loctree.aicx.reindex.plist";

/// Same hot window `aicx index` refreshes before it parses.
const INDEX_CENSUS_WINDOW: Duration = Duration::from_secs(48 * 60 * 60);

const DEFAULT_CLOUD_URL: &str = "https://api.openai.com/v1/embeddings";
const DEFAULT_CLOUD_MODEL: &str = "text-embedding-3-small";
const DEFAULT_CLOUD_API_KEY_ENV: &str = "OPENAI_API_KEY";

/// Readiness counts directory entries without `stat`, so it stays cheap on a
/// corpus of 85 000 files. Past either cap the row says `session_files_truncated`.
const SOURCE_FILE_CAP: usize = 2_000;
const SOURCE_ENTRY_CAP: usize = 20_000;
const SOURCE_WALK_DEPTH: usize = 8;

const SUGGESTION_SESSION_CAP: usize = 30;
const SUGGESTION_UTTERANCE_CAP: usize = 400;
const SUGGESTION_BUDGET: Duration = Duration::from_millis(150);
/// Enough for the opening turns of a session and small enough to parse inside
/// the budget. Active sessions run to many megabytes; their head still parses.
const SUGGESTION_HEAD_BYTES: u64 = 256 * 1024;
const RECENT_WALK_DEPTH: usize = 5;
const RECENT_DIR_FANOUT: usize = 3;
const RECENT_DIR_ENTRY_CAP: usize = 256;
/// A longer message is a worker prompt or a paste, not how this person talks.
const UTTERANCE_MESSAGE_BYTE_CAP: usize = 4_000;
const UTTERANCE_MESSAGE_LINE_CAP: usize = 12;
const UTTERANCE_LINE_CHAR_CAP: usize = 400;
const MATCHED_SAMPLE_CAP: usize = 16;
const MATCHED_SAMPLES_PER_TYPE: usize = 4;
const CANDIDATE_OPENER_CAP: usize = 24;
const EXCERPT_CHAR_CAP: usize = 160;
const PREVIEW_LINE_CAP: usize = 200;
const PREVIEW_LINE_CHAR_CAP: usize = 2_000;
const PHRASE_CHAR_CAP: usize = 120;
const INDEX_ERROR_CHAR_CAP: usize = 500;

/// `[section].key` for each phrase list onboarding reads and extends.
const PHRASE_CATEGORIES: [(&str, &str); 5] = [
    ("intent", "keywords"),
    ("task", "action_heads"),
    ("decision", "policy"),
    ("requirement", "markers"),
    ("question", "markers"),
];

/// The clients onboarding may wire. Paths come only from this table, never from
/// a request.
#[derive(Debug, Clone, Copy)]
struct McpClientSpec {
    id: &'static str,
    label: &'static str,
    config_rel: &'static str,
    /// The key this client reads its endpoint from.
    url_key: &'static str,
    /// Keys that already count as wired when they hold our endpoint.
    accepted_url_keys: &'static [&'static str],
}

static MCP_CLIENTS: [McpClientSpec; 4] = [
    McpClientSpec {
        id: "claude",
        label: "Claude Code",
        config_rel: ".claude/settings.json",
        url_key: "url",
        accepted_url_keys: &["url"],
    },
    McpClientSpec {
        id: "codex",
        label: "OpenAI Codex",
        config_rel: ".codex/settings.json",
        url_key: "url",
        accepted_url_keys: &["url"],
    },
    McpClientSpec {
        id: "gemini",
        label: "Gemini CLI",
        config_rel: ".gemini/settings.json",
        url_key: "url",
        accepted_url_keys: &["url", "httpUrl"],
    },
    // Antigravity keeps HTTP servers as `{ "serverUrl", "disabled" }` on disk.
    McpClientSpec {
        id: "agy",
        label: "Antigravity (AGY)",
        config_rel: ".gemini/config/mcp_config.json",
        url_key: "serverUrl",
        accepted_url_keys: &["serverUrl"],
    },
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FirstStart {
    Opened(FirstStartReport),
    AlreadyConfigured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstStartReport {
    pub dashboard_url: String,
    pub service: String,
}

impl FirstStartReport {
    pub fn render(&self) -> String {
        format!(
            "aicx onboarding\nOpen the dashboard: {url}\nType the phrases you use. Save stores them on this machine and installs the background service when it is missing.\nService: {service}\nSearch still works if that install fails: aicx search '<query>'\n",
            url = self.dashboard_url,
            service = self.service,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceInstall {
    /// macOS LaunchAgent. This variant is launchd, and only launchd.
    Installed,
    /// Linux systemd or Windows service. Not a LaunchAgent.
    NativeInstalled,
    Skipped {
        reason: String,
    },
    Failed {
        reason: String,
    },
}

impl ServiceInstall {
    pub fn summary(&self) -> String {
        match self {
            Self::Installed => "launchd LaunchAgent installed (com.loctree.aicx.mcp)".to_string(),
            Self::NativeInstalled => {
                format!("native service installed on {}", std::env::consts::OS)
            }
            Self::Skipped { reason } => format!("skipped: {reason}"),
            Self::Failed { reason } => format!("failed: {reason}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct InstallOptions {
    pub dry_run: bool,
    pub platform_is_macos: bool,
    pub script: Option<PathBuf>,
    pub bin: Option<PathBuf>,
    pub port: u16,
}

impl InstallOptions {
    pub fn from_env() -> Self {
        Self {
            dry_run: env_flag("AICX_ONBOARDING_DRY_RUN"),
            platform_is_macos: cfg!(target_os = "macos"),
            script: locate_service_installer(),
            bin: std::env::current_exe().ok(),
            port: std::env::var("AICX_MCP_PORT")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(8044),
        }
    }
}

/// ProgramArguments the loopback HTTP service must run (LaunchAgent / `aicx serve`).
///
/// Host is always `127.0.0.1` (never `0.0.0.0`). Auth is off on loopback.
/// Index maintenance remains separate; the compatibility flag makes the
/// reader-only ownership explicit even against older binaries.
pub fn loopback_http_argv(port: u16) -> Vec<String> {
    vec![
        "--transport".into(),
        "http".into(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--no-require-auth".into(),
        "--no-auto-refresh".into(),
    ]
}

/// How production invokes `tools/install-mcp-service.sh` without spawning it.
///
/// When `InstallOptions.script` points at a real file, argv is that path under
/// `bash`. Otherwise stdin carries the embedded copy (`bash -s`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallerInvocation {
    pub program: &'static str,
    pub args: Vec<std::ffi::OsString>,
    pub stdin_embedded: bool,
    pub port: u16,
}

/// Resolve the macOS service installer command the survey save would run.
pub fn resolve_macos_installer_invocation(options: &InstallOptions) -> InstallerInvocation {
    if let Some(script_path) = options.script.as_ref().filter(|path| path.is_file()) {
        return InstallerInvocation {
            program: "bash",
            args: vec![script_path.as_os_str().to_os_string()],
            stdin_embedded: false,
            port: options.port,
        };
    }
    InstallerInvocation {
        program: "bash",
        args: vec![std::ffi::OsString::from("-s")],
        stdin_embedded: true,
        port: options.port,
    }
}

pub fn package_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn phrases_missing(home: &Path) -> bool {
    !home.join("intent_phrases.toml").is_file()
}

pub fn embedder_backend(home: &Path) -> Option<String> {
    let path = home.join("config.toml");
    let raw = std::fs::read_to_string(path).ok()?;
    let value: toml::Value = toml::from_str(&raw).ok()?;
    value
        .get("embedder")
        .and_then(|embedder| embedder.get("backend"))
        .and_then(|backend| backend.as_str())
        .map(str::trim)
        .filter(|backend| matches!(*backend, "gguf" | "auto" | "cloud"))
        .map(str::to_string)
}

/// Persist the embedder choice in the existing `config.toml`.
///
/// Local GGUF is the preselected backend. A cloud URL is optional and an API
/// key is not required to finish.
pub fn write_embedder_choice(home: &Path, backend: &str, cloud_url: Option<&str>) -> Result<()> {
    if !matches!(backend, "gguf" | "auto" | "cloud") {
        anyhow::bail!("embedder backend must be gguf, auto, or cloud");
    }
    // Basename is a fixed constant under operator-owned AICX_HOME (not request input).
    let path = home.join("config.toml");
    anyhow::ensure!(
        path.file_name().and_then(|name| name.to_str()) == Some("config.toml"),
        "refusing unexpected config path"
    );
    let mut value: toml::Value = if path.is_file() {
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let raw = std::fs::read_to_string(&path)?;
        toml::from_str(&raw)?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let root = value
        .as_table_mut()
        .context("config.toml must be a table")?;
    let embedder = root
        .entry("embedder")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
    let embedder = embedder
        .as_table_mut()
        .context("[embedder] must be a table")?;
    embedder.insert(
        "backend".to_string(),
        toml::Value::String(backend.to_string()),
    );
    if backend != "cloud" {
        embedder
            .entry("profile")
            .or_insert_with(|| toml::Value::String("base".to_string()));
    }
    if backend == "cloud"
        && let Some(url) = cloud_url.map(str::trim).filter(|url| !url.is_empty())
    {
        let cloud = embedder
            .entry("cloud")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()));
        if let Some(cloud) = cloud.as_table_mut() {
            cloud.insert("url".to_string(), toml::Value::String(url.to_string()));
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, toml::to_string_pretty(&value)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

pub fn service_definition_meets_contract(text: &str) -> bool {
    let has = |needle: &str| text.contains(needle);
    has("--transport")
        && has("http")
        && has("--host")
        && has("127.0.0.1")
        && has("--port")
        && has("--no-require-auth")
        && !has("--experimental-auto-refresh")
        && !text.contains("--transport\n    <string>stdio")
        && !text.contains("--transport stdio")
        && !text.contains("\"stdio\"")
}

pub fn service_error(home: &Path) -> Option<String> {
    let text = std::fs::read_to_string(home.join(SERVICE_ERROR_REL)).ok()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

fn write_service_error(home: &Path, error: Option<&str>) {
    let path = home.join(SERVICE_ERROR_REL);
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    match error.map(str::trim).filter(|text| !text.is_empty()) {
        Some(text) => {
            let _ = std::fs::write(path, format!("{text}\n"));
        }
        None => {
            let _ = std::fs::remove_file(path);
        }
    }
}

pub fn service_meets_contract(home: &Path) -> bool {
    if service_error(home).is_some() {
        return false;
    }
    let Some(text) = read_service_definition() else {
        return false;
    };
    let _ = home;
    service_definition_meets_contract(&text)
}

pub fn survey_required(phrases_missing: bool, embedder: Option<&str>, service_ok: bool) -> bool {
    phrases_missing || embedder.is_none() || !service_ok
}

pub fn needs_full_survey(home: &Path) -> bool {
    survey_required(
        phrases_missing(home),
        embedder_backend(home).as_deref(),
        service_meets_contract(home),
    )
}

fn read_service_definition() -> Option<String> {
    if let Some(path) = std::env::var_os("AICX_SERVICE_UNIT") {
        return std::fs::read_to_string(path).ok();
    }
    let path = match std::env::consts::OS {
        "macos" => user_home()?.join("Library/LaunchAgents/com.loctree.aicx.mcp.plist"),
        "linux" => systemd_user_dir()?.join("aicx-mcp.service"),
        "windows" => user_home()?.join("AppData/Local/aicx/aicx-mcp-service.xml"),
        _ => return None,
    };
    let text = std::fs::read_to_string(path).ok()?;
    // Legacy SCM records described a console app that cannot run as a service.
    if cfg!(target_os = "windows") && !text.contains("<Task") {
        return None;
    }
    Some(text)
}

fn user_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

fn systemd_user_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| user_home().map(|home| home.join(".config")))?;
    Some(base.join("systemd/user"))
}

pub fn release_notes_body(changelog: &str, version: &str) -> String {
    let header = format!("## [{version}]");
    let Some(start) = changelog.find(&header) else {
        return "No detailed release notes were recorded for this version.".to_string();
    };
    let after = &changelog[start + header.len()..];
    let body = after
        .find("\n## [")
        .map(|index| &after[..index])
        .unwrap_or(after);
    let body = body
        .trim()
        .trim_start_matches(|ch: char| ch == '-' || ch.is_whitespace());
    let body = body.trim();
    if body.is_empty() {
        "No detailed release notes were recorded for this version.".to_string()
    } else {
        body.to_string()
    }
}

pub fn current_release_notes() -> String {
    if let Some(path) = std::env::var_os("AICX_RELEASE_NOTES")
        && let Ok(text) = std::fs::read_to_string(path)
    {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    release_notes_body(CHANGELOG, package_version())
}

pub fn whats_new_acked(home: &Path) -> bool {
    std::fs::read_to_string(home.join(WHATS_NEW_ACKED_REL))
        .ok()
        .is_some_and(|text| text.trim() == package_version())
}

pub fn ack_whats_new(home: &Path) -> Result<()> {
    write_state_line(home, WHATS_NEW_ACKED_REL, package_version())
}

fn whats_new_offered(home: &Path) -> bool {
    std::fs::read_to_string(home.join(WHATS_NEW_OFFERED_REL))
        .ok()
        .is_some_and(|text| text.trim() == package_version())
}

fn offer_whats_new(home: &Path) -> Result<()> {
    write_state_line(home, WHATS_NEW_OFFERED_REL, package_version())
}

pub const MARKER_REL: &str = "state/first-run-complete";

pub fn needs_onboarding(home: &Path) -> bool {
    !home.join(MARKER_REL).is_file() && !home.join("config.toml").is_file()
}

pub fn mark_complete(home: &Path) -> Result<()> {
    write_state_line(home, MARKER_REL, "first-run-complete")
}

fn write_state_line(home: &Path, rel: &str, line: &str) -> Result<()> {
    let path = home.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, format!("{line}\n"))?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortListener {
    pub pid: u32,
    pub command: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForeignListener {
    pub pid: u32,
    pub command: String,
    pub occupied_port: u16,
    pub alternate_port: u16,
}

impl ForeignListener {
    pub fn instructions(&self) -> String {
        format!(
            "port {occupied} is used by {command} (pid {pid}). AICX left that process running.\n\nFree {occupied} yourself:\n  kill {pid}\n\nInstall the AICX service on {alt} instead. The dashboard and MCP share that port:\n  AICX_MCP_PORT={alt} aicx\n",
            occupied = self.occupied_port,
            command = self.command,
            pid = self.pid,
            alt = self.alternate_port,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortPlan {
    pub port: u16,
    pub replaced_own_service: bool,
    pub foreign: Option<ForeignListener>,
}

impl PortPlan {
    fn keep(port: u16) -> Self {
        Self {
            port,
            replaced_own_service: false,
            foreign: None,
        }
    }
}

pub fn is_our_aicx_process(command: &str) -> bool {
    let lower = command.to_ascii_lowercase();
    if lower.contains("aicx-mcp") || lower.contains("com.loctree.aicx.mcp") {
        return true;
    }
    command.split_whitespace().any(|word| {
        let word = word.trim_matches(|ch: char| matches!(ch, '"' | '\'' | ','));
        word == "aicx"
            || word.ends_with("/aicx")
            || word.ends_with("\\aicx")
            || word.ends_with("/aicx-mcp")
            || word.ends_with("\\aicx-mcp")
    })
}

pub fn choose_service_port(listeners: &[PortListener], preferred: u16, busy: &[u16]) -> PortPlan {
    if listeners.is_empty()
        || listeners
            .iter()
            .all(|listener| is_our_aicx_process(&listener.command))
    {
        return PortPlan {
            port: preferred,
            replaced_own_service: !listeners.is_empty(),
            foreign: None,
        };
    }
    let foreign = listeners
        .iter()
        .find(|listener| !is_our_aicx_process(&listener.command))
        .expect("foreign listener");
    let alternate = (preferred.saturating_add(1)..)
        .take(64)
        .find(|port| *port != preferred && !busy.contains(port))
        .unwrap_or(preferred.saturating_add(1));
    PortPlan {
        port: alternate,
        replaced_own_service: false,
        foreign: Some(ForeignListener {
            pid: foreign.pid,
            command: foreign.command.clone(),
            occupied_port: preferred,
            alternate_port: alternate,
        }),
    }
}

fn resolve_service_port(preferred: u16) -> PortPlan {
    let listeners = listeners_on_port(preferred);
    let mut busy = Vec::new();
    if listeners
        .iter()
        .any(|listener| !is_our_aicx_process(&listener.command))
    {
        let mut candidate = preferred.saturating_add(1);
        for _ in 0..64 {
            if listeners_on_port(candidate).is_empty() {
                break;
            }
            busy.push(candidate);
            candidate = candidate.saturating_add(1);
        }
    }
    choose_service_port(&listeners, preferred, &busy)
}

fn listens_on_port(name: &str, port: u16) -> bool {
    let local = name.split("->").next().unwrap_or(name);
    local.ends_with(&format!(":{port}"))
}

fn listeners_on_port(port: u16) -> Vec<PortListener> {
    let output = Command::new("lsof")
        .args(["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpcn"])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let mut listeners = Vec::new();
    let mut pid = 0u32;
    let mut command = String::new();
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix('p') {
            pid = rest.parse().unwrap_or(0);
            command.clear();
        } else if let Some(rest) = line.strip_prefix('c') {
            command = rest.to_string();
        } else if let Some(rest) = line.strip_prefix('n')
            && listens_on_port(rest, port)
            && pid != 0
        {
            let full = process_command(pid).unwrap_or_else(|| command.clone());
            listeners.push(PortListener { pid, command: full });
        }
    }
    listeners
}

fn process_command(pid: u32) -> Option<String> {
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "command="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() { None } else { Some(text) }
}

enum PortOccupancy {
    Free,
    Ours,
    Foreign(PortListener),
}

fn classify_port(port: u16) -> PortOccupancy {
    let listeners = listeners_on_port(port);
    if listeners.is_empty() {
        PortOccupancy::Free
    } else if listeners
        .iter()
        .all(|listener| is_our_aicx_process(&listener.command))
    {
        PortOccupancy::Ours
    } else {
        PortOccupancy::Foreign(
            listeners
                .into_iter()
                .find(|listener| !is_our_aicx_process(&listener.command))
                .expect("foreign listener"),
        )
    }
}

pub fn dashboard_url_for(port: u16) -> String {
    format!("http://127.0.0.1:{port}/")
}

pub fn maybe_first_start() -> Result<FirstStart> {
    let home = crate::aicx_home::resolve()?;
    if !bare_start_opens_dashboard(&home) {
        return Ok(FirstStart::AlreadyConfigured);
    }
    let survey = needs_full_survey(&home);
    let home = crate::aicx_home::ensure()?;
    open_first_start(&home, survey, InstallOptions::from_env())
}

/// Open the dashboard for this run.
///
/// Opening it finishes nothing. The completion marker belongs to the save that
/// finished onboarding, in [`apply_onboarding_request`].
fn open_first_start(home: &Path, survey: bool, mut options: InstallOptions) -> Result<FirstStart> {
    let plan = if options.dry_run {
        PortPlan::keep(options.port)
    } else {
        resolve_service_port(options.port)
    };
    options.port = plan.port;
    if let Some(foreign) = &plan.foreign {
        println!("{}", foreign.instructions());
    }
    let service = if survey {
        bring_service_onto_contract(home, &options)
    } else {
        ServiceInstall::Skipped {
            reason: "service already matches the loopback contract".to_string(),
        }
    };
    let url = dashboard_url_for(plan.port);
    let gui = gui_available();
    let loopback_ready = if options.dry_run {
        false
    } else {
        match ensure_loopback_server(plan.port) {
            Ok(()) => true,
            Err(err) => {
                write_service_error(home, Some(&err.to_string()));
                // Reason first, then the URL below — never open a dead page.
                println!("{err}");
                false
            }
        }
    };
    println!("{url}");
    if should_open_dashboard(gui, options.dry_run, loopback_ready) {
        // A missing browser is not a failure. The URL is already printed.
        let _ = open_dashboard(&url);
    }
    if !survey {
        offer_whats_new(home)?;
    }
    Ok(FirstStart::Opened(FirstStartReport {
        dashboard_url: url,
        service: service.summary(),
    }))
}

/// After this version is configured, bare `aicx` is the short CLI help.
///
/// It does not open a browser and it does not launch `aicx wizard`.
/// The browser opens only for first configuration and for What's new.
pub fn steady_state_opens_browser() -> bool {
    false
}

pub fn bare_start_opens_dashboard(home: &Path) -> bool {
    if needs_full_survey(home) || !whats_new_offered(home) {
        return true;
    }
    steady_state_opens_browser()
}

pub fn dashboard_url() -> String {
    let host = std::env::var("AICX_MCP_HOST").unwrap_or_else(|_| "127.0.0.1".to_string());
    let port = std::env::var("AICX_MCP_PORT").unwrap_or_else(|_| "8044".to_string());
    format!("http://{host}:{port}/")
}

pub fn install_launch_agent(options: &InstallOptions) -> ServiceInstall {
    if options.dry_run {
        return ServiceInstall::Skipped {
            reason: "dry-run; native service installer was not executed".to_string(),
        };
    }
    match std::env::consts::OS {
        "macos" if options.platform_is_macos => run_embedded_installer(
            "bash",
            &["-s"],
            EMBEDDED_MCP_SERVICE_INSTALLER,
            options,
            true,
        ),
        "linux" if !options.platform_is_macos => run_embedded_installer(
            "bash",
            &["-s"],
            EMBEDDED_LINUX_SERVICE_INSTALLER,
            options,
            true,
        ),
        "windows" if !options.platform_is_macos => run_embedded_installer(
            "powershell",
            &["-NoProfile", "-Command", "-"],
            EMBEDDED_WINDOWS_SERVICE_INSTALLER,
            options,
            true,
        ),
        "macos" => ServiceInstall::Skipped {
            reason: "dry platform override; launchd was not started".to_string(),
        },
        other => ServiceInstall::Failed {
            reason: format!(
                "native service was not installed on {other}. Search still works with `aicx search`."
            ),
        },
    }
}

fn run_embedded_installer(
    program: &str,
    args: &[&str],
    script: &str,
    options: &InstallOptions,
    _use_script_path: bool,
) -> ServiceInstall {
    if let Some(script_path) = options.script.as_ref().filter(|path| path.is_file()) {
        let script = script_path.as_os_str().to_os_string();
        let args = if program == "powershell" {
            vec![
                std::ffi::OsString::from("-NoProfile"),
                std::ffi::OsString::from("-File"),
                script,
            ]
        } else {
            vec![script]
        };
        return spawn_installer(program, &args, options, None);
    }
    let os_args: Vec<std::ffi::OsString> = args.iter().map(std::ffi::OsString::from).collect();
    spawn_installer(program, &os_args, options, Some(script))
}

fn spawn_installer(
    program: &str,
    args: &[std::ffi::OsString],
    options: &InstallOptions,
    stdin_body: Option<&str>,
) -> ServiceInstall {
    let mut command = Command::new(program);
    command
        .args(args)
        .env("AICX_SKIP_MCP_CLIENTS", "1")
        .env("AICX_MCP_PORT", options.port.to_string())
        .env_remove("AICX_MCP_HOST")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(bin) = options.bin.as_ref() {
        command.env("AICX_BIN", bin);
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(err) => {
            return ServiceInstall::Failed {
                reason: format!("could not run the native service installer: {err}"),
            };
        }
    };
    if let Some(body) = stdin_body
        && let Some(mut stdin) = child.stdin.take()
        && let Err(err) = stdin.write_all(body.as_bytes())
    {
        return ServiceInstall::Failed {
            reason: format!("could not send the service installer: {err}"),
        };
    }
    let output = match child.wait_with_output() {
        Ok(output) => output,
        Err(err) => {
            return ServiceInstall::Failed {
                reason: format!("native service installer did not finish: {err}"),
            };
        }
    };
    if output.status.success() {
        // `Installed` is the LaunchAgent claim. A zero exit on Linux or
        // Windows is the native service, not launchd.
        return if std::env::consts::OS == "macos" {
            ServiceInstall::Installed
        } else {
            ServiceInstall::NativeInstalled
        };
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let detail = format!("{stdout}{stderr}").trim().to_string();
    ServiceInstall::Failed {
        reason: if detail.is_empty() {
            format!("native service installer exited {}", output.status)
        } else {
            detail
        },
    }
}

fn gui_available() -> bool {
    if env_flag("AICX_ONBOARDING_NO_OPEN") || std::env::var_os("CI").is_some() {
        return false;
    }
    if std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some() {
        return false;
    }
    match std::env::consts::OS {
        "linux" => {
            std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some()
        }
        "macos" | "windows" => true,
        _ => false,
    }
}

fn ensure_loopback_server(port: u16) -> Result<()> {
    match classify_port(port) {
        PortOccupancy::Free => {
            let bin = std::env::current_exe().context("cannot resolve aicx binary")?;
            let log_dir = crate::aicx_home::resolve()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join("logs");
            std::fs::create_dir_all(&log_dir).ok();
            let log = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(log_dir.join("aicx-serve-http.log"))
                .ok();
            let mut serve_args = vec!["serve".to_string()];
            serve_args.extend(loopback_http_argv(port));
            let mut command = Command::new(bin);
            command
                .args(&serve_args)
                .stdin(Stdio::null())
                .stdout(Stdio::null());
            if let Some(log) = log {
                command.stderr(log);
            } else {
                command.stderr(Stdio::null());
            }
            command
                .spawn()
                .context("could not start the loopback dashboard on 127.0.0.1:8044")?;
        }
        PortOccupancy::Ours => {
            // Launchd (or a prior spawn) already owns the port claim — still wait
            // until TCP accept works before the browser is allowed to open.
        }
        PortOccupancy::Foreign(listener) => {
            let plan = choose_service_port(std::slice::from_ref(&listener), port, &[]);
            let Some(foreign) = plan.foreign else {
                anyhow::bail!("port {port} is already taken");
            };
            anyhow::bail!("{}", foreign.instructions());
        }
    }
    wait_until_loopback_accepts(port, LOOPBACK_READY_BUDGET)
        .with_context(|| format!("timed out waiting for 127.0.0.1:{port} to accept connections"))?;
    Ok(())
}

/// Poll until something on 127.0.0.1:`port` accepts a TCP connect, or `budget` elapses.
fn wait_until_loopback_accepts(port: u16, budget: Duration) -> Result<()> {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let deadline = Instant::now() + budget;
    let mut last_err = None;
    while Instant::now() < deadline {
        match TcpStream::connect_timeout(&addr, Duration::from_millis(100)) {
            Ok(_) => return Ok(()),
            Err(err) => last_err = Some(err),
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    match last_err {
        Some(err) => Err(err).context(format!("127.0.0.1:{port} did not accept within {budget:?}")),
        None => anyhow::bail!("127.0.0.1:{port} did not accept within {budget:?}"),
    }
}

/// Open the OS browser only when GUI is available, not dry-run, and loopback is ready.
fn should_open_dashboard(gui: bool, dry_run: bool, loopback_ready: bool) -> bool {
    gui && !dry_run && loopback_ready
}

/// Install the existing native service unit when this machine is off-contract.
///
/// Uses `tools/install-mcp-service*.sh` (embedded or `InstallOptions.script`).
/// Does not invent a second installer.
pub fn bring_service_onto_contract(home: &Path, options: &InstallOptions) -> ServiceInstall {
    if service_meets_contract(home) {
        return ServiceInstall::Skipped {
            reason: "service already matches the loopback contract".to_string(),
        };
    }
    let outcome = install_launch_agent(options);
    match &outcome {
        ServiceInstall::Failed { reason } => write_service_error(home, Some(reason)),
        ServiceInstall::Installed | ServiceInstall::NativeInstalled => {
            write_service_error(home, None);
        }
        ServiceInstall::Skipped { .. } => {}
    }
    outcome
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OnboardingApply {
    pub phrases_path: PathBuf,
    pub service: ServiceInstall,
}

/// Write survey phrases into the existing phrase file, set the local embedder
/// when none is configured, then install the native service when the unit is
/// missing or off-contract. A failed install leaves the survey required.
pub fn apply_onboarding_survey(
    home: &Path,
    current: &str,
    extra: &[String],
    options: &InstallOptions,
) -> Result<OnboardingApply, String> {
    let rendered = merge_intent_keywords(current, extra)?;
    crate::parser::intent_phrases::reload_from_str(&rendered)?;
    let phrases_path = home.join("intent_phrases.toml");
    if let Some(parent) = phrases_path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| format!("write:{err}"))?;
    }
    std::fs::write(&phrases_path, rendered).map_err(|err| format!("write:{err}"))?;
    if embedder_backend(home).is_none() {
        write_embedder_choice(home, "gguf", None).map_err(|err| format!("write:{err}"))?;
    }
    let service = bring_service_onto_contract(home, options);
    if let ServiceInstall::Failed { reason } = &service {
        return Err(format!("install:{reason}"));
    }
    Ok(OnboardingApply {
        phrases_path,
        service,
    })
}

/// Append operator phrases to `[intent].keywords` in the existing phrase file.
///
/// This is the onboarding survey store. It does not create a second database.
pub fn merge_intent_keywords(current: &str, extra: &[String]) -> Result<String, String> {
    let mut value: toml::Value = toml::from_str(current).map_err(|err| err.to_string())?;
    let keywords = value
        .get_mut("intent")
        .and_then(|intent| intent.get_mut("keywords"))
        .and_then(|keywords| keywords.as_array_mut())
        .ok_or_else(|| "intent.keywords is missing from intent_phrases.toml".to_string())?;
    let mut added = 0usize;
    for phrase in extra {
        let phrase = phrase.trim();
        if phrase.is_empty() {
            continue;
        }
        let already = keywords.iter().any(|item| item.as_str() == Some(phrase));
        if !already {
            keywords.push(toml::Value::String(phrase.to_string()));
            added += 1;
        }
    }
    if added == 0 {
        return Err("add at least one new intent phrase".to_string());
    }
    toml::to_string_pretty(&value).map_err(|err| err.to_string())
}

// ── Embedder choice ─────────────────────────────────────────────────

/// What the person chose for `[embedder]`.
///
/// `profile` stays a string so this writer accepts the contract's names without
/// depending on the enum that reads them back.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct EmbedderOnboardingSelection {
    pub backend: String,
    pub profile: String,
    pub cloud_url: String,
    pub cloud_model: String,
    pub cloud_api_key_env: String,
}

/// One spelling reaches `config.toml`, whichever name the page sent.
fn canonical_embedder_profile(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "base" | "0.6b" => Some("base"),
        "dev" | "1.7b" => Some("dev"),
        "premium" | "1.7b-q6" => Some("premium"),
        "max" | "4b" => Some("max"),
        _ => None,
    }
}

/// A key pasted into the URL would land in a plain config file.
fn valid_cloud_url(url: &str) -> bool {
    let Some(rest) = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
    else {
        return false;
    };
    !rest.is_empty()
        && !rest.starts_with('/')
        && !url.contains(['@', '?', '#'])
        && !url.chars().any(|ch| ch.is_whitespace() || ch.is_control())
}

/// A key pasted into the variable name is refused, and never echoed back.
fn valid_env_var_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
        && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

/// Render `config.toml` with this choice merged in. Writes nothing, so a bad
/// selection is refused before the first store changes.
fn render_embedder_config(home: &Path, selection: &EmbedderOnboardingSelection) -> Result<String> {
    let backend = match selection.backend.trim() {
        "" => "gguf",
        backend @ ("gguf" | "auto" | "cloud") => backend,
        _ => anyhow::bail!("embedder backend must be gguf, auto, or cloud"),
    };
    let profile = match selection.profile.trim() {
        "" => None,
        raw => Some(canonical_embedder_profile(raw).context(
            "embedder profile must be base, dev, premium, max, 0.6b, 1.7b, 1.7b-q6, or 4b",
        )?),
    };
    let cloud_url = selection.cloud_url.trim();
    anyhow::ensure!(
        cloud_url.is_empty() || valid_cloud_url(cloud_url),
        "cloud_url must be http(s) with no credentials or query string"
    );
    let cloud_model = selection.cloud_model.trim();
    anyhow::ensure!(
        !cloud_model.chars().any(char::is_control),
        "cloud_model must stay on one line"
    );
    let cloud_api_key_env = selection.cloud_api_key_env.trim();
    anyhow::ensure!(
        cloud_api_key_env.is_empty() || valid_env_var_name(cloud_api_key_env),
        "cloud_api_key_env must name an environment variable, not hold a key"
    );

    // Basename is a fixed constant under operator-owned AICX_HOME (not request input).
    let path = home.join("config.toml");
    let mut value: toml::Value = if path.is_file() {
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path
        let raw = std::fs::read_to_string(&path).context("read config.toml")?;
        toml::from_str(&raw).context("config.toml is not valid TOML; it was left untouched")?
    } else {
        toml::Value::Table(toml::map::Map::new())
    };
    let embedder = value
        .as_table_mut()
        .context("config.toml must be a table")?
        .entry("embedder")
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("[embedder] must be a table")?;
    embedder.insert(
        "backend".to_string(),
        toml::Value::String(backend.to_string()),
    );
    match profile {
        Some(profile) => {
            embedder.insert(
                "profile".to_string(),
                toml::Value::String(profile.to_string()),
            );
        }
        None if backend != "cloud" => {
            embedder
                .entry("profile")
                .or_insert_with(|| toml::Value::String("base".to_string()));
        }
        None => {}
    }
    if backend == "cloud"
        || !cloud_url.is_empty()
        || !cloud_model.is_empty()
        || !cloud_api_key_env.is_empty()
    {
        let cloud = embedder
            .entry("cloud")
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .context("[embedder.cloud] must be a table")?;
        for (key, text) in [
            ("url", cloud_url),
            ("model", cloud_model),
            ("api_key_env", cloud_api_key_env),
        ] {
            if !text.is_empty() {
                cloud.insert(key.to_string(), toml::Value::String(text.to_string()));
            }
        }
        if backend == "cloud" {
            for required in ["url", "model"] {
                anyhow::ensure!(
                    cloud
                        .get(required)
                        .and_then(toml::Value::as_str)
                        .is_some_and(|text| !text.trim().is_empty()),
                    "the cloud embedder needs cloud_url and cloud_model"
                );
            }
        }
    }
    toml::to_string_pretty(&value).context("render config.toml")
}

/// Merge the canonical `[embedder]` choice into `config.toml`. Other sections
/// stay. The API key is named by its environment variable and never stored.
pub fn write_embedder_config(home: &Path, selection: &EmbedderOnboardingSelection) -> Result<()> {
    let rendered = render_embedder_config(home, selection)?;
    let path = home.join("config.toml");
    write_user_file(&path, rendered.as_bytes()).with_context(|| format!("write {}", path.display()))
}

/// Replace a file this person owns without tearing it.
///
/// The temporary copy is owner-only from its first byte, because the file may
/// hold tokens. It goes through a symlink rather than replacing it, and the
/// mode the file had before comes back after the swap.
fn write_user_file(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let target = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let previous = std::fs::metadata(&target)
        .ok()
        .map(|meta| meta.permissions());
    crate::legacy_archive::atomic_write::atomic_write_private(&target, bytes)?;
    if let Some(permissions) = previous {
        std::fs::set_permissions(&target, permissions)?;
    }
    Ok(())
}

// ── Reindex schedule ────────────────────────────────────────────────

fn reindex_unit_path(user_home: &Path) -> PathBuf {
    user_home.join(REINDEX_UNIT_REL)
}

fn same_path(left: &Path, right: &Path) -> bool {
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    canonical(left) == canonical(right)
}

/// The job runs `aicx catalog refresh` and `aicx index`. `aicx-mcp` has neither.
fn reindex_job_binary(options: &InstallOptions) -> Option<PathBuf> {
    let bin = options.bin.as_ref()?;
    if bin.file_stem().and_then(|stem| stem.to_str()) == Some("aicx-mcp") {
        let sibling = bin.with_file_name("aicx");
        return sibling.is_file().then_some(sibling);
    }
    Some(bin.clone())
}

/// Install the background reindex schedule from the copy inside this binary.
pub fn install_reindex_schedule(home: &Path) -> ServiceInstall {
    let Some(user_home) = crate::os_user_home() else {
        return ServiceInstall::Failed {
            reason: "no home directory".to_string(),
        };
    };
    install_reindex_schedule_in(home, &user_home, &InstallOptions::from_env())
}

fn install_reindex_schedule_in(
    home: &Path,
    user_home: &Path,
    options: &InstallOptions,
) -> ServiceInstall {
    if options.dry_run {
        return ServiceInstall::Skipped {
            reason: "dry-run; the reindex schedule was not installed".to_string(),
        };
    }
    if !options.platform_is_macos || std::env::consts::OS != "macos" {
        return ServiceInstall::Skipped {
            reason: "launchd-only; run `aicx index` yourself on this platform".to_string(),
        };
    }
    // launchd starts the job without AICX_HOME, so it indexes the home a bare
    // `aicx` resolves. A schedule that updates a different home is worse than none.
    let scheduled_home = crate::aicx_home::resolve_from(None, user_home).ok();
    if !scheduled_home.is_some_and(|scheduled| same_path(&scheduled, home)) {
        return ServiceInstall::Skipped {
            reason: format!(
                "the schedule would index the default home, not {}",
                home.display()
            ),
        };
    }
    let Some(bin) = reindex_job_binary(options) else {
        return ServiceInstall::Skipped {
            reason: "no `aicx` binary next to this one; save again after install".to_string(),
        };
    };
    let options = InstallOptions {
        bin: Some(bin),
        ..options.clone()
    };
    let args = [std::ffi::OsString::from("-s")];
    match spawn_installer(
        "bash",
        &args,
        &options,
        Some(EMBEDDED_REINDEX_SCHEDULE_INSTALLER),
    ) {
        // The script exits 0 without writing when it cannot resolve the binary.
        ServiceInstall::Installed | ServiceInstall::NativeInstalled
            if !reindex_unit_path(user_home).is_file() =>
        {
            ServiceInstall::Failed {
                reason: "the schedule installer finished without writing its unit".to_string(),
            }
        }
        outcome => outcome,
    }
}

// ── MCP client wiring ───────────────────────────────────────────────

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpWiringReport {
    /// Selected clients whose config now points at this service.
    pub wired: Vec<String>,
    /// Selected clients with no directory on this machine. Nothing was created.
    pub skipped: Vec<String>,
    /// `"<id>: <reason>"`. The client's file is exactly as it was.
    pub failed: Vec<String>,
}

fn client_mcp_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/mcp")
}

fn mcp_entry_is_wired(entry: &serde_json::Value, spec: &McpClientSpec, port: u16) -> bool {
    if entry.get("disabled").and_then(serde_json::Value::as_bool) == Some(true) {
        return false;
    }
    let loopback = client_mcp_url(port);
    let localhost = format!("http://localhost:{port}/mcp");
    spec.accepted_url_keys
        .iter()
        .filter_map(|key| entry.get(*key).and_then(serde_json::Value::as_str))
        .map(|url| url.trim_end_matches('/'))
        .any(|url| url == loopback || url == localhost)
}

fn client_is_wired(path: &Path, spec: &McpClientSpec, port: u16) -> bool {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return false;
    };
    let Ok(root) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return false;
    };
    root.get("mcpServers")
        .and_then(|servers| servers.get("aicx"))
        .is_some_and(|entry| mcp_entry_is_wired(entry, spec, port))
}

/// Point one client at the service. `Ok(false)` means the client is not here.
///
/// A matching entry is left byte-for-byte alone. A file this code cannot read
/// as a JSON object is refused rather than overwritten.
fn wire_one_client(path: &Path, spec: &McpClientSpec, port: u16) -> Result<bool, String> {
    if !path.parent().is_some_and(Path::is_dir) {
        return Ok(false);
    }
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("cannot read {}: {err}", path.display())),
    };
    let mut root = if raw.trim().is_empty() {
        serde_json::Value::Object(serde_json::Map::new())
    } else {
        serde_json::from_str::<serde_json::Value>(&raw)
            .map_err(|_| format!("{} is not valid JSON; left untouched", path.display()))?
    };
    let Some(root_object) = root.as_object_mut() else {
        return Err(format!(
            "{} is not a JSON object; left untouched",
            path.display()
        ));
    };
    let servers = root_object
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    let Some(servers) = servers.as_object_mut() else {
        return Err(format!(
            "mcpServers in {} is not an object; left untouched",
            path.display()
        ));
    };
    let entry = servers
        .entry("aicx")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if mcp_entry_is_wired(entry, spec, port) {
        return Ok(true);
    }
    let mut wired = serde_json::Map::new();
    wired.insert(
        spec.url_key.to_string(),
        serde_json::Value::String(client_mcp_url(port)),
    );
    if let Some(headers) = entry.get("headers") {
        wired.insert("headers".to_string(), headers.clone());
    }
    if spec.id == "agy" {
        wired.insert("disabled".to_string(), serde_json::Value::Bool(false));
    }
    *entry = serde_json::Value::Object(wired);
    let mut bytes = serde_json::to_vec_pretty(&root)
        .map_err(|err| format!("cannot render {}: {err}", path.display()))?;
    bytes.push(b'\n');
    write_user_file(path, &bytes)
        .map_err(|err| format!("cannot write {}: {err}", path.display()))?;
    Ok(true)
}

/// Point the selected MCP clients at `http://127.0.0.1:<port>/mcp`. Idempotent.
/// Every other server in those files stays, and no API key is ever written.
pub fn wire_mcp_clients(port: u16, selected_clients: &[String]) -> McpWiringReport {
    match crate::os_user_home() {
        Some(user_home) => wire_mcp_clients_in(&user_home, port, selected_clients),
        None => McpWiringReport {
            failed: vec!["all: no home directory".to_string()],
            ..McpWiringReport::default()
        },
    }
}

fn wire_mcp_clients_in(
    user_home: &Path,
    port: u16,
    selected_clients: &[String],
) -> McpWiringReport {
    let mut report = McpWiringReport::default();
    for id in selected_clients {
        let id = id.trim();
        let Some(spec) = MCP_CLIENTS.iter().find(|spec| spec.id == id) else {
            report
                .failed
                .push(format!("{}: unknown MCP client", clamp_chars(id, 40)));
            continue;
        };
        let already_handled = report.wired.iter().any(|seen| seen == spec.id)
            || report.skipped.iter().any(|seen| seen == spec.id);
        if already_handled {
            continue;
        }
        match wire_one_client(&user_home.join(spec.config_rel), spec, port) {
            Ok(true) => report.wired.push(spec.id.to_string()),
            Ok(false) => report.skipped.push(spec.id.to_string()),
            Err(reason) => report.failed.push(format!("{}: {reason}", spec.id)),
        }
    }
    report
}

// ── Readiness: what this machine actually has ───────────────────────

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingSource {
    pub agent: &'static str,
    pub label: &'static str,
    pub detected: bool,
    /// A bounded count of files on disk, not the catalog census. The census is
    /// `index.catalog_sessions`.
    pub session_files: usize,
    pub root_path: String,
    /// True when `session_files` stopped at the cap. The real number is higher.
    #[serde(skip_serializing_if = "is_false")]
    pub session_files_truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct OnboardingPhrases {
    pub custom_file_exists: bool,
    pub intent_keywords: Vec<String>,
    pub task_action_heads: Vec<String>,
    pub decision_policy: Vec<String>,
    pub requirement_markers: Vec<String>,
    pub question_markers: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingEmbedder {
    pub backend: String,
    pub profile: String,
    pub model_cached: bool,
    pub cloud_url: String,
    pub cloud_model: String,
    pub cloud_api_key_env: String,
    /// Whether that variable is set. Its value never leaves the process.
    pub cloud_api_key_present: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingServices {
    pub mcp_service_installed: bool,
    pub mcp_service_healthy: bool,
    pub mcp_port: u16,
    pub reindex_schedule_installed: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingMcpClient {
    pub id: &'static str,
    pub label: &'static str,
    pub config_path: String,
    pub detected: bool,
    pub wired: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct OnboardingIndex {
    pub catalog_exists: bool,
    pub catalog_sessions: usize,
    pub index_exists: bool,
    pub indexed_documents: usize,
    /// Why the last onboarding build failed, when it did.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

/// `GET /api/onboarding/state`.
#[derive(Debug, Clone, Serialize)]
pub struct OnboardingReadiness {
    pub survey_required: bool,
    pub ready: bool,
    pub home_dir: String,
    pub phrases_path: String,
    pub config_path: String,
    pub rebuilding: bool,
    pub sources: Vec<OnboardingSource>,
    pub phrases: OnboardingPhrases,
    pub embedder: OnboardingEmbedder,
    pub services: OnboardingServices,
    pub mcp_clients: Vec<OnboardingMcpClient>,
    pub index: OnboardingIndex,
}

/// Read every onboarding pillar from this machine.
pub fn inspect_onboarding_readiness(
    home: &Path,
    port: u16,
    rebuilding: bool,
) -> OnboardingReadiness {
    let user_home = crate::os_user_home().unwrap_or_else(|| home.to_path_buf());
    inspect_onboarding_readiness_in(home, &user_home, port, rebuilding)
}

fn inspect_onboarding_readiness_in(
    home: &Path,
    user_home: &Path,
    port: u16,
    rebuilding: bool,
) -> OnboardingReadiness {
    let survey_required = needs_full_survey(home);
    let index = inspect_index(home);
    let services = inspect_services(home, user_home, port);
    OnboardingReadiness {
        survey_required,
        ready: !survey_required && index.catalog_exists && index.index_exists,
        home_dir: home.display().to_string(),
        phrases_path: home.join("intent_phrases.toml").display().to_string(),
        config_path: home.join("config.toml").display().to_string(),
        rebuilding,
        sources: inspect_sources(user_home),
        phrases: inspect_phrases(home),
        embedder: inspect_embedder(home),
        mcp_clients: inspect_mcp_clients(user_home, services.mcp_port),
        services,
        index,
    }
}

/// Same shape rules as `AgentKind::accepts_extension` and
/// `is_primary_source_file`, which stay private to the catalog. A `session_files`
/// count is a readiness signal; the catalog stays the only census.
fn is_source_file(kind: AgentKind, path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    let extension = path.extension().and_then(|ext| ext.to_str());
    match kind {
        AgentKind::Claude | AgentKind::Codex | AgentKind::Junie => extension == Some("jsonl"),
        AgentKind::Grok => name == "chat_history.jsonl",
        AgentKind::Kimi => name == "wire.jsonl",
        AgentKind::Gemini => {
            matches!(extension, Some("json" | "jsonl"))
                && path
                    .ancestors()
                    .skip(1)
                    .take(2)
                    .any(|dir| dir.file_name().and_then(|dir| dir.to_str()) == Some("chats"))
        }
        AgentKind::Cursor => {
            extension == Some("jsonl")
                && path.file_stem() == path.parent().and_then(Path::file_name)
                && path
                    .ancestors()
                    .nth(2)
                    .and_then(Path::file_name)
                    .and_then(|dir| dir.to_str())
                    == Some("agent-transcripts")
        }
        AgentKind::Copilot => crate::session_catalog::is_copilot_source_file(path),
        AgentKind::Omp => crate::session_catalog::is_omp_source_file(path),
        AgentKind::Agy => crate::session_catalog::is_agy_source_file(path),
    }
}

/// Count session files under `roots`. Returns the count and whether a cap cut it.
fn count_source_files(roots: &[PathBuf], kind: AgentKind) -> (usize, bool) {
    let mut files = 0usize;
    let mut entries_seen = 0usize;
    let mut pending: Vec<(PathBuf, usize)> = roots.iter().map(|root| (root.clone(), 0)).collect();
    while let Some((dir, depth)) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            if files >= SOURCE_FILE_CAP || entries_seen >= SOURCE_ENTRY_CAP {
                return (files, true);
            }
            entries_seen += 1;
            // Never follows a symlink, so a link cycle cannot trap the walk.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let path = entry.path();
            if file_type.is_dir() {
                if depth < SOURCE_WALK_DEPTH {
                    pending.push((path, depth + 1));
                }
            } else if file_type.is_file() && is_source_file(kind, &path) {
                files += 1;
            }
        }
    }
    (files, false)
}

fn inspect_sources(user_home: &Path) -> Vec<OnboardingSource> {
    let agent_row = |kind: AgentKind, label: &'static str| {
        let roots = kind.session_roots(user_home);
        let (session_files, session_files_truncated) = count_source_files(&roots, kind);
        source_row(
            kind.as_str(),
            label,
            &roots,
            session_files,
            session_files_truncated,
        )
    };
    let codescribe_root = user_home.join(".codescribe").join("transcriptions");
    let codescribe_files =
        crate::importers::codescribe::discover_codescribe_transcripts(user_home).len();
    vec![
        agent_row(AgentKind::Claude, "Claude Code"),
        agent_row(AgentKind::Codex, "OpenAI Codex"),
        source_row(
            "codescribe",
            "Codescribe",
            std::slice::from_ref(&codescribe_root),
            codescribe_files,
            false,
        ),
        agent_row(AgentKind::Cursor, "Cursor"),
        agent_row(AgentKind::Gemini, "Gemini CLI"),
        agent_row(AgentKind::Agy, "Antigravity"),
        agent_row(AgentKind::Grok, "Grok"),
        agent_row(AgentKind::Junie, "Junie"),
        agent_row(AgentKind::Kimi, "Kimi"),
        agent_row(AgentKind::Copilot, "GitHub Copilot"),
        agent_row(AgentKind::Omp, "Oh My Pi"),
    ]
}

fn source_row(
    agent: &'static str,
    label: &'static str,
    roots: &[PathBuf],
    session_files: usize,
    session_files_truncated: bool,
) -> OnboardingSource {
    let present = roots.iter().find(|root| root.is_dir());
    OnboardingSource {
        agent,
        label,
        detected: present.is_some(),
        session_files,
        root_path: present
            .or_else(|| roots.first())
            .map(|root| root.display().to_string())
            .unwrap_or_default(),
        session_files_truncated,
    }
}

fn phrase_category(value: &toml::Value, section: &str, key: &str) -> Vec<String> {
    value
        .get(section)
        .and_then(|table| table.get(key))
        .and_then(toml::Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// How many phrases the five lists onboarding edits hold together.
fn phrase_total(source: &str) -> usize {
    let Ok(value) = toml::from_str::<toml::Value>(source) else {
        return 0;
    };
    PHRASE_CATEGORIES
        .iter()
        .map(|(section, key)| phrase_category(&value, section, key).len())
        .sum()
}

fn inspect_phrases(home: &Path) -> OnboardingPhrases {
    let path = home.join("intent_phrases.toml");
    let source = crate::parser::intent_phrases::read_operator_or_embedded(&path);
    let value = toml::from_str::<toml::Value>(&source)
        .or_else(
            |_| toml::from_str::<toml::Value>(crate::parser::intent_phrases::embedded_source()),
        )
        .unwrap_or_else(|_| toml::Value::Table(toml::map::Map::new()));
    OnboardingPhrases {
        custom_file_exists: path.is_file(),
        intent_keywords: phrase_category(&value, "intent", "keywords"),
        task_action_heads: phrase_category(&value, "task", "action_heads"),
        decision_policy: phrase_category(&value, "decision", "policy"),
        requirement_markers: phrase_category(&value, "requirement", "markers"),
        question_markers: phrase_category(&value, "question", "markers"),
    }
}

fn toml_text(table: Option<&toml::Value>, key: &str) -> Option<String> {
    table
        .and_then(|table| table.get(key))
        .and_then(toml::Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
}

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
fn embedder_model_cached(profile: &str) -> bool {
    let Some(profile) = crate::embedder::EmbeddingProfile::parse(profile) else {
        return false;
    };
    let spec = crate::embedder::profile_spec(profile);
    crate::embedder::find_cached_model_file(spec.repo, spec.filename).is_some()
}

#[cfg(not(any(feature = "native-embedder", feature = "cloud-embedder")))]
fn embedder_model_cached(_profile: &str) -> bool {
    false
}

fn inspect_embedder(home: &Path) -> OnboardingEmbedder {
    let config = std::fs::read_to_string(home.join("config.toml"))
        .ok()
        .and_then(|raw| toml::from_str::<toml::Value>(&raw).ok());
    let embedder = config.as_ref().and_then(|config| config.get("embedder"));
    let cloud = embedder.and_then(|embedder| embedder.get("cloud"));
    let profile = toml_text(embedder, "profile").unwrap_or_else(|| "base".to_string());
    let cloud_api_key_env =
        toml_text(cloud, "api_key_env").unwrap_or_else(|| DEFAULT_CLOUD_API_KEY_ENV.to_string());
    OnboardingEmbedder {
        backend: embedder_backend(home).unwrap_or_else(|| "gguf".to_string()),
        model_cached: embedder_model_cached(&profile),
        profile,
        cloud_url: toml_text(cloud, "url").unwrap_or_else(|| DEFAULT_CLOUD_URL.to_string()),
        cloud_model: toml_text(cloud, "model").unwrap_or_else(|| DEFAULT_CLOUD_MODEL.to_string()),
        cloud_api_key_present: valid_env_var_name(&cloud_api_key_env)
            && std::env::var_os(&cloud_api_key_env).is_some_and(|value| !value.is_empty()),
        cloud_api_key_env,
    }
}

/// The port written into a service unit, in plist or command-line form.
fn service_definition_port(text: &str) -> Option<u16> {
    let after_flag = text.split("--port").nth(1)?;
    let digits: String = after_flag
        .chars()
        .skip_while(|ch| !ch.is_ascii_digit())
        .take_while(char::is_ascii_digit)
        .collect();
    digits.parse().ok()
}

fn inspect_services(home: &Path, user_home: &Path, port: u16) -> OnboardingServices {
    let definition = read_service_definition();
    let mcp_port = definition
        .as_deref()
        .and_then(service_definition_port)
        .unwrap_or(port);
    let address = SocketAddr::from(([127, 0, 0, 1], mcp_port));
    OnboardingServices {
        mcp_service_installed: definition.is_some(),
        mcp_service_healthy: service_meets_contract(home)
            && TcpStream::connect_timeout(&address, Duration::from_millis(100)).is_ok(),
        mcp_port,
        reindex_schedule_installed: reindex_unit_path(user_home).is_file(),
    }
}

fn inspect_mcp_clients(user_home: &Path, port: u16) -> Vec<OnboardingMcpClient> {
    MCP_CLIENTS
        .iter()
        .map(|spec| {
            let path = user_home.join(spec.config_rel);
            OnboardingMcpClient {
                id: spec.id,
                label: spec.label,
                detected: path.parent().is_some_and(Path::is_dir),
                wired: client_is_wired(&path, spec, port),
                config_path: path.display().to_string(),
            }
        })
        .collect()
}

fn count_catalog_rows(home: &Path) -> usize {
    let path = crate::catalog::sessions_path_for(home);
    let Ok(file) = crate::source_path::open_under_aicx_home(home, &path) else {
        return 0;
    };
    std::io::BufReader::new(file)
        .lines()
        .map_while(Result::ok)
        .filter(|line| !line.trim().is_empty())
        .count()
}

/// Documents in the generation `CURRENT` points at, the one search reads.
fn current_index_documents(home: &Path) -> usize {
    let hybrid_root = home.join("indexed").join("_all").join("hybrid");
    let manifest =
        crate::vector_index::resolve_hybrid_generation_dir(&hybrid_root).join("manifest.json");
    aicx_retrieve::Manifest::read_from_path(&manifest)
        .map(|manifest| manifest.lexical_doc_count)
        .unwrap_or(0)
}

fn inspect_index(home: &Path) -> OnboardingIndex {
    let catalog_exists = crate::catalog::sessions_path_for(home).is_file();
    let indexed_documents = current_index_documents(home);
    OnboardingIndex {
        catalog_exists,
        catalog_sessions: if catalog_exists {
            count_catalog_rows(home)
        } else {
            0
        },
        index_exists: indexed_documents > 0,
        indexed_documents,
        last_error: std::fs::read_to_string(home.join(INDEX_ERROR_REL))
            .ok()
            .map(|text| text.trim().to_string())
            .filter(|text| !text.is_empty()),
    }
}

// ── Phrase suggestions from what this person typed ──────────────────

#[derive(Debug, Clone, Serialize)]
pub struct MatchedPhraseSample {
    pub text: String,
    pub entry_type: String,
    pub confidence: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct CandidateOpener {
    pub phrase: String,
    pub count: usize,
    pub suggested_bucket: &'static str,
    pub sample_line: String,
}

/// `GET /api/onboarding/phrase-suggestions`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PhraseSuggestions {
    pub scanned_sessions: usize,
    pub scanned_utterances: usize,
    pub matched_samples: Vec<MatchedPhraseSample>,
    pub candidate_openers: Vec<CandidateOpener>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PhrasePreviewLine {
    pub line: String,
    pub matched: bool,
    pub entry_types: Vec<String>,
    pub max_confidence: f32,
}

/// `POST /api/onboarding/preview-phrases`.
#[derive(Debug, Clone, Default, Serialize)]
pub struct PhrasePreview {
    pub results: Vec<PhrasePreviewLine>,
}

fn clamp_chars(text: &str, max_chars: usize) -> String {
    match text.char_indices().nth(max_chars) {
        Some((end, _)) => format!("{}…", &text[..end]),
        None => text.to_string(),
    }
}

/// Everything quoted back to the page passes through here first.
fn safe_excerpt(line: &str) -> String {
    clamp_chars(&crate::redact::redact_secrets(line), EXCERPT_CHAR_CAP)
}

struct RecentSession {
    modified: SystemTime,
    path: PathBuf,
    kind: AgentKind,
}

/// Follow the newest directories down instead of walking the tree. Bounded by
/// depth, fan-out, and entries read per directory, and it stops at `deadline`.
fn collect_recent_sessions(
    root: &Path,
    kind: AgentKind,
    deadline: Instant,
    found: &mut Vec<RecentSession>,
) {
    let mut frontier = vec![root.to_path_buf()];
    for _ in 0..RECENT_WALK_DEPTH {
        let mut next: Vec<(SystemTime, PathBuf)> = Vec::new();
        for dir in frontier.drain(..) {
            if Instant::now() >= deadline {
                return;
            }
            let Ok(entries) = std::fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten().take(RECENT_DIR_ENTRY_CAP) {
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };
                let Ok(modified) = entry.metadata().and_then(|meta| meta.modified()) else {
                    continue;
                };
                let path = entry.path();
                if file_type.is_dir() {
                    // Agent-written prompts, not this person's words.
                    if entry.file_name() != "subagents" {
                        next.push((modified, path));
                    }
                } else if file_type.is_file() && is_source_file(kind, &path) {
                    found.push(RecentSession {
                        modified,
                        path,
                        kind,
                    });
                }
            }
        }
        if next.is_empty() {
            return;
        }
        next.sort_by(|left, right| right.0.cmp(&left.0));
        next.truncate(RECENT_DIR_FANOUT);
        frontier = next.into_iter().map(|(_, path)| path).collect();
    }
}

fn recent_sessions(user_home: &Path, deadline: Instant) -> Vec<RecentSession> {
    let mut found = Vec::new();
    for kind in AgentKind::ALL {
        // A SQLite store has no bounded read: it would be copied out whole.
        if matches!(kind, AgentKind::Agy) {
            continue;
        }
        for root in kind.session_roots(user_home) {
            collect_recent_sessions(&root, kind, deadline, &mut found);
        }
    }
    found.sort_by(|left, right| right.modified.cmp(&left.modified));
    found.truncate(SUGGESTION_SESSION_CAP);
    found
}

/// Read up to `SUGGESTION_HEAD_BYTES`, cut back to a whole line.
/// Returns the bytes and whether the file continued past them.
fn read_session_head(path: &Path) -> Option<(Vec<u8>, bool)> {
    let file = crate::sanitize::open_file_validated(path).ok()?;
    let mut bytes = Vec::new();
    let mut reader = file.take(SUGGESTION_HEAD_BYTES + 1);
    reader.read_to_end(&mut bytes).ok()?;
    let truncated = bytes.len() as u64 > SUGGESTION_HEAD_BYTES;
    if truncated {
        let last_newline = bytes.iter().rposition(|byte| *byte == b'\n')?;
        bytes.truncate(last_newline + 1);
    }
    Some((bytes, truncated))
}

/// The messages this person typed in one session. `None` when the session
/// cannot be parsed from a bounded read, and then it is not counted as scanned.
fn session_user_messages(session: &RecentSession) -> Option<Vec<String>> {
    use aicx_parser::engine::{SourceArtifact, SourceFraming, SourceHandle};

    let (bytes, truncated) = read_session_head(&session.path)?;
    let whole_document = matches!(session.kind, AgentKind::Gemini)
        && session.path.extension().and_then(|ext| ext.to_str()) == Some("json");
    // One JSON document cannot be cut in half and still parse.
    if whole_document && truncated {
        return None;
    }
    let framing = if whole_document {
        SourceFraming::WholeDocument
    } else {
        SourceFraming::JsonLines
    };
    let artifact_name = session
        .path
        .file_name()
        .and_then(|name| name.to_str())
        .filter(|name| !name.starts_with('.'))
        .unwrap_or("source.jsonl");
    let artifact = SourceArtifact::memory(artifact_name, bytes, framing).ok()?;
    let handle = SourceHandle::new(
        session.kind.parser_kind(),
        "onboarding-probe",
        None,
        vec![artifact],
    )
    .ok()?;
    let parsed = crate::parser_dispatch::parse_handle(&handle).ok()?;
    Some(
        crate::output::timeline_entries_from_model(parsed.model())
            .into_iter()
            .filter(|entry| {
                entry.frame_kind == Some(crate::timeline::FrameKind::UserMsg)
                    && crate::intents::is_user_role(&entry.role)
                    && !crate::extraction::conversation::is_harness_injected_noise(
                        &entry.role,
                        &entry.message,
                    )
                    && !crate::sanitize::is_self_echo(&entry.message)
            })
            .map(|entry| entry.message)
            .collect(),
    )
}

/// The lines of a message that read as something a person typed.
fn utterance_lines(message: &str) -> Vec<&str> {
    use crate::extraction::conversation::{IntentLineModality, intent_line_modality};

    if message.len() > UTTERANCE_MESSAGE_BYTE_CAP {
        return Vec::new();
    }
    let lines: Vec<&str> = message
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    if lines.len() > UTTERANCE_MESSAGE_LINE_CAP {
        return Vec::new();
    }
    lines
        .into_iter()
        .filter(|line| {
            line.chars().count() <= UTTERANCE_LINE_CHAR_CAP
                && intent_line_modality("user", line) != IntentLineModality::PastedReference
        })
        .collect()
}

#[derive(Debug, Default)]
struct OpenerTally {
    count: usize,
    question_lines: usize,
    sample_line: String,
}

/// Tally the first two and three words of a line. Stops at the first token that
/// is not a plain word, so code and paths never become a phrase.
fn tally_openers(line: &str, openers: &mut HashMap<String, OpenerTally>) {
    let words: Vec<String> = line
        .split_whitespace()
        .take(3)
        .map(|word| word.trim_matches(|ch: char| !ch.is_alphanumeric()))
        .take_while(|word| !word.is_empty() && word.chars().all(char::is_alphabetic))
        .map(str::to_lowercase)
        .collect();
    for size in 2..=words.len() {
        let tally = openers.entry(words[..size].join(" ")).or_default();
        tally.count += 1;
        if line.ends_with('?') {
            tally.question_lines += 1;
        }
        if tally.sample_line.is_empty() {
            tally.sample_line = safe_excerpt(line);
        }
    }
}

/// Openers worth offering: said at least twice, not already in the dictionary.
/// When every "weź sprawdź" continues as "weź sprawdź dlaczego", the longer
/// phrase is the one that was really said, so the prefix drops out.
fn rank_openers(
    openers: &HashMap<String, OpenerTally>,
    known: &BTreeSet<String>,
) -> Vec<CandidateOpener> {
    let mut ranked: Vec<CandidateOpener> = openers
        .iter()
        .filter(|(phrase, tally)| {
            let longer_prefix = format!("{phrase} ");
            tally.count >= 2
                && !known.contains(phrase.as_str())
                && !openers.iter().any(|(longer, other)| {
                    other.count == tally.count && longer.starts_with(&longer_prefix)
                })
        })
        .map(|(phrase, tally)| CandidateOpener {
            phrase: phrase.clone(),
            count: tally.count,
            suggested_bucket: if tally.question_lines * 2 > tally.count {
                "question_markers"
            } else {
                "intent_keywords"
            },
            sample_line: tally.sample_line.clone(),
        })
        .collect();
    ranked.sort_by(|left, right| {
        right
            .count
            .cmp(&left.count)
            .then_with(|| left.phrase.cmp(&right.phrase))
    });
    ranked.truncate(CANDIDATE_OPENER_CAP);
    ranked
}

/// What the current dictionary finds in this person's recent sessions, and what
/// they keep saying that it misses. Everything here comes from their own files:
/// no sessions means empty lists, never examples.
pub fn discover_phrase_suggestions(home: &Path) -> PhraseSuggestions {
    let user_home = crate::os_user_home().unwrap_or_else(|| home.to_path_buf());
    discover_phrase_suggestions_in(home, &user_home, SUGGESTION_BUDGET)
}

fn discover_phrase_suggestions_in(
    home: &Path,
    user_home: &Path,
    budget: Duration,
) -> PhraseSuggestions {
    let started = Instant::now();
    let deadline = started + budget;
    let mut suggestions = PhraseSuggestions::default();
    let mut samples_per_type: HashMap<&'static str, usize> = HashMap::new();
    let mut openers: HashMap<String, OpenerTally> = HashMap::new();
    // Half the budget at most for finding files, so some is left to read them.
    for session in recent_sessions(user_home, started + budget / 2) {
        if suggestions.scanned_utterances >= SUGGESTION_UTTERANCE_CAP
            || (suggestions.scanned_sessions > 0 && Instant::now() >= deadline)
        {
            break;
        }
        let Some(messages) = session_user_messages(&session) else {
            continue;
        };
        suggestions.scanned_sessions += 1;
        for message in &messages {
            for line in utterance_lines(message) {
                if suggestions.scanned_utterances >= SUGGESTION_UTTERANCE_CAP {
                    break;
                }
                suggestions.scanned_utterances += 1;
                match crate::intents::classify_line_entry_type(line, true) {
                    Some((entry_type, confidence)) => {
                        let entry_type = entry_type.as_str();
                        let seen = samples_per_type.entry(entry_type).or_insert(0);
                        if *seen < MATCHED_SAMPLES_PER_TYPE
                            && suggestions.matched_samples.len() < MATCHED_SAMPLE_CAP
                        {
                            *seen += 1;
                            suggestions.matched_samples.push(MatchedPhraseSample {
                                text: safe_excerpt(line),
                                entry_type: entry_type.to_string(),
                                confidence,
                            });
                        }
                    }
                    None => tally_openers(line, &mut openers),
                }
            }
        }
    }
    let phrases = inspect_phrases(home);
    let known: BTreeSet<String> = [
        phrases.intent_keywords,
        phrases.task_action_heads,
        phrases.decision_policy,
        phrases.requirement_markers,
        phrases.question_markers,
    ]
    .into_iter()
    .flatten()
    .map(|phrase| phrase.trim().to_lowercase())
    .collect();
    suggestions.candidate_openers = rank_openers(&openers, &known);
    suggestions
}

/// Classify lines against the dictionary this process has loaded. A line gets
/// one type, the one the classifier decides on. Writes nothing.
pub fn preview_phrases_on_lines(lines: &[String]) -> PhrasePreview {
    PhrasePreview {
        results: lines
            .iter()
            .take(PREVIEW_LINE_CAP)
            .map(|line| {
                let line = clamp_chars(line, PREVIEW_LINE_CHAR_CAP);
                match crate::intents::classify_line_entry_type(&line, true) {
                    Some((entry_type, confidence)) => PhrasePreviewLine {
                        line,
                        matched: true,
                        entry_types: vec![entry_type.as_str().to_string()],
                        max_confidence: confidence,
                    },
                    None => PhrasePreviewLine {
                        line,
                        matched: false,
                        entry_types: Vec::new(),
                        max_confidence: 0.0,
                    },
                }
            })
            .collect(),
    }
}

// ── Finishing onboarding ────────────────────────────────────────────

/// Body of `POST /api/onboarding`. Every field is optional. `{ "phrases": [...] }`
/// alone is the shape the survey page has always sent, and it keeps its behavior.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct OnboardingRequest {
    pub phrases: Vec<String>,
    pub intent_keywords: Vec<String>,
    pub task_action_heads: Vec<String>,
    pub decision_policy: Vec<String>,
    pub requirement_markers: Vec<String>,
    pub question_markers: Vec<String>,
    pub embedder: Option<EmbedderOnboardingSelection>,
    pub install_services: Option<bool>,
    pub install_reindex_schedule: Option<bool>,
    pub wire_mcp_clients: Option<bool>,
    pub selected_mcp_clients: Option<Vec<String>>,
    pub build_initial_index: Option<bool>,
}

impl OnboardingRequest {
    fn is_legacy_survey(&self) -> bool {
        self.intent_keywords.is_empty()
            && self.task_action_heads.is_empty()
            && self.decision_policy.is_empty()
            && self.requirement_markers.is_empty()
            && self.question_markers.is_empty()
            && self.embedder.is_none()
            && self.install_services.is_none()
            && self.install_reindex_schedule.is_none()
            && self.wire_mcp_clients.is_none()
            && self.selected_mcp_clients.is_none()
            && self.build_initial_index.is_none()
    }

    fn phrase_additions(&self) -> [(&'static str, &'static str, &[String]); 6] {
        [
            ("intent", "keywords", self.phrases.as_slice()),
            ("intent", "keywords", self.intent_keywords.as_slice()),
            ("task", "action_heads", self.task_action_heads.as_slice()),
            ("decision", "policy", self.decision_policy.as_slice()),
            (
                "requirement",
                "markers",
                self.requirement_markers.as_slice(),
            ),
            ("question", "markers", self.question_markers.as_slice()),
        ]
    }
}

#[derive(Debug, Clone)]
pub struct OnboardingOutcome {
    pub added: usize,
    /// Phrases across the five lists onboarding edits, after this save.
    pub total: usize,
    pub phrases_path: PathBuf,
    pub config_path: PathBuf,
    pub service: ServiceInstall,
    pub services_installed: bool,
    pub reindex_installed: bool,
    pub wired_clients: Vec<String>,
    /// Requested work that did not happen, in words the page can show.
    pub warnings: Vec<String>,
    /// Whether a requested index build has any session file to read.
    pub session_files_found: bool,
}

/// Append phrases to their lists. Markers keep trailing spaces, because a
/// marker like `"jak "` means something different from `"jak"`.
fn merge_phrase_categories(
    current: &str,
    additions: &[(&str, &str, &[String])],
) -> Result<String, String> {
    let mut value: toml::Value = toml::from_str(current).map_err(|err| err.to_string())?;
    for (section, key, phrases) in additions {
        if phrases.is_empty() {
            continue;
        }
        let list = value
            .get_mut(*section)
            .and_then(|table| table.get_mut(*key))
            .and_then(|list| list.as_array_mut())
            .ok_or_else(|| format!("{section}.{key} is missing from intent_phrases.toml"))?;
        for phrase in phrases.iter() {
            let phrase = if *section == "intent" {
                phrase.trim()
            } else {
                phrase.trim_start()
            };
            if phrase.trim().is_empty() {
                continue;
            }
            if phrase.chars().count() > PHRASE_CHAR_CAP || phrase.chars().any(char::is_control) {
                return Err(format!(
                    "{section}.{key}: a phrase is one line of at most {PHRASE_CHAR_CAP} characters"
                ));
            }
            if !list.iter().any(|item| item.as_str() == Some(phrase)) {
                list.push(toml::Value::String(phrase.to_string()));
            }
        }
    }
    toml::to_string_pretty(&value).map_err(|err| err.to_string())
}

/// Write phrases, embedder, and service for the full payload. A selection the
/// page got wrong is refused before the first store changes.
fn apply_full_configuration(
    home: &Path,
    phrases_path: &Path,
    current: &str,
    request: &OnboardingRequest,
    options: &InstallOptions,
) -> Result<ServiceInstall, String> {
    let rendered_config = match &request.embedder {
        Some(selection) => Some(
            render_embedder_config(home, selection).map_err(|err| format!("embedder:{err:#}"))?,
        ),
        None => None,
    };
    let additions = request.phrase_additions();
    let merged = if additions.iter().any(|(_, _, phrases)| !phrases.is_empty()) {
        Some(merge_phrase_categories(current, &additions)?)
    } else {
        None
    };
    let grew = merged
        .as_deref()
        .filter(|rendered| phrase_total(rendered) > phrase_total(current));
    if let Some(rendered) = grew {
        crate::parser::intent_phrases::reload_from_str(rendered)?;
        write_user_file(phrases_path, rendered.as_bytes()).map_err(|err| format!("write:{err}"))?;
    } else if !phrases_path.is_file() {
        write_user_file(phrases_path, current.as_bytes()).map_err(|err| format!("write:{err}"))?;
    }
    match rendered_config {
        Some(rendered) => write_user_file(&home.join("config.toml"), rendered.as_bytes())
            .map_err(|err| format!("write:{err}"))?,
        None if embedder_backend(home).is_none() => {
            write_embedder_choice(home, "gguf", None).map_err(|err| format!("write:{err}"))?;
        }
        None => {}
    }
    let service = if request.install_services.unwrap_or(true) {
        bring_service_onto_contract(home, options)
    } else {
        ServiceInstall::Skipped {
            reason: "the background service was not requested".to_string(),
        }
    };
    if let ServiceInstall::Failed { reason } = &service {
        return Err(format!("install:{reason}"));
    }
    Ok(service)
}

/// Apply one onboarding save and mark onboarding complete.
///
/// `Err` strings carry the stable prefixes `write:`, `install:`, and `embedder:`.
/// A failed write or service install leaves onboarding unfinished. A schedule
/// or client that could not be set up is a warning, because what it needs can
/// be fixed with one more save.
pub fn apply_onboarding_request(
    home: &Path,
    request: &OnboardingRequest,
    options: &InstallOptions,
) -> Result<OnboardingOutcome, String> {
    let user_home = crate::os_user_home().unwrap_or_else(|| home.to_path_buf());
    apply_onboarding_request_in(home, &user_home, request, options)
}

fn apply_onboarding_request_in(
    home: &Path,
    user_home: &Path,
    request: &OnboardingRequest,
    options: &InstallOptions,
) -> Result<OnboardingOutcome, String> {
    let phrases_path = home.join("intent_phrases.toml");
    let current = crate::parser::intent_phrases::read_operator_or_embedded(&phrases_path);
    let total_before = phrase_total(&current);
    let service = if request.is_legacy_survey() {
        apply_onboarding_survey(home, &current, &request.phrases, options)?.service
    } else {
        apply_full_configuration(home, &phrases_path, &current, request, options)?
    };

    let mut warnings = Vec::new();
    if request.install_reindex_schedule == Some(true) {
        match install_reindex_schedule_in(home, user_home, options) {
            ServiceInstall::Failed { reason } | ServiceInstall::Skipped { reason } => {
                warnings.push(format!("reindex schedule: {reason}"));
            }
            ServiceInstall::Installed | ServiceInstall::NativeInstalled => {}
        }
    }
    let mut wired_clients = Vec::new();
    if request.wire_mcp_clients == Some(true) {
        let port = read_service_definition()
            .as_deref()
            .and_then(service_definition_port)
            .unwrap_or(options.port);
        let selected = request
            .selected_mcp_clients
            .clone()
            .unwrap_or_else(|| MCP_CLIENTS.iter().map(|spec| spec.id.to_string()).collect());
        let report = wire_mcp_clients_in(user_home, port, &selected);
        warnings.extend(
            report
                .skipped
                .iter()
                .map(|id| format!("{id}: not found on this machine, so it was not wired")),
        );
        warnings.extend(
            report
                .failed
                .iter()
                .map(|failure| format!("MCP wiring failed for {failure}")),
        );
        wired_clients = report.wired;
    }
    mark_complete(home).map_err(|err| format!("write:{err}"))?;

    let total = phrase_total(&crate::parser::intent_phrases::read_operator_or_embedded(
        &phrases_path,
    ));
    Ok(OnboardingOutcome {
        added: total.saturating_sub(total_before),
        total,
        phrases_path,
        config_path: home.join("config.toml"),
        services_installed: matches!(
            service,
            ServiceInstall::Installed | ServiceInstall::NativeInstalled
        ) || service_meets_contract(home),
        service,
        reindex_installed: reindex_unit_path(user_home).is_file(),
        wired_clients,
        warnings,
        session_files_found: request.build_initial_index == Some(true)
            && inspect_sources(user_home)
                .iter()
                .any(|source| source.session_files > 0),
    })
}

/// Census, then build, in the order `aicx index` runs them. The build refuses an
/// empty catalog, and a first run has no catalog yet.
fn build_initial_index(home: &Path, user_home: &Path) -> Result<()> {
    let total_sessions = if crate::catalog::sessions_path_for(home).is_file() {
        let cutoff = SystemTime::now()
            .checked_sub(INDEX_CENSUS_WINDOW)
            .and_then(|at| at.duration_since(SystemTime::UNIX_EPOCH).ok())
            .map_or(0, |since| since.as_nanos());
        crate::catalog::refresh_hot(home, user_home, cutoff)?.total_sessions
    } else {
        crate::catalog::rebuild_with_progress(home, user_home, |_| {})?.total_sessions
    };
    anyhow::ensure!(
        total_sessions > 0,
        "no agent sessions were found on this machine yet"
    );
    let _lock = crate::locks::acquire_exclusive(crate::locks::lance_lock_path()?)?;
    crate::source_index::build_with_reporter(
        home,
        &[],
        false,
        false,
        false,
        crate::progress::select_reporter(true),
    )?;
    Ok(())
}

/// The lexical build an onboarding save asks for. It runs after the HTTP
/// response has gone, so a failure is left where the state endpoint can show it.
pub fn run_onboarding_index_job(home: &Path) {
    let _ = std::fs::remove_file(home.join(INDEX_ERROR_REL));
    let outcome = crate::os_user_home()
        .context("No home directory")
        .and_then(|user_home| build_initial_index(home, &user_home));
    if let Err(error) = outcome {
        tracing::error!(%error, "onboarding index build failed");
        let detail = clamp_chars(
            &crate::redact::redact_secrets(&format!("{error:#}")),
            INDEX_ERROR_CHAR_CAP,
        );
        let _ = write_state_line(home, INDEX_ERROR_REL, &detail);
    }
}

pub fn locate_service_installer() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("AICX_MCP_SERVICE_INSTALLER") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    let filename = match std::env::consts::OS {
        "macos" => "install-mcp-service.sh",
        "linux" => "install-mcp-service-linux.sh",
        "windows" => "install-mcp-service.ps1",
        _ => return None,
    };
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("../../tools").join(filename));
        candidates.push(dir.join("../tools").join(filename));
    }
    candidates.push(PathBuf::from("tools").join(filename));
    candidates.into_iter().find(|path| path.is_file())
}

fn env_flag(name: &str) -> bool {
    matches!(
        std::env::var(name).ok().as_deref(),
        Some("1" | "true" | "yes")
    )
}

fn open_dashboard(url: &str) -> bool {
    let mut command = if cfg!(target_os = "macos") {
        Command::new("open")
    } else if cfg!(target_os = "windows") {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        return command
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
    } else {
        Command::new("xdg-open")
    };
    command
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// Serializes tests that mutate process-global `AICX_SERVICE_UNIT`.
#[cfg(test)]
pub(crate) static SERVICE_UNIT_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_home_needs_onboarding_until_marker_or_config() {
        let root =
            std::env::temp_dir().join(format!("aicx-onboarding-{}-{}", std::process::id(), "bare"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(needs_onboarding(&root));

        std::fs::write(root.join("config.toml"), "[embedder]\n").unwrap();
        assert!(!needs_onboarding(&root));
        std::fs::remove_file(root.join("config.toml")).unwrap();
        assert!(needs_onboarding(&root));

        mark_complete(&root).unwrap();
        assert!(!needs_onboarding(&root));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn survey_appends_intent_keywords_into_the_existing_phrase_file() {
        let current = crate::parser::intent_phrases::embedded_source();
        let merged =
            merge_intent_keywords(current, &["ship the hybrid listener".into()]).expect("merge");
        assert!(merged.contains("ship the hybrid listener"));
        assert!(merged.contains("[intent]"));
        crate::parser::intent_phrases::reload_from_str(&merged).expect("merged phrases parse");
        let again = merge_intent_keywords(&merged, &["ship the hybrid listener".into()]);
        assert!(again.is_err(), "a repeated phrase is not a second store");
        crate::parser::intent_phrases::reload_from_str(
            crate::parser::intent_phrases::embedded_source(),
        )
        .expect("restore embedded phrases");
    }

    #[test]
    fn survey_writes_phrases_and_invokes_installer_when_service_is_missing() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "aicx-onboarding-apply-{}-{}",
            std::process::id(),
            "survey"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("ran");
        let bin = dir.join("aicx-under-test");
        let script = if cfg!(windows) {
            dir.join("install-mcp-service.ps1")
        } else {
            dir.join("install-mcp-service.sh")
        };
        let body = if cfg!(windows) {
            format!(
                "$utf8 = New-Object System.Text.UTF8Encoding $false\n[System.IO.File]::WriteAllText('{}', $env:AICX_BIN, $utf8)\nexit 0\n",
                log.display().to_string().replace('\'', "''")
            )
        } else {
            format!(
                "#!/bin/sh\nprintf '%s' \"$AICX_BIN\" > '{}'\n",
                log.display().to_string().replace('\'', "'\\''")
            )
        };
        std::fs::write(&script, body).unwrap();
        let missing_unit = dir.join("missing-service-unit");
        let prev_unit = std::env::var_os("AICX_SERVICE_UNIT");
        unsafe {
            std::env::set_var("AICX_SERVICE_UNIT", &missing_unit);
        }
        let applied = apply_onboarding_survey(
            &dir,
            crate::parser::intent_phrases::embedded_source(),
            &["ship the loopback dashboard".into()],
            &InstallOptions {
                dry_run: false,
                platform_is_macos: cfg!(target_os = "macos"),
                script: Some(script),
                bin: Some(bin.clone()),
                port: 8044,
            },
        )
        .expect("apply");
        unsafe {
            match prev_unit {
                Some(value) => std::env::set_var("AICX_SERVICE_UNIT", value),
                None => std::env::remove_var("AICX_SERVICE_UNIT"),
            }
        }
        let written = std::fs::read_to_string(&applied.phrases_path).unwrap();
        assert!(written.contains("ship the loopback dashboard"));
        assert!(applied.phrases_path.ends_with("intent_phrases.toml"));
        if cfg!(target_os = "macos") {
            assert_eq!(applied.service, ServiceInstall::Installed);
        } else {
            assert_eq!(applied.service, ServiceInstall::NativeInstalled);
        }
        let recorded = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            Path::new(recorded.trim().trim_start_matches('\u{feff}')),
            bin.as_path()
        );
        crate::parser::intent_phrases::reload_from_str(
            crate::parser::intent_phrases::embedded_source(),
        )
        .expect("restore embedded phrases");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn one_save_finishes_survey_with_phrases_local_embedder_and_installer() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "aicx-onboarding-finish-{}-{}",
            std::process::id(),
            "survey"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("ran");
        let bin = dir.join("aicx-under-test");
        let unit = dir.join("service-unit");
        let script = if cfg!(windows) {
            dir.join("install-mcp-service.ps1")
        } else {
            dir.join("install-mcp-service.sh")
        };
        let body = if cfg!(windows) {
            format!(
                "$utf8 = New-Object System.Text.UTF8Encoding $false\n[System.IO.File]::WriteAllText('{}', $env:AICX_BIN, $utf8)\n[System.IO.File]::WriteAllText($env:AICX_SERVICE_UNIT, \"--transport http --host 127.0.0.1 --port 8044 --no-require-auth --no-auto-refresh`n\", $utf8)\nexit 0\n",
                log.display().to_string().replace('\'', "''")
            )
        } else {
            format!(
                "#!/bin/sh\nprintf '%s' \"$AICX_BIN\" > '{}'\nprintf '%s\\n' '--transport http --host 127.0.0.1 --port 8044 --no-require-auth --no-auto-refresh' > \"$AICX_SERVICE_UNIT\"\n",
                log.display().to_string().replace('\'', "'\\''")
            )
        };
        std::fs::write(&script, body).unwrap();
        assert!(
            needs_full_survey(&dir),
            "unconfigured home must still require the survey"
        );
        let prev_unit = std::env::var_os("AICX_SERVICE_UNIT");
        unsafe {
            std::env::set_var("AICX_SERVICE_UNIT", &unit);
        }
        let applied = apply_onboarding_survey(
            &dir,
            crate::parser::intent_phrases::embedded_source(),
            &["finish the first-start survey".into()],
            &InstallOptions {
                dry_run: false,
                platform_is_macos: cfg!(target_os = "macos"),
                script: Some(script),
                bin: Some(bin.clone()),
                port: 8044,
            },
        )
        .expect("one save must finish when the installer succeeds");
        let survey_done = !needs_full_survey(&dir);
        unsafe {
            match prev_unit {
                Some(value) => std::env::set_var("AICX_SERVICE_UNIT", value),
                None => std::env::remove_var("AICX_SERVICE_UNIT"),
            }
        }
        let written = std::fs::read_to_string(&applied.phrases_path).unwrap();
        assert!(written.contains("finish the first-start survey"));
        assert_eq!(embedder_backend(&dir).as_deref(), Some("gguf"));
        let config = std::fs::read_to_string(dir.join("config.toml")).unwrap();
        assert!(!config.contains("OPENAI_API_KEY"));
        assert!(!config.contains("api_key"));
        if cfg!(target_os = "macos") {
            assert_eq!(applied.service, ServiceInstall::Installed);
        } else {
            assert_eq!(applied.service, ServiceInstall::NativeInstalled);
        }
        let recorded = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            Path::new(recorded.trim().trim_start_matches('\u{feff}')),
            bin.as_path()
        );
        assert!(
            survey_done,
            "after one successful save, needs_full_survey must be false"
        );
        crate::parser::intent_phrases::reload_from_str(
            crate::parser::intent_phrases::embedded_source(),
        )
        .expect("restore embedded phrases");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn installer_failure_leaves_needs_full_survey_true() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let dir = std::env::temp_dir().join(format!(
            "aicx-onboarding-fail-{}-{}",
            std::process::id(),
            "survey"
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = if cfg!(windows) {
            dir.join("install-mcp-service.ps1")
        } else {
            dir.join("install-mcp-service.sh")
        };
        let body = if cfg!(windows) {
            "Write-Error 'installer boom'\nexit 1\n".to_string()
        } else {
            "#!/bin/sh\necho installer boom >&2\nexit 1\n".to_string()
        };
        std::fs::write(&script, body).unwrap();
        let missing_unit = dir.join("missing-service-unit");
        let prev_unit = std::env::var_os("AICX_SERVICE_UNIT");
        unsafe {
            std::env::set_var("AICX_SERVICE_UNIT", &missing_unit);
        }
        let err = apply_onboarding_survey(
            &dir,
            crate::parser::intent_phrases::embedded_source(),
            &["keep the survey open on install failure".into()],
            &InstallOptions {
                dry_run: false,
                platform_is_macos: cfg!(target_os = "macos"),
                script: Some(script),
                bin: Some(dir.join("aicx-under-test")),
                port: 8044,
            },
        )
        .expect_err("a failed installer must not finish onboarding");
        let still_required = needs_full_survey(&dir);
        let install_error = service_error(&dir);
        unsafe {
            match prev_unit {
                Some(value) => std::env::set_var("AICX_SERVICE_UNIT", value),
                None => std::env::remove_var("AICX_SERVICE_UNIT"),
            }
        }
        assert!(
            err.starts_with("install:"),
            "failure must surface as install:…, got {err}"
        );
        assert!(
            still_required,
            "a failed install must leave needs_full_survey true"
        );
        assert!(
            install_error.is_some(),
            "failed install must leave the service error marker"
        );
        assert!(dir.join("intent_phrases.toml").is_file());
        assert_eq!(embedder_backend(&dir).as_deref(), Some("gguf"));
        crate::parser::intent_phrases::reload_from_str(
            crate::parser::intent_phrases::embedded_source(),
        )
        .expect("restore embedded phrases");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn non_macos_and_dry_run_do_not_pretend_launchd_installed() {
        let skipped = install_launch_agent(&InstallOptions {
            dry_run: false,
            platform_is_macos: false,
            script: None,
            bin: None,
            port: 8044,
        });
        assert_ne!(
            skipped,
            ServiceInstall::Installed,
            "a non-macOS override must not claim the LaunchAgent was installed"
        );

        let dry = install_launch_agent(&InstallOptions {
            dry_run: true,
            platform_is_macos: true,
            script: Some(PathBuf::from("/nope/install-mcp-service.sh")),
            bin: None,
            port: 8044,
        });
        assert!(
            dry.summary().contains("dry-run"),
            "dry-run must not execute the installer"
        );
    }

    #[test]
    fn loopback_service_contract_accepts_readers_and_rejects_embedded_writer() {
        let reader = "--transport http --host 127.0.0.1 --port 8044 --no-require-auth";
        assert!(service_definition_meets_contract(reader));
        assert!(service_definition_meets_contract(&format!(
            "{reader} --no-auto-refresh"
        )));
        assert!(!service_definition_meets_contract(&format!(
            "{reader} --experimental-auto-refresh"
        )));
        assert!(!service_definition_meets_contract(
            "--transport http --host 0.0.0.0 --port 8044 --no-require-auth"
        ));
    }

    #[test]
    fn installer_invocation_uses_the_service_script() {
        let dir = std::env::temp_dir().join(format!("aicx-install-stub-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let log = dir.join("ran");
        let bin = dir.join("aicx-under-test");
        let script = if cfg!(windows) {
            dir.join("install-mcp-service.ps1")
        } else {
            dir.join("install-mcp-service.sh")
        };
        let body = if cfg!(windows) {
            format!(
                "$utf8 = New-Object System.Text.UTF8Encoding $false\n[System.IO.File]::WriteAllText('{}', $env:AICX_BIN, $utf8)\nexit 0\n",
                log.display().to_string().replace('\'', "''")
            )
        } else {
            format!(
                "#!/bin/sh\nprintf '%s' \"$AICX_BIN\" > '{}'\n",
                log.display().to_string().replace('\'', "'\\''")
            )
        };
        std::fs::write(&script, body).unwrap();
        let outcome = install_launch_agent(&InstallOptions {
            dry_run: false,
            platform_is_macos: cfg!(target_os = "macos"),
            script: Some(script),
            bin: Some(bin.clone()),
            port: 8044,
        });
        if cfg!(target_os = "macos") {
            assert_eq!(outcome, ServiceInstall::Installed);
        } else {
            assert_ne!(
                outcome,
                ServiceInstall::Installed,
                "a non-macOS installer must not claim the LaunchAgent was installed"
            );
            assert_eq!(outcome, ServiceInstall::NativeInstalled);
        }
        let recorded = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            Path::new(recorded.trim().trim_start_matches('\u{feff}')),
            bin.as_path()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn windows_installer_without_override_keeps_configured_storage_home() {
        // Stay outside the checkout. A repo-local config is outside the
        // Windows user allowlist (D:\ runner vs C:\Users) and dirties git status
        // for the provenance test.
        let root = std::env::temp_dir().join(format!(
            "aicx-service-home-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let configured = root.join("configured-corpus");
        std::fs::create_dir_all(root.join(".aicx")).unwrap();
        std::fs::write(
            root.join(".aicx/config.toml"),
            format!("[storage]\nhome = '{}'\n", configured.display()),
        )
        .unwrap();
        assert_eq!(
            crate::aicx_home::resolve_from(None, &root).unwrap(),
            configured
        );
        assert_eq!(
            crate::aicx_home::resolve_from(Some(std::ffi::OsString::new()), &root).unwrap(),
            configured
        );
        assert!(
            EMBEDDED_WINDOWS_SERVICE_INSTALLER
                .contains("Remove-Item Env:AICX_HOME -ErrorAction SilentlyContinue")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn production_installer_resolves_loopback_argv_without_launchctl() {
        let script = locate_service_installer()
            .expect("repo checkout must resolve the native service installer without a test stub");
        let (filename, embedded) = match std::env::consts::OS {
            "linux" => (
                "install-mcp-service-linux.sh",
                EMBEDDED_LINUX_SERVICE_INSTALLER,
            ),
            "windows" => (
                "install-mcp-service.ps1",
                EMBEDDED_WINDOWS_SERVICE_INSTALLER,
            ),
            _ => ("install-mcp-service.sh", EMBEDDED_MCP_SERVICE_INSTALLER),
        };
        assert_eq!(
            script.file_name().and_then(|name| name.to_str()),
            Some(filename)
        );
        assert!(
            script
                .components()
                .any(|component| component.as_os_str() == "tools"),
            "installer path must stay under tools/, got {}",
            script.display()
        );

        let options = InstallOptions {
            dry_run: true,
            platform_is_macos: true,
            script: Some(script.clone()),
            bin: None,
            port: 8044,
        };
        let invocation = resolve_macos_installer_invocation(&options);
        assert_eq!(invocation.program, "bash");
        assert_eq!(
            invocation.args,
            vec![script.as_os_str().to_os_string()],
            "production must exec the real installer path, not a stub"
        );
        assert!(!invocation.stdin_embedded);
        assert_eq!(invocation.port, 8044);

        let argv = loopback_http_argv(8044);
        assert_eq!(
            argv,
            vec![
                "--transport".to_string(),
                "http".to_string(),
                "--host".to_string(),
                "127.0.0.1".to_string(),
                "--port".to_string(),
                "8044".to_string(),
                "--no-require-auth".to_string(),
                "--no-auto-refresh".to_string(),
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "0.0.0.0"));
        assert!(!argv.iter().any(|arg| arg == "--experimental-auto-refresh"));

        let body = std::fs::read_to_string(&script).expect("read installer");
        assert_eq!(body, embedded);
        if cfg!(target_os = "macos") {
            assert!(body.contains("HOST=\"${AICX_MCP_HOST:-127.0.0.1}\""));
            assert!(body.contains("PORT=\"${AICX_MCP_PORT:-8044}\""));
        }
        assert!(body.contains("--no-auto-refresh"));
        assert!(body.contains("--no-require-auth"));
        assert!(!body.contains("0.0.0.0"));
        let program_args = EMBEDDED_MCP_SERVICE_INSTALLER
            .split("<key>ProgramArguments</key>")
            .nth(1)
            .and_then(|rest| rest.split("</array>").next())
            .expect("ProgramArguments array");
        assert!(
            !program_args.contains("--experimental-auto-refresh"),
            "LaunchAgent argv must not enable the embedded writer"
        );
        assert!(program_args.contains("--no-auto-refresh"));
        assert!(program_args.contains("$HOST_XML"));
        assert!(program_args.contains("$PORT_XML"));
        assert!(program_args.contains("<string>serve</string>"));

        let skipped = install_launch_agent(&options);
        assert!(
            skipped.summary().contains("dry-run"),
            "this lock must not call launchctl"
        );
    }

    #[test]
    fn first_start_report_names_the_dashboard_survey() {
        let report = FirstStartReport {
            dashboard_url: "http://127.0.0.1:8044/".into(),
            service: "skipped: dry-run; launchd installer was not executed".into(),
        };
        let rendered = report.render();
        assert!(rendered.contains("onboarding"));
        assert!(rendered.contains("http://127.0.0.1:8044/"));
        assert!(rendered.contains("Type the phrases you use"));
        assert!(rendered.contains("Save stores them on this machine"));
        assert!(rendered.contains("background service"));
        assert!(!rendered.contains("intent_phrases.toml"));
        assert!(!rendered.contains("config.toml"));
        assert!(!rendered.contains("Usage: aicx"));
    }

    #[test]
    fn our_old_service_is_replaced_on_8044() {
        let listeners = [PortListener {
            pid: 9,
            command: "/opt/homebrew/bin/aicx-mcp --host 100.82.232.70 --port 8044".into(),
        }];
        let plan = choose_service_port(&listeners, 8044, &[]);
        assert!(plan.replaced_own_service);
        assert_eq!(plan.port, 8044);
        assert!(plan.foreign.is_none());
    }

    #[test]
    fn foreign_listener_keeps_the_process_and_names_another_port() {
        let listeners = [PortListener {
            pid: 4242,
            command: "nginx: master process".into(),
        }];
        let plan = choose_service_port(&listeners, 8044, &[8045]);
        let foreign = plan.foreign.expect("foreign");
        assert_eq!(plan.port, 8046);
        assert!(!is_our_aicx_process(&foreign.command));
        let text = foreign.instructions();
        assert!(text.contains("nginx: master process"));
        assert!(text.contains("pid 4242"));
        assert!(text.contains("kill 4242"));
        assert!(text.contains("AICX_MCP_PORT=8046 aicx"));
        assert!(text.contains("left that process running"));
    }

    #[test]
    fn empty_home_opens_the_dashboard_path() {
        let root = std::env::temp_dir().join(format!("aicx-first-start-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        assert!(
            bare_start_opens_dashboard(&root),
            "no first-run marker must still open/print the dashboard"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn marked_configured_home_stays_on_short_help() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = std::env::temp_dir().join(format!("aicx-configured-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("state")).unwrap();
        std::fs::write(root.join("intent_phrases.toml"), "phrases = [\"find\"]\n").unwrap();
        write_embedder_choice(&root, "gguf", None).unwrap();
        std::fs::write(
            root.join("state/whats-new-offered"),
            format!("{}\n", package_version()),
        )
        .unwrap();
        mark_complete(&root).unwrap();
        let unit = root.join("aicx-mcp.plist");
        std::fs::write(
            &unit,
            "--transport http --host 127.0.0.1 --port 18044 --no-require-auth --no-auto-refresh\n",
        )
        .unwrap();
        let previous_unit = std::env::var_os("AICX_SERVICE_UNIT");
        unsafe {
            std::env::set_var("AICX_SERVICE_UNIT", &unit);
        }
        let opens = bare_start_opens_dashboard(&root);
        unsafe {
            match previous_unit {
                Some(value) => std::env::set_var("AICX_SERVICE_UNIT", value),
                None => std::env::remove_var("AICX_SERVICE_UNIT"),
            }
        }
        assert!(
            !opens,
            "after this version is configured, bare aicx must not reopen the dashboard"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn configured_version_does_not_open_the_browser() {
        assert!(!steady_state_opens_browser());
        assert!(!survey_required(false, Some("gguf"), true));
    }

    #[test]
    fn local_embedder_choice_does_not_require_an_api_key() {
        let root = std::env::temp_dir().join(format!("aicx-embed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        write_embedder_choice(&root, "gguf", None).unwrap();
        let text = std::fs::read_to_string(root.join("config.toml")).unwrap();
        assert!(text.contains("backend = \"gguf\"") || text.contains("backend = 'gguf'"));
        assert!(!text.contains("OPENAI_API_KEY"));
        assert_eq!(embedder_backend(&root).as_deref(), Some("gguf"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn open_is_skipped_until_loopback_accepts_and_after_timeout() {
        // Ephemeral port — never touch the operator's real 8044.
        let probe = std::net::TcpListener::bind("127.0.0.1:0").expect("bind probe");
        let port = probe.local_addr().expect("addr").port();
        drop(probe);

        let timed_out = wait_until_loopback_accepts(port, Duration::from_millis(250));
        assert!(
            timed_out.is_err(),
            "nothing listening must time out: {timed_out:?}"
        );
        assert!(
            !should_open_dashboard(true, false, timed_out.is_ok()),
            "timeout must not open the browser"
        );

        let listener = std::net::TcpListener::bind(("127.0.0.1", port)).expect("bind fake");
        let acceptor = std::thread::spawn(move || {
            let _ = listener.accept();
        });
        let ready = wait_until_loopback_accepts(port, Duration::from_secs(2));
        assert!(
            ready.is_ok(),
            "fake listener must satisfy the ready wait: {ready:?}"
        );
        assert!(
            should_open_dashboard(true, false, ready.is_ok()),
            "open is allowed only after accept"
        );
        assert!(
            !should_open_dashboard(true, true, true),
            "dry-run must still skip open"
        );
        assert!(
            !should_open_dashboard(false, false, true),
            "no-GUI must still skip open"
        );
        // Never call open_dashboard / launchctl — the gate is the contract under test.
        let _ = acceptor.join();
    }

    fn scratch_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "aicx-onboarding-pillars-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_file(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn read_json(path: &Path) -> serde_json::Value {
        serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn read_toml(path: &Path) -> toml::Value {
        toml::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
    }

    fn dry_run_options() -> InstallOptions {
        InstallOptions {
            dry_run: true,
            platform_is_macos: cfg!(target_os = "macos"),
            script: None,
            bin: None,
            port: 8044,
        }
    }

    fn restore_embedded_phrases() {
        crate::parser::intent_phrases::reload_from_str(
            crate::parser::intent_phrases::embedded_source(),
        )
        .expect("restore embedded phrases");
    }

    /// Point `AICX_SERVICE_UNIT` at `unit` for one test, and put it back even on
    /// panic. Hold `SERVICE_UNIT_ENV_LOCK` for as long as this lives.
    struct ServiceUnitOverride(Option<std::ffi::OsString>);

    impl ServiceUnitOverride {
        fn set(unit: &Path) -> Self {
            let previous = std::env::var_os("AICX_SERVICE_UNIT");
            unsafe {
                std::env::set_var("AICX_SERVICE_UNIT", unit);
            }
            Self(previous)
        }
    }

    impl Drop for ServiceUnitOverride {
        fn drop(&mut self) {
            unsafe {
                match self.0.take() {
                    Some(value) => std::env::set_var("AICX_SERVICE_UNIT", value),
                    None => std::env::remove_var("AICX_SERVICE_UNIT"),
                }
            }
        }
    }

    fn write_claude_session(user_home: &Path, lines: &[&str]) {
        let session = "99999999-9999-4999-8999-999999999999";
        let mut body = String::new();
        for (index, text) in lines.iter().enumerate() {
            let row = serde_json::json!({
                "type": "user",
                "cwd": "/tmp/demo",
                "sessionId": session,
                "timestamp": format!("2026-07-13T10:00:{index:02}.000Z"),
                "message": { "role": "user", "content": text },
            });
            body.push_str(&format!("{row}\n"));
        }
        let reply = serde_json::json!({
            "type": "assistant",
            "sessionId": session,
            "timestamp": "2026-07-13T10:01:00.000Z",
            "message": {
                "role": "assistant",
                "model": "claude-opus-4-8",
                "content": [{ "type": "text", "text": "Odpowiedz." }],
            },
        });
        body.push_str(&format!("{reply}\n"));
        write_file(
            &user_home
                .join(".claude/projects/-tmp-demo")
                .join(format!("{session}.jsonl")),
            &body,
        );
    }

    #[test]
    fn opening_the_dashboard_does_not_finish_onboarding() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let home = scratch_dir("no-premature-mark");
        let _unit = ServiceUnitOverride::set(&home.join("missing-unit"));
        let opened = open_first_start(&home, true, dry_run_options()).unwrap();
        assert!(matches!(opened, FirstStart::Opened(_)));
        assert!(
            !home.join(MARKER_REL).exists(),
            "only a finished save may write the completion marker"
        );
        assert!(needs_onboarding(&home));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn embedder_profiles_accept_every_contract_name() {
        for (sent, stored) in [
            ("base", "base"),
            ("0.6b", "base"),
            ("dev", "dev"),
            ("1.7b", "dev"),
            ("premium", "premium"),
            ("1.7b-q6", "premium"),
            ("max", "max"),
            ("4b", "max"),
            (" 1.7B ", "dev"),
        ] {
            assert_eq!(canonical_embedder_profile(sent), Some(stored), "{sent}");
        }
        assert_eq!(canonical_embedder_profile("huge"), None);
    }

    #[test]
    fn embedder_choice_lands_in_the_canonical_section_beside_other_config() {
        let home = scratch_dir("embedder");
        write_file(
            &home.join("config.toml"),
            "[storage]\nhome = \"/elsewhere\"\n\n[embedder]\nthreads = 4\n",
        );
        write_embedder_config(
            &home,
            &EmbedderOnboardingSelection {
                backend: "gguf".into(),
                profile: "1.7b".into(),
                ..Default::default()
            },
        )
        .unwrap();
        let config = read_toml(&home.join("config.toml"));
        assert_eq!(config["embedder"]["backend"].as_str(), Some("gguf"));
        assert_eq!(config["embedder"]["profile"].as_str(), Some("dev"));
        assert_eq!(config["embedder"]["threads"].as_integer(), Some(4));
        assert_eq!(config["storage"]["home"].as_str(), Some("/elsewhere"));
        assert!(config["embedder"].get("cloud").is_none());
        assert_eq!(embedder_backend(&home).as_deref(), Some("gguf"));
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn cloud_choice_names_the_key_variable_and_refuses_anything_that_holds_a_secret() {
        let home = scratch_dir("embedder-cloud");
        let cloud = EmbedderOnboardingSelection {
            backend: "cloud".into(),
            cloud_url: "https://embed.example.test/v1/embeddings".into(),
            cloud_model: "embed-small".into(),
            cloud_api_key_env: "AICX_TEST_EMBED_KEY".into(),
            ..Default::default()
        };
        write_embedder_config(&home, &cloud).unwrap();
        let path = home.join("config.toml");
        let config = read_toml(&path);
        let section = &config["embedder"]["cloud"];
        assert_eq!(
            section["url"].as_str(),
            Some("https://embed.example.test/v1/embeddings")
        );
        assert_eq!(section["model"].as_str(), Some("embed-small"));
        assert_eq!(section["api_key_env"].as_str(), Some("AICX_TEST_EMBED_KEY"));
        let before = std::fs::read_to_string(&path).unwrap();

        let refused = [
            EmbedderOnboardingSelection {
                cloud_api_key_env: "sk-pasted-secret-value".into(),
                ..cloud.clone()
            },
            EmbedderOnboardingSelection {
                cloud_url: "https://user:hunter2@embed.example.test/v1".into(),
                ..cloud.clone()
            },
            EmbedderOnboardingSelection {
                cloud_url: "https://embed.example.test/v1?key=hunter2".into(),
                ..cloud.clone()
            },
            EmbedderOnboardingSelection {
                backend: "mystery".into(),
                ..cloud.clone()
            },
            EmbedderOnboardingSelection {
                profile: "huge".into(),
                ..cloud.clone()
            },
        ];
        for selection in &refused {
            let error = format!("{:#}", write_embedder_config(&home, selection).unwrap_err());
            assert!(!error.contains("sk-pasted"), "secret echoed: {error}");
            assert!(!error.contains("hunter2"), "secret echoed: {error}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
        }

        let bare = scratch_dir("embedder-cloud-bare");
        let without_url = EmbedderOnboardingSelection {
            backend: "cloud".into(),
            ..Default::default()
        };
        assert!(write_embedder_config(&bare, &without_url).is_err());
        assert!(!bare.join("config.toml").exists());
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&bare);
    }

    #[test]
    fn wiring_points_selected_clients_at_the_service_and_keeps_everything_else() {
        let user_home = scratch_dir("wire");
        let claude = user_home.join(".claude/settings.json");
        write_file(
            &claude,
            r#"{"theme":"dark","mcpServers":{"aicx":{"command":"/old/aicx-mcp","args":[]},"other":{"command":"keep-me"}}}"#,
        );
        let agy = user_home.join(".gemini/config/mcp_config.json");
        write_file(
            &agy,
            r#"{"mcpServers":{"aicx":{"disabled":true,"serverUrl":"http://127.0.0.1:1/mcp"},"context7":{"disabled":false,"serverUrl":"https://example.test/mcp"}}}"#,
        );
        let selected: Vec<String> = ["claude", "codex", "gemini", "agy"]
            .into_iter()
            .map(String::from)
            .collect();

        let report = wire_mcp_clients_in(&user_home, 18044, &selected);
        assert_eq!(report.wired, ["claude", "gemini", "agy"]);
        assert_eq!(report.skipped, ["codex"]);
        assert!(report.failed.is_empty(), "{:?}", report.failed);
        assert!(
            !user_home.join(".codex").exists(),
            "a client that is not installed gets nothing created for it"
        );

        let wired = read_json(&claude);
        assert_eq!(
            wired["mcpServers"]["aicx"],
            serde_json::json!({ "url": "http://127.0.0.1:18044/mcp" })
        );
        assert_eq!(wired["mcpServers"]["other"]["command"], "keep-me");
        assert_eq!(wired["theme"], "dark");
        let gemini = user_home.join(".gemini/settings.json");
        assert_eq!(
            read_json(&gemini)["mcpServers"]["aicx"],
            serde_json::json!({ "url": "http://127.0.0.1:18044/mcp" })
        );
        let wired = read_json(&agy);
        assert_eq!(
            wired["mcpServers"]["aicx"],
            serde_json::json!({ "serverUrl": "http://127.0.0.1:18044/mcp", "disabled": false })
        );
        assert_eq!(
            wired["mcpServers"]["context7"]["serverUrl"],
            "https://example.test/mcp"
        );

        let before: Vec<String> = [&claude, &gemini, &agy]
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        let again = wire_mcp_clients_in(&user_home, 18044, &selected);
        assert_eq!(again, report, "wiring twice changes nothing");
        let after: Vec<String> = [&claude, &gemini, &agy]
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        assert_eq!(before, after);

        let clients = inspect_mcp_clients(&user_home, 18044);
        let ids: Vec<&str> = clients.iter().map(|client| client.id).collect();
        assert_eq!(ids, ["claude", "codex", "gemini", "agy"]);
        assert!(clients[0].detected && clients[0].wired);
        assert!(!clients[1].detected && !clients[1].wired);
        assert!(clients[2].detected && clients[2].wired);
        assert!(clients[3].detected && clients[3].wired);
        assert!(
            inspect_mcp_clients(&user_home, 18045)
                .iter()
                .all(|client| !client.wired),
            "an entry on another port is not wired to this service"
        );
        let _ = std::fs::remove_dir_all(&user_home);
    }

    #[test]
    fn wiring_keeps_an_existing_token_on_a_matching_entry() {
        let user_home = scratch_dir("wire-token");
        let claude = user_home.join(".claude/settings.json");
        let body = r#"{"mcpServers":{"aicx":{"url":"http://127.0.0.1:18044/mcp","headers":{"Authorization":"Bearer unit-test-token"}}}}"#;
        write_file(&claude, body);
        let report = wire_mcp_clients_in(&user_home, 18044, &["claude".to_string()]);
        assert_eq!(report.wired, ["claude"]);
        assert_eq!(std::fs::read_to_string(&claude).unwrap(), body);

        let moved = wire_mcp_clients_in(&user_home, 18045, &["claude".to_string()]);
        assert_eq!(moved.wired, ["claude"]);
        let entry = read_json(&claude)["mcpServers"]["aicx"].clone();
        assert_eq!(entry["url"], "http://127.0.0.1:18045/mcp");
        assert_eq!(entry["headers"]["Authorization"], "Bearer unit-test-token");
        let _ = std::fs::remove_dir_all(&user_home);
    }

    #[test]
    fn wiring_refuses_what_it_cannot_read_and_ignores_unknown_clients() {
        let user_home = scratch_dir("wire-refuse");
        let broken = user_home.join(".codex/settings.json");
        write_file(&broken, "{ not json");
        let not_object = user_home.join(".claude/settings.json");
        write_file(&not_object, "[1, 2]");
        let report = wire_mcp_clients_in(
            &user_home,
            18044,
            &[
                "codex".to_string(),
                "claude".to_string(),
                "../../etc/passwd".to_string(),
            ],
        );
        assert!(report.wired.is_empty());
        assert_eq!(report.failed.len(), 3, "{:?}", report.failed);
        assert!(report.failed[0].starts_with("codex:"));
        assert!(report.failed[1].starts_with("claude:"));
        assert!(report.failed[2].contains("unknown MCP client"));
        assert_eq!(std::fs::read_to_string(&broken).unwrap(), "{ not json");
        assert_eq!(std::fs::read_to_string(&not_object).unwrap(), "[1, 2]");
        let _ = std::fs::remove_dir_all(&user_home);
    }

    #[cfg(unix)]
    #[test]
    fn wiring_writes_through_a_symlink_and_keeps_the_file_mode() {
        use std::os::unix::fs::PermissionsExt;

        let user_home = scratch_dir("wire-symlink");
        let real = user_home.join("dotfiles/claude-settings.json");
        write_file(&real, "{}\n");
        std::fs::set_permissions(&real, std::fs::Permissions::from_mode(0o640)).unwrap();
        let link = user_home.join(".claude/settings.json");
        std::fs::create_dir_all(link.parent().unwrap()).unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let report = wire_mcp_clients_in(&user_home, 18044, &["claude".to_string()]);
        assert_eq!(report.wired, ["claude"]);
        assert!(
            std::fs::symlink_metadata(&link)
                .unwrap()
                .file_type()
                .is_symlink()
        );
        assert_eq!(
            read_json(&real)["mcpServers"]["aicx"]["url"],
            "http://127.0.0.1:18044/mcp"
        );
        assert_eq!(
            std::fs::metadata(&real).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let _ = std::fs::remove_dir_all(&user_home);
    }

    #[test]
    fn source_shape_follows_the_catalog_rules() {
        let session = "99999999-9999-4999-8999-999999999999";
        for (kind, path, expected) in [
            (
                AgentKind::Claude,
                "/h/.claude/projects/p/s.jsonl".to_string(),
                true,
            ),
            (
                AgentKind::Claude,
                "/h/.claude/projects/p/notes.txt".to_string(),
                false,
            ),
            (
                AgentKind::Grok,
                "/h/.grok/sessions/x/chat_history.jsonl".to_string(),
                true,
            ),
            (
                AgentKind::Grok,
                "/h/.grok/sessions/x/updates.jsonl".to_string(),
                false,
            ),
            (
                AgentKind::Kimi,
                "/h/.kimi-code/sessions/x/wire.jsonl".to_string(),
                true,
            ),
            (
                AgentKind::Kimi,
                "/h/.kimi-code/sessions/x/other.jsonl".to_string(),
                false,
            ),
            (
                AgentKind::Gemini,
                "/h/.gemini/tmp/p/chats/session-1.json".to_string(),
                true,
            ),
            (
                AgentKind::Gemini,
                "/h/.gemini/tmp/p/logs.json".to_string(),
                false,
            ),
            (
                AgentKind::Cursor,
                format!("/h/.cursor/projects/p/agent-transcripts/{session}/{session}.jsonl"),
                true,
            ),
            (
                AgentKind::Cursor,
                "/h/.cursor/projects/p/agent-tools/t.jsonl".to_string(),
                false,
            ),
        ] {
            assert_eq!(is_source_file(kind, Path::new(&path)), expected, "{path}");
        }
    }

    #[test]
    fn source_count_stops_at_its_cap_and_says_so() {
        let root = scratch_dir("source-cap");
        let session_dir = root.join("project");
        std::fs::create_dir_all(&session_dir).unwrap();
        for index in 0..=SOURCE_FILE_CAP {
            std::fs::write(session_dir.join(format!("{index}.jsonl")), "").unwrap();
        }
        assert_eq!(
            count_source_files(std::slice::from_ref(&root), AgentKind::Claude),
            (SOURCE_FILE_CAP, true)
        );
        assert_eq!(
            count_source_files(&[root.join("absent")], AgentKind::Claude),
            (0, false)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn service_port_reads_plist_and_command_line_units() {
        let plist = "<string>--port</string>\n    <string>8044</string>\n    <string>--no-require-auth</string>";
        assert_eq!(service_definition_port(plist), Some(8044));
        assert_eq!(
            service_definition_port("--transport http --port 18044 --no-require-auth"),
            Some(18044)
        );
        assert_eq!(service_definition_port("--transport http"), None);
    }

    #[test]
    fn readiness_reads_the_machine_instead_of_guessing() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = scratch_dir("readiness");
        let home = root.join("aicx");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        write_file(&user_home.join(".claude/projects/p/one.jsonl"), "{}\n");
        write_file(&user_home.join(".claude/projects/p/notes.txt"), "");
        write_file(&user_home.join(".grok/sessions/s/chat_history.jsonl"), "");
        write_file(&user_home.join(".grok/sessions/s/updates.jsonl"), "");

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let service_port = listener.local_addr().unwrap().port();
        let unit = root.join("service-unit");
        write_file(
            &unit,
            &format!(
                "--transport http --host 127.0.0.1 --port {service_port} --no-require-auth --no-auto-refresh\n"
            ),
        );
        let _unit = ServiceUnitOverride::set(&unit);
        write_file(&reindex_unit_path(&user_home), "<plist/>\n");
        write_file(
            &user_home.join(".claude/settings.json"),
            &format!(
                r#"{{"mcpServers":{{"aicx":{{"url":"{}"}}}}}}"#,
                client_mcp_url(service_port)
            ),
        );
        write_file(&crate::catalog::sessions_path_for(&home), "{}\n{}\n\n");

        let state = inspect_onboarding_readiness_in(&home, &user_home, 8044, true);
        assert!(state.survey_required, "no phrase file and no embedder yet");
        assert!(!state.ready);
        assert!(state.rebuilding);
        assert_eq!(state.home_dir, home.display().to_string());

        let agents: Vec<&str> = state.sources.iter().map(|source| source.agent).collect();
        assert_eq!(
            agents,
            [
                "claude",
                "codex",
                "codescribe",
                "cursor",
                "gemini",
                "agy",
                "grok",
                "junie",
                "kimi",
                "copilot",
                "omp"
            ]
        );
        let row = |agent: &str| state.sources.iter().find(|s| s.agent == agent).unwrap();
        assert!(row("claude").detected);
        assert_eq!(row("claude").session_files, 1);
        assert_eq!(row("grok").session_files, 1);
        assert!(!row("codex").detected);
        assert_eq!(row("codex").session_files, 0);
        assert!(row("codex").root_path.ends_with("sessions"));

        assert!(!state.phrases.custom_file_exists);
        assert!(!state.phrases.intent_keywords.is_empty());
        assert!(!state.phrases.question_markers.is_empty());

        assert_eq!(state.embedder.backend, "gguf");
        assert_eq!(state.embedder.profile, "base");
        assert_eq!(state.embedder.cloud_url, DEFAULT_CLOUD_URL);
        assert_eq!(state.embedder.cloud_model, DEFAULT_CLOUD_MODEL);
        assert_eq!(state.embedder.cloud_api_key_env, DEFAULT_CLOUD_API_KEY_ENV);

        assert!(state.services.mcp_service_installed);
        assert!(state.services.mcp_service_healthy);
        assert_eq!(state.services.mcp_port, service_port);
        assert!(state.services.reindex_schedule_installed);
        drop(listener);

        assert!(state.mcp_clients[0].wired, "wired on the unit's port");
        assert!(state.index.catalog_exists);
        assert_eq!(state.index.catalog_sessions, 2);
        assert!(!state.index.index_exists);
        assert_eq!(state.index.indexed_documents, 0);

        let rendered = serde_json::to_value(&state).unwrap();
        assert!(
            rendered["sources"][0]
                .get("session_files_truncated")
                .is_none(),
            "the additive field only appears when a count was cut"
        );
        assert!(rendered["index"].get("last_error").is_none());

        write_file(&home.join("intent_phrases.toml"), "");
        write_embedder_choice(&home, "gguf", None).unwrap();
        let configured = inspect_onboarding_readiness_in(&home, &user_home, 8044, false);
        assert!(!configured.survey_required);
        assert!(!configured.ready, "no published index yet");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn suggestions_come_only_from_what_this_person_typed() {
        let root = scratch_dir("suggestions");
        let home = root.join("aicx");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        write_claude_session(
            &user_home,
            &[
                "decision: keep the loopback contract",
                "qwerty asdf zxcv sk-abcdefghijklmnopqrstuvwxyz0123456789",
                "qwerty asdf zxcv beta",
                "src/main.rs: this path is not an opener",
            ],
        );

        let found = discover_phrase_suggestions_in(&home, &user_home, Duration::from_secs(60));
        assert_eq!(found.scanned_sessions, 1);
        assert_eq!(found.scanned_utterances, 4);

        let decision = found
            .matched_samples
            .iter()
            .find(|sample| sample.entry_type == "decision")
            .expect("the dictionary-independent decision line matches");
        assert_eq!(decision.text, "decision: keep the loopback contract");
        assert!((decision.confidence - 0.95).abs() < f32::EPSILON);

        let openers: Vec<&str> = found
            .candidate_openers
            .iter()
            .map(|opener| opener.phrase.as_str())
            .collect();
        assert_eq!(
            openers,
            ["qwerty asdf zxcv"],
            "the two-word prefix drops out when every use continues the same way"
        );
        let opener = &found.candidate_openers[0];
        assert_eq!(opener.count, 2);
        assert_eq!(opener.suggested_bucket, "intent_keywords");
        assert!(opener.sample_line.starts_with("qwerty asdf zxcv"));
        assert!(
            !opener.sample_line.contains("sk-abcdef"),
            "a sample is redacted before it leaves the process"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_machine_with_no_sessions_gets_no_invented_suggestions() {
        let root = scratch_dir("suggestions-empty");
        let home = root.join("aicx");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&user_home).unwrap();
        let found = discover_phrase_suggestions_in(&home, &user_home, Duration::from_secs(60));
        assert_eq!(found.scanned_sessions, 0);
        assert_eq!(found.scanned_utterances, 0);
        assert!(found.matched_samples.is_empty());
        assert!(found.candidate_openers.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn long_messages_and_pastes_are_not_how_a_person_talks() {
        let prompt = (0..=UTTERANCE_MESSAGE_LINE_CAP)
            .map(|line| format!("step {line}"))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(utterance_lines(&prompt).is_empty());
        assert!(utterance_lines(&"x".repeat(UTTERANCE_MESSAGE_BYTE_CAP + 1)).is_empty());
        assert_eq!(
            utterance_lines("  pierwsza linia \n\n druga linia"),
            ["pierwsza linia", "druga linia"]
        );
    }

    #[test]
    fn openers_hold_plain_words_only_and_skip_the_dictionary() {
        let mut openers = HashMap::new();
        tally_openers("./tools/run.sh --now", &mut openers);
        tally_openers("v2 is out", &mut openers);
        tally_openers("solo", &mut openers);
        assert!(openers.is_empty(), "{openers:?}");

        tally_openers("Weź, sprawdź dlaczego to pada", &mut openers);
        tally_openers("weź sprawdź czemu to pada?", &mut openers);
        tally_openers("czemu tak wolno działa?", &mut openers);
        tally_openers("czemu tak wolno idzie?", &mut openers);
        let known = BTreeSet::from(["weź sprawdź".to_string()]);
        let ranked = rank_openers(&openers, &known);
        let phrases: Vec<&str> = ranked.iter().map(|o| o.phrase.as_str()).collect();
        assert_eq!(phrases, ["czemu tak wolno"]);
        assert_eq!(ranked[0].suggested_bucket, "question_markers");
    }

    #[test]
    fn preview_reports_the_classifiers_verdict_and_writes_nothing() {
        let preview =
            preview_phrases_on_lines(&["decision: ship it".to_string(), "qwerty asdf".to_string()]);
        assert!(preview.results[0].matched);
        assert_eq!(preview.results[0].entry_types, ["decision"]);
        assert!((preview.results[0].max_confidence - 0.95).abs() < f32::EPSILON);
        assert!(!preview.results[1].matched);
        assert!(preview.results[1].entry_types.is_empty());
        assert_eq!(preview.results[1].max_confidence, 0.0);

        let many = vec!["qwerty asdf".to_string(); PREVIEW_LINE_CAP + 1];
        assert_eq!(
            preview_phrases_on_lines(&many).results.len(),
            PREVIEW_LINE_CAP
        );
    }

    #[test]
    fn marker_phrases_keep_the_trailing_space_that_gives_them_meaning() {
        let current = crate::parser::intent_phrases::embedded_source();
        let keywords = ["  spaced keyword  ".to_string()];
        let markers = ["  czemu ".to_string()];
        let merged = merge_phrase_categories(
            current,
            &[
                ("intent", "keywords", keywords.as_slice()),
                ("question", "markers", markers.as_slice()),
            ],
        )
        .unwrap();
        let value: toml::Value = toml::from_str(&merged).unwrap();
        let intent = phrase_category(&value, "intent", "keywords");
        assert!(intent.iter().any(|phrase| phrase == "spaced keyword"));
        let question = phrase_category(&value, "question", "markers");
        assert!(question.iter().any(|phrase| phrase == "czemu "));

        let again =
            merge_phrase_categories(&merged, &[("question", "markers", markers.as_slice())])
                .unwrap();
        assert_eq!(phrase_total(&again), phrase_total(&merged));

        let too_long = ["x".repeat(PHRASE_CHAR_CAP + 1)];
        assert!(
            merge_phrase_categories(current, &[("intent", "keywords", too_long.as_slice())])
                .is_err()
        );
    }

    #[test]
    fn full_payload_writes_every_requested_pillar_and_finishes_onboarding() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = scratch_dir("full-payload");
        let home = root.join("aicx");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(user_home.join(".claude")).unwrap();
        std::fs::create_dir_all(user_home.join(".gemini/config")).unwrap();
        let _unit = ServiceUnitOverride::set(&root.join("missing-unit"));
        let request: OnboardingRequest = serde_json::from_value(serde_json::json!({
            "phrases": ["full payload opener"],
            "intent_keywords": ["full payload keyword"],
            "task_action_heads": ["dopnij to"],
            "decision_policy": ["od dziś zawsze"],
            "requirement_markers": ["koniecznie "],
            "question_markers": ["czemu "],
            "embedder": {
                "backend": "gguf",
                "profile": "1.7b",
                "cloud_url": "",
                "cloud_model": "",
                "cloud_api_key_env": "",
            },
            "install_services": true,
            "install_reindex_schedule": true,
            "wire_mcp_clients": true,
            "selected_mcp_clients": ["claude", "codex", "agy"],
            "build_initial_index": true,
        }))
        .unwrap();

        let outcome =
            apply_onboarding_request_in(&home, &user_home, &request, &dry_run_options()).unwrap();

        let embedded = crate::parser::intent_phrases::embedded_source();
        assert_eq!(outcome.added, 6);
        assert_eq!(outcome.total, phrase_total(embedded) + 6);
        assert_eq!(outcome.phrases_path, home.join("intent_phrases.toml"));
        assert_eq!(outcome.config_path, home.join("config.toml"));
        assert!(matches!(outcome.service, ServiceInstall::Skipped { .. }));
        assert!(!outcome.services_installed, "dry-run installed nothing");
        assert!(!outcome.reindex_installed);
        assert_eq!(outcome.wired_clients, ["claude", "agy"]);
        assert!(!outcome.session_files_found);
        assert!(
            outcome
                .warnings
                .iter()
                .any(|warning| warning.contains("reindex schedule: dry-run"))
        );
        assert!(
            outcome
                .warnings
                .iter()
                .any(|warning| warning.contains("codex: not found"))
        );

        assert!(home.join(MARKER_REL).is_file());
        let phrases = inspect_phrases(&home);
        assert!(phrases.custom_file_exists);
        assert!(
            phrases
                .intent_keywords
                .iter()
                .any(|p| p == "full payload opener")
        );
        assert!(
            phrases
                .intent_keywords
                .iter()
                .any(|p| p == "full payload keyword")
        );
        assert!(phrases.task_action_heads.iter().any(|p| p == "dopnij to"));
        assert!(
            phrases
                .decision_policy
                .iter()
                .any(|p| p == "od dziś zawsze")
        );
        assert!(
            phrases
                .requirement_markers
                .iter()
                .any(|p| p == "koniecznie ")
        );
        assert!(phrases.question_markers.iter().any(|p| p == "czemu "));
        assert_eq!(
            read_toml(&outcome.config_path)["embedder"]["profile"].as_str(),
            Some("dev")
        );
        assert_eq!(
            read_json(&user_home.join(".claude/settings.json"))["mcpServers"]["aicx"]["url"],
            "http://127.0.0.1:8044/mcp"
        );
        restore_embedded_phrases();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn phrases_only_payload_keeps_its_strict_contract() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = scratch_dir("legacy-payload");
        let home = root.join("aicx");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        let _unit = ServiceUnitOverride::set(&root.join("missing-unit"));
        let request: OnboardingRequest =
            serde_json::from_value(serde_json::json!({ "phrases": ["legacy save opener"] }))
                .unwrap();

        let outcome =
            apply_onboarding_request_in(&home, &user_home, &request, &dry_run_options()).unwrap();
        assert_eq!(outcome.added, 1);
        assert!(outcome.warnings.is_empty());
        assert_eq!(embedder_backend(&home).as_deref(), Some("gguf"));
        assert!(home.join(MARKER_REL).is_file());

        let repeated = apply_onboarding_request_in(&home, &user_home, &request, &dry_run_options())
            .unwrap_err();
        assert_eq!(repeated, "add at least one new intent phrase");
        restore_embedded_phrases();
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_save_with_no_new_phrases_still_finishes_and_keeps_the_dictionary_file() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = scratch_dir("no-new-phrases");
        let home = root.join("aicx");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        let _unit = ServiceUnitOverride::set(&root.join("missing-unit"));
        let request: OnboardingRequest =
            serde_json::from_value(serde_json::json!({ "install_services": false })).unwrap();

        let outcome =
            apply_onboarding_request_in(&home, &user_home, &request, &dry_run_options()).unwrap();
        assert_eq!(outcome.added, 0);
        assert_eq!(
            std::fs::read_to_string(&outcome.phrases_path).unwrap(),
            crate::parser::intent_phrases::embedded_source()
        );
        assert_eq!(embedder_backend(&home).as_deref(), Some("gguf"));
        assert!(home.join(MARKER_REL).is_file());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn failed_service_install_leaves_onboarding_unfinished() {
        let _unit_guard = SERVICE_UNIT_ENV_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let root = scratch_dir("full-install-fails");
        let home = root.join("aicx");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(user_home.join(".claude")).unwrap();
        let _unit = ServiceUnitOverride::set(&root.join("missing-unit"));
        let script = root.join("install-mcp-service.sh");
        write_file(&script, "#!/bin/sh\necho installer boom >&2\nexit 1\n");
        let request: OnboardingRequest = serde_json::from_value(serde_json::json!({
            "wire_mcp_clients": true,
            "selected_mcp_clients": ["claude"],
        }))
        .unwrap();

        let error = apply_onboarding_request_in(
            &home,
            &user_home,
            &request,
            &InstallOptions {
                dry_run: false,
                platform_is_macos: cfg!(target_os = "macos"),
                script: Some(script),
                bin: Some(root.join("aicx-under-test")),
                port: 8044,
            },
        )
        .unwrap_err();
        assert!(error.starts_with("install:"), "{error}");
        assert!(!home.join(MARKER_REL).exists());
        assert!(
            !user_home.join(".claude/settings.json").exists(),
            "no client is wired to a service that did not install"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reindex_schedule_stays_away_from_launchd_when_it_should() {
        assert!(EMBEDDED_REINDEX_SCHEDULE_INSTALLER.contains("com.loctree.aicx.reindex"));
        let root = scratch_dir("reindex");
        let home = root.join("custom-home");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&user_home).unwrap();

        let dry = install_reindex_schedule_in(&home, &user_home, &dry_run_options());
        assert!(
            matches!(&dry, ServiceInstall::Skipped { reason } if reason.contains("dry-run")),
            "{dry:?}"
        );
        // A home the launchd job would not index. Never reaches launchctl: the
        // job runs without AICX_HOME, and non-macOS stops at the platform check.
        let live = InstallOptions {
            dry_run: false,
            ..dry_run_options()
        };
        let elsewhere = install_reindex_schedule_in(&home, &user_home, &live);
        assert!(
            matches!(elsewhere, ServiceInstall::Skipped { .. }),
            "{elsewhere:?}"
        );
        let not_mac = InstallOptions {
            dry_run: false,
            platform_is_macos: false,
            ..dry_run_options()
        };
        let other_platform = install_reindex_schedule_in(&home, &user_home, &not_mac);
        assert!(
            matches!(&other_platform, ServiceInstall::Skipped { reason } if reason.contains("launchd-only")),
            "{other_platform:?}"
        );
        assert!(!reindex_unit_path(&user_home).exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn reindex_job_runs_the_cli_binary_not_the_mcp_server() {
        let root = scratch_dir("reindex-bin");
        let cli = root.join("aicx");
        let server = root.join("aicx-mcp");
        write_file(&server, "");
        let options = |bin: PathBuf| InstallOptions {
            bin: Some(bin),
            ..dry_run_options()
        };
        assert_eq!(reindex_job_binary(&options(server.clone())), None);
        write_file(&cli, "");
        assert_eq!(reindex_job_binary(&options(server)), Some(cli.clone()));
        assert_eq!(reindex_job_binary(&options(cli.clone())), Some(cli));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn initial_build_refuses_a_machine_with_no_sessions() {
        let root = scratch_dir("empty-index");
        let home = root.join("aicx");
        let user_home = root.join("user");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&user_home).unwrap();
        assert!(build_initial_index(&home, &user_home).is_err());
        let _ = std::fs::remove_dir_all(&root);
    }
}
