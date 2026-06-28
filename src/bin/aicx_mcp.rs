//! Standalone MCP server binary for aicx.
//!
//! Exposes aicx search, rank, and steer as MCP tools.
//! Supports stdio (default) and streamable HTTP transports.
//!
//! Usage:
//!   aicx-mcp                          # stdio transport
//!   aicx-mcp --transport http         # streamable HTTP on port 8044
//!   aicx-mcp --transport http --port 9000
//!   aicx-mcp --transport http --host 0.0.0.0 --port 9000
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use aicx::auth;
use aicx::mcp::{self, McpHttpConfig, McpTransport};
use std::io::Write as _;
use std::net::IpAddr;
use std::panic;
use std::process::ExitCode;

use clap::Parser;

/// aicx MCP server — AI session context as MCP tools
#[derive(Parser)]
#[command(name = "aicx-mcp")]
#[command(author = "M&K (c)2026 VetCoders")]
#[command(version)]
struct Args {
    /// Transport: stdio (default) or http. Legacy alias: sse.
    #[arg(long, value_enum, default_value_t = McpTransport::Stdio)]
    transport: McpTransport,

    /// Port for streamable HTTP transport
    #[arg(long, default_value = "8044")]
    port: u16,

    /// Host/IP for streamable HTTP transport (default: 127.0.0.1)
    #[arg(long, default_value = "127.0.0.1")]
    host: IpAddr,

    /// Allowed HTTP Host header for streamable HTTP clients. Repeat for remote hostnames/IPs.
    #[arg(long = "allowed-host", value_name = "HOST")]
    allowed_hosts: Vec<String>,

    /// Disable HTTP Host header validation. Not recommended outside trusted networks.
    #[arg(long)]
    allow_any_host: bool,

    /// Optional explicit auth token (overrides env / file / generated). HTTP transport only.
    #[arg(long, value_name = "TOKEN")]
    auth_token: Option<String>,

    /// Require Bearer auth on HTTP transport (default: true). Pass `--no-require-auth` to opt out.
    #[arg(long, default_value_t = true, action = clap::ArgAction::Set)]
    require_auth: bool,
}

// Safe stderr logging — never panics, even if stderr is closed.
fn safe_stderr_log(line: &str) {
    let mut stderr = std::io::stderr().lock();
    let _ = stderr.write_all(line.as_bytes());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
}

fn install_panic_hook() {
    panic::set_hook(Box::new(|panic_info| {
        let msg = if let Some(s) = panic_info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else if let Some(s) = panic_info.payload().downcast_ref::<String>() {
            s.clone()
        } else {
            "Unknown panic".to_string()
        };

        if msg.contains("Broken pipe") || msg.contains("os error 32") {
            safe_stderr_log("[aicx-mcp] Client disconnected (broken pipe), shutting down");
            std::process::exit(0);
        } else {
            let location = panic_info
                .location()
                .map(|loc| format!(" at {}:{}:{}", loc.file(), loc.line(), loc.column()))
                .unwrap_or_default();
            safe_stderr_log(&format!("[aicx-mcp] Panic{}: {}", location, msg));
        }
    }));
}

#[cfg(unix)]
fn ignore_sigpipe() {
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
    }
}

#[cfg(not(unix))]
fn ignore_sigpipe() {}

fn main() -> ExitCode {
    ignore_sigpipe();
    install_panic_hook();

    let args = Args::parse();

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            safe_stderr_log(&format!("[aicx-mcp] Failed to create runtime: {e}"));
            return ExitCode::FAILURE;
        }
    };

    let auth_config = match auth::load_auth_config(args.auth_token.as_deref(), args.require_auth) {
        Ok(cfg) => cfg,
        Err(e) => {
            safe_stderr_log(&format!("[aicx-mcp] Failed to load auth config: {e:#}"));
            return ExitCode::FAILURE;
        }
    };
    if matches!(args.transport, McpTransport::Http) && !args.require_auth {
        safe_stderr_log(
            "[aicx-mcp] WARNING: HTTP transport bound without auth (--no-require-auth)",
        );
    }
    if matches!(args.transport, McpTransport::Http) && args.allow_any_host {
        safe_stderr_log("[aicx-mcp] WARNING: HTTP Host validation disabled (--allow-any-host)");
    }
    let http_config = McpHttpConfig {
        host: args.host,
        port: args.port,
        allowed_hosts: args.allowed_hosts,
        allow_any_host: args.allow_any_host,
    };
    match rt.block_on(async { mcp::run_transport(args.transport, http_config, auth_config).await })
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            let err_str = format!("{e:?}");
            if err_str.contains("Broken pipe") || err_str.contains("os error 32") {
                safe_stderr_log("[aicx-mcp] Client disconnected, shutting down");
                ExitCode::SUCCESS
            } else {
                safe_stderr_log(&format!("[aicx-mcp] Error: {e:#}"));
                ExitCode::FAILURE
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn http_host_defaults_to_loopback() {
        let args = Args::try_parse_from(["aicx-mcp", "--transport", "http"])
            .expect("http transport should parse");

        assert_eq!(args.host, IpAddr::V4(Ipv4Addr::LOCALHOST));
        assert_eq!(args.port, 8044);
    }

    #[test]
    fn http_host_accepts_all_interfaces() {
        let args = Args::try_parse_from([
            "aicx-mcp",
            "--transport",
            "http",
            "--host",
            "0.0.0.0",
            "--allowed-host",
            "mcp.example.internal",
            "--port",
            "9000",
        ])
        .expect("explicit http host should parse");

        assert_eq!(args.host, IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        assert_eq!(args.allowed_hosts, vec!["mcp.example.internal"]);
        assert_eq!(args.port, 9000);
    }
}
