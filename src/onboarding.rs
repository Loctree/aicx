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
            "aicx onboarding\nOpen the dashboard: {url}\nOne phrase list (intent_phrases.toml), the local embedder in config.toml, and the loopback service.\nService: {service}\nSearch still works if that install fails: aicx search '<query>'\n",
            url = self.dashboard_url,
            service = self.service,
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceInstall {
    Installed,
    Skipped { reason: String },
    Failed { reason: String },
}

impl ServiceInstall {
    pub fn summary(&self) -> String {
        match self {
            Self::Installed => "launchd LaunchAgent installed (com.loctree.aicx.mcp)".to_string(),
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
}

impl InstallOptions {
    pub fn from_env() -> Self {
        Self {
            dry_run: env_flag("AICX_ONBOARDING_DRY_RUN"),
            platform_is_macos: cfg!(target_os = "macos"),
            script: locate_service_installer(),
            bin: std::env::current_exe().ok(),
        }
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
    let path = home.join("config.toml");
    let mut value: toml::Value = if path.is_file() {
        toml::from_str(&std::fs::read_to_string(&path)?)?
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
        && has("8044")
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

pub fn maybe_first_start() -> Result<FirstStart> {
    let home = crate::aicx_home::resolve()?;
    if !bare_start_opens_dashboard(&home) {
        return Ok(FirstStart::AlreadyConfigured);
    }
    let survey = needs_full_survey(&home);
    let home = crate::aicx_home::ensure()?;
    let options = InstallOptions::from_env();
    let service = if survey {
        let outcome = install_launch_agent(&options);
        match &outcome {
            ServiceInstall::Failed { reason } => write_service_error(&home, Some(reason)),
            ServiceInstall::Installed => write_service_error(&home, None),
            ServiceInstall::Skipped { .. } => {}
        }
        outcome
    } else {
        ServiceInstall::Skipped {
            reason: "service already matches the loopback contract".to_string(),
        }
    };
    let gui = gui_available();
    if !options.dry_run
        && let Err(err) = ensure_loopback_server()
    {
        write_service_error(&home, Some(&err.to_string()));
    }
    println!("{DASHBOARD_URL}");
    if gui && !options.dry_run {
        // A missing browser is not a failure. The URL is already printed.
        let _ = open_dashboard(DASHBOARD_URL);
    }
    if !survey {
        offer_whats_new(&home)?;
    }
    mark_complete(&home)?;
    Ok(FirstStart::Opened(FirstStartReport {
        dashboard_url: DASHBOARD_URL.to_string(),
        service: service.summary(),
    }))
}

/// Steady-state no-args behavior is not settled.
///
/// First-run and a new version open the dashboard. Later bare `aicx` calls
/// stay on the short front door until a founder note flips this to `true`.
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
        return spawn_installer(
            program,
            &[script_path.as_os_str().to_os_string()],
            options,
            None,
        );
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
        return ServiceInstall::Installed;
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

fn ensure_loopback_server() -> Result<()> {
    if loopback_dashboard_open() {
        return Ok(());
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
    let mut command = Command::new(bin);
    command
        .args([
            "serve",
            "--transport",
            "http",
            "--host",
            "127.0.0.1",
            "--port",
            "8044",
            "--no-require-auth",
            "--experimental-auto-refresh",
        ])
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

fn loopback_dashboard_open() -> bool {
    std::net::TcpStream::connect_timeout(
        &"127.0.0.1:8044"
            .parse()
            .expect("loopback dashboard address"),
        std::time::Duration::from_millis(200),
    )
    .is_ok()
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
    fn non_macos_and_dry_run_do_not_pretend_launchd_installed() {
        let skipped = install_launch_agent(&InstallOptions {
            dry_run: false,
            platform_is_macos: false,
            script: None,
            bin: None,
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
        let script = dir.join("install-mcp-service.sh");
        let log = dir.join("ran");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nprintf '%s' \"$AICX_BIN\" > '{}'\n",
                log.display()
            ),
        )
        .unwrap();
        let outcome = install_launch_agent(&InstallOptions {
            dry_run: false,
            platform_is_macos: true,
            script: Some(script),
            bin: Some(PathBuf::from("/tmp/aicx-under-test")),
        });
        assert_eq!(outcome, ServiceInstall::Installed);
        let recorded = std::fs::read_to_string(&log).unwrap();
        assert_eq!(recorded, "/tmp/aicx-under-test");
        let _ = std::fs::remove_dir_all(&dir);
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
        assert!(rendered.contains("intent_phrases.toml"));
        assert!(!rendered.contains("Usage: aicx"));
    }
}
