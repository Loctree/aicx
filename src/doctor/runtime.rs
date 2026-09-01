//! Repair the macOS AICX runtime ownership contract from one doctor command.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, Result, bail};
use serde::Serialize;

const REPAIR_MCP_SCRIPT: &str = include_str!("../../tools/repair-mcp-runtime.sh");
const INSTALL_REINDEX_SCRIPT: &str = include_str!("../../tools/install-reindex-schedule.sh");
const REINDEX_INTERVAL_SECONDS: &str = "8640";

#[derive(Debug, Serialize)]
pub struct RuntimeRepairReport {
    pub status: &'static str,
    pub launcher: PathBuf,
    pub mcp_service: String,
    pub index_scheduler: String,
    pub reindex_interval_seconds: u64,
}

fn public_launcher() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("AICX_RUNTIME_BIN").filter(|value| !value.is_empty()) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        bail!("AICX_RUNTIME_BIN is not a file: {}", path.display());
    }
    std::env::current_exe()
        .context("resolve the running `aicx` executable")
        .or_else(|_| which::which("aicx").context("resolve the public `aicx` launcher on PATH"))
}

fn run_script(script: &str, launcher: &Path, extra_env: Option<(&str, &str)>) -> Result<String> {
    let mut command = Command::new("/bin/bash");
    command
        .arg("-s")
        .env("AICX_BIN", launcher)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some((key, value)) = extra_env {
        command.env(key, value);
    }
    let mut child = command.spawn().context("start embedded runtime repair")?;
    child
        .stdin
        .take()
        .context("open runtime repair stdin")?
        .write_all(script.as_bytes())
        .context("send embedded runtime repair")?;
    let output = child
        .wait_with_output()
        .context("wait for embedded runtime repair")?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        bail!("runtime repair failed: {stderr}");
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn repair_runtime() -> Result<RuntimeRepairReport> {
    if !cfg!(target_os = "macos") {
        bail!("runtime repair currently supports macOS launchd installations only");
    }
    let launcher = public_launcher()?;
    let mcp_service = run_script(REPAIR_MCP_SCRIPT, &launcher, None)?;
    let index_scheduler = run_script(
        INSTALL_REINDEX_SCRIPT,
        &launcher,
        Some(("AICX_REINDEX_INTERVAL", REINDEX_INTERVAL_SECONDS)),
    )?;
    Ok(RuntimeRepairReport {
        status: "repaired",
        launcher,
        mcp_service,
        index_scheduler,
        reindex_interval_seconds: 8_640,
    })
}

pub fn format_runtime_repair_text(report: &RuntimeRepairReport) -> String {
    format!(
        "aicx runtime repaired\n\n{}\n{}\n\n  ownership: MCP serves requests; short-lived hot refresh/index runs every 2h24m\n",
        report.mcp_service, report.index_scheduler
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repair_preserves_existing_service_arguments() {
        assert!(REPAIR_MCP_SCRIPT.contains("Set :ProgramArguments:0"));
        assert!(REPAIR_MCP_SCRIPT.contains("--no-auto-refresh"));
        assert!(!REPAIR_MCP_SCRIPT.contains("--host\n"));
        assert!(!REPAIR_MCP_SCRIPT.contains("--port\n"));
    }

    #[test]
    fn repair_uses_short_lived_indexer_every_8640_seconds() {
        assert_eq!(REINDEX_INTERVAL_SECONDS, "8640");
        assert!(INSTALL_REINDEX_SCRIPT.contains("catalog refresh --json"));
        assert!(INSTALL_REINDEX_SCRIPT.contains("catalog_present\": false"));
        assert!(INSTALL_REINDEX_SCRIPT.contains("index"));
    }
}
