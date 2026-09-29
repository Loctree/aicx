//! First `aicx` after install opens the dashboard on 127.0.0.1:8044.
//!
//! Onboarding finishes by writing three existing stores: `intent_phrases.toml`,
//! `[embedder]` in `config.toml`, and the native loopback service. It does not
//! run from npm. What's new is the current CHANGELOG section, the same body
//! `make release-prepare` writes to `dist/release-notes.md`.

use anyhow::{Context, Result};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Carried inside the binary. npm install does not run this; a bare `aicx` does.
const EMBEDDED_MCP_SERVICE_INSTALLER: &str = include_str!("../tools/install-mcp-service.sh");
const EMBEDDED_LINUX_SERVICE_INSTALLER: &str =
    include_str!("../tools/install-mcp-service-linux.sh");
const EMBEDDED_WINDOWS_SERVICE_INSTALLER: &str = include_str!("../tools/install-mcp-service.ps1");
const CHANGELOG: &str = include_str!("../CHANGELOG.md");

pub const DASHBOARD_URL: &str = "http://127.0.0.1:8044/";
const SERVICE_ERROR_REL: &str = "state/service-install-error";
const WHATS_NEW_OFFERED_REL: &str = "state/whats-new-offered";
const WHATS_NEW_ACKED_REL: &str = "state/whats-new-acked";

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
/// Auto-refresh is the experimental opt-in — never `--no-auto-refresh`.
pub fn loopback_http_argv(port: u16) -> Vec<String> {
    vec![
        "--transport".into(),
        "http".into(),
        "--host".into(),
        "127.0.0.1".into(),
        "--port".into(),
        port.to_string(),
        "--no-require-auth".into(),
        "--experimental-auto-refresh".into(),
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
        && has("--experimental-auto-refresh")
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
    std::fs::read_to_string(path).ok()
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
    let mut options = InstallOptions::from_env();
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
        bring_service_onto_contract(&home, &options)
    } else {
        ServiceInstall::Skipped {
            reason: "service already matches the loopback contract".to_string(),
        }
    };
    let url = dashboard_url_for(plan.port);
    let gui = gui_available();
    if !options.dry_run
        && let Err(err) = ensure_loopback_server(plan.port)
    {
        write_service_error(&home, Some(&err.to_string()));
        println!("{err}");
    }
    println!("{url}");
    if gui && !options.dry_run {
        // A missing browser is not a failure. The URL is already printed.
        let _ = open_dashboard(&url);
    }
    if !survey {
        offer_whats_new(&home)?;
    }
    mark_complete(&home)?;
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
        PortOccupancy::Free => {}
        PortOccupancy::Ours => return Ok(()),
        PortOccupancy::Foreign(listener) => {
            let plan = choose_service_port(std::slice::from_ref(&listener), port, &[]);
            let Some(foreign) = plan.foreign else {
                anyhow::bail!("port {port} is already taken");
            };
            anyhow::bail!("{}", foreign.instructions());
        }
    }
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
    Ok(())
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

pub fn locate_service_installer() -> Option<PathBuf> {
    if let Some(value) = std::env::var_os("AICX_MCP_SERVICE_INSTALLER") {
        let path = PathBuf::from(value);
        if path.is_file() {
            return Some(path);
        }
    }
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join("../../tools/install-mcp-service.sh"));
        candidates.push(dir.join("../tools/install-mcp-service.sh"));
    }
    candidates.push(PathBuf::from("tools/install-mcp-service.sh"));
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
                "$utf8 = New-Object System.Text.UTF8Encoding $false\n[System.IO.File]::WriteAllText('{}', $env:AICX_BIN, $utf8)\n[System.IO.File]::WriteAllText($env:AICX_SERVICE_UNIT, \"--transport http --host 127.0.0.1 --port 8044 --no-require-auth --experimental-auto-refresh`n\", $utf8)\nexit 0\n",
                log.display().to_string().replace('\'', "''")
            )
        } else {
            format!(
                "#!/bin/sh\nprintf '%s' \"$AICX_BIN\" > '{}'\nprintf '%s\\n' '--transport http --host 127.0.0.1 --port 8044 --no-require-auth --experimental-auto-refresh' > \"$AICX_SERVICE_UNIT\"\n",
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
    fn production_installer_resolves_loopback_argv_without_launchctl() {
        let script = locate_service_installer()
            .expect("repo checkout must resolve tools/install-mcp-service.sh without a test stub");
        assert!(
            script.file_name().and_then(|name| name.to_str()) == Some("install-mcp-service.sh"),
            "production installer must be install-mcp-service.sh, got {}",
            script.display()
        );
        assert!(
            script
                .to_string_lossy()
                .contains("tools/install-mcp-service"),
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
                "--experimental-auto-refresh".to_string(),
            ]
        );
        assert!(!argv.iter().any(|arg| arg == "0.0.0.0"));
        assert!(!argv.iter().any(|arg| arg == "--no-auto-refresh"));

        let body = std::fs::read_to_string(&script).expect("read installer");
        assert_eq!(body, EMBEDDED_MCP_SERVICE_INSTALLER);
        assert!(body.contains("HOST=\"${AICX_MCP_HOST:-127.0.0.1}\""));
        assert!(body.contains("PORT=\"${AICX_MCP_PORT:-8044}\""));
        assert!(body.contains("--experimental-auto-refresh"));
        assert!(body.contains("--no-require-auth"));
        assert!(!body.contains("0.0.0.0"));
        // Comment may mention the flag as forbidden; the ProgramArguments array must not.
        let program_args = body
            .split("<key>ProgramArguments</key>")
            .nth(1)
            .and_then(|rest| rest.split("</array>").next())
            .expect("ProgramArguments array");
        assert!(
            !program_args.contains("--no-auto-refresh"),
            "LaunchAgent argv must not disable auto-refresh"
        );
        assert!(program_args.contains("--experimental-auto-refresh"));
        assert!(program_args.contains("$HOST_XML"));
        assert!(program_args.contains("$PORT_XML"));

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
            "--transport http --host 127.0.0.1 --port 18044 --no-require-auth --experimental-auto-refresh\n",
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
}
