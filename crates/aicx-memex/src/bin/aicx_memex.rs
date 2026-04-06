//! Compatibility front door for the background memex/steer daemon surface.
//!
//! `aicx-memex` keeps a daemon-first operator experience while delegating the
//! real implementation to the main `aicx` CLI.

use std::ffi::OsString;
use std::process::{Command, ExitCode};

const ROOT_HELP: &str = "\
Background memex/steer daemon front door.

Usage:
  aicx-memex daemon [OPTIONS]
  aicx-memex status [OPTIONS]
  aicx-memex sync [OPTIONS]
  aicx-memex stop [OPTIONS]

Commands:
  daemon   Start the background indexer (alias for `aicx daemon`)
  status   Show daemon status (alias for `aicx daemon-status`)
  sync     Queue an immediate daemon sync cycle
  stop     Stop the background daemon cleanly

Examples:
  aicx-memex daemon
  aicx-memex daemon --foreground --project ai-contexters
  aicx-memex status --json
  aicx-memex sync
  aicx-memex stop

Use `aicx-memex <command> --help` for the full delegated help.
";

#[derive(Debug)]
enum Dispatch {
    Help,
    Version,
    Args(Vec<OsString>),
}

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let dispatch = match dispatch_args(&args) {
        Ok(dispatch) => dispatch,
        Err(message) => {
            eprintln!("{message}\n");
            eprint!("{ROOT_HELP}");
            return ExitCode::FAILURE;
        }
    };

    match dispatch {
        Dispatch::Help => {
            print!("{ROOT_HELP}");
            ExitCode::SUCCESS
        }
        Dispatch::Version => {
            println!("{}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Dispatch::Args(forwarded_args) => run_aicx(forwarded_args),
    }
}

fn run_aicx(args: Vec<OsString>) -> ExitCode {
    let aicx = aicx_memex::daemon::find_aicx_binary();
    let status = match Command::new(&aicx).args(&args).status() {
        Ok(status) => status,
        Err(err) => {
            eprintln!("Failed to launch {}: {err}", aicx.to_string_lossy());
            return ExitCode::FAILURE;
        }
    };

    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .map(ExitCode::from)
        .unwrap_or_else(|| {
            if status.success() {
                ExitCode::SUCCESS
            } else {
                ExitCode::FAILURE
            }
        })
}

fn dispatch_args(args: &[OsString]) -> Result<Dispatch, String> {
    match args.first().and_then(as_str) {
        None => Ok(Dispatch::Help),
        Some("--help" | "-h" | "help") => Ok(Dispatch::Help),
        Some("--version" | "-V" | "version") => Ok(Dispatch::Version),
        Some("daemon") => Ok(Dispatch::Args(prefix_command("daemon", &args[1..]))),
        Some("status" | "daemon-status") => {
            Ok(Dispatch::Args(prefix_command("daemon-status", &args[1..])))
        }
        Some("sync" | "daemon-sync") => {
            Ok(Dispatch::Args(prefix_command("daemon-sync", &args[1..])))
        }
        Some("stop" | "daemon-stop") => {
            Ok(Dispatch::Args(prefix_command("daemon-stop", &args[1..])))
        }
        Some("run" | "daemon-run") => Ok(Dispatch::Args(prefix_command("daemon-run", &args[1..]))),
        Some(other) => Err(format!("Unknown aicx-memex subcommand '{other}'")),
    }
}

fn prefix_command(command: &str, rest: &[OsString]) -> Vec<OsString> {
    let mut forwarded = Vec::with_capacity(rest.len() + 1);
    forwarded.push(OsString::from(command));
    forwarded.extend(rest.iter().cloned());
    forwarded
}

fn as_str(value: &OsString) -> Option<&str> {
    value.as_os_str().to_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os_vec(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn dispatch_args_maps_daemon_aliases_to_aicx_subcommands() {
        let dispatch = dispatch_args(&os_vec(&["status", "--json"])).expect("dispatch");
        let Dispatch::Args(args) = dispatch else {
            panic!("status should dispatch to args");
        };
        assert_eq!(args, os_vec(&["daemon-status", "--json"]));

        let dispatch = dispatch_args(&os_vec(&["sync"])).expect("dispatch");
        let Dispatch::Args(args) = dispatch else {
            panic!("sync should dispatch to args");
        };
        assert_eq!(args, os_vec(&["daemon-sync"]));
    }

    #[test]
    fn dispatch_args_handles_root_help_and_version() {
        assert!(matches!(dispatch_args(&[]).unwrap(), Dispatch::Help));
        assert!(matches!(
            dispatch_args(&os_vec(&["--version"])).unwrap(),
            Dispatch::Version
        ));
    }

    #[test]
    fn dispatch_args_rejects_unknown_subcommands() {
        let err = dispatch_args(&os_vec(&["explode"])).expect_err("unknown command should fail");
        assert!(err.contains("Unknown aicx-memex subcommand"));
    }
}
