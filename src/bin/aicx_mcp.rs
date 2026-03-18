//! Standalone MCP server binary for aicx.
//!
//! Exposes aicx search, rank, and steer as MCP tools.
//! Supports stdio (default) and streamable HTTP transports.
//!
//! Usage:
//!   aicx-mcp                          # stdio transport
//!   aicx-mcp --transport http         # streamable HTTP on port 8044
//!   aicx-mcp --transport http --port 9000
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use ai_contexters::mcp::{self, McpTransport};
use std::io::Write as _;
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

    match rt.block_on(async { mcp::run_transport(args.transport, args.port).await }) {
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
