#![cfg(unix)]

use std::os::fd::FromRawFd;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

#[test]
fn aicx_help_exits_quietly_when_stdout_reader_is_gone() {
    let mut fds = [0; 2];
    let pipe_result = unsafe { libc::pipe(fds.as_mut_ptr()) };
    assert_eq!(pipe_result, 0, "pipe() should succeed");

    unsafe {
        libc::close(fds[0]);
    }

    let output = unsafe {
        Command::new(env!("CARGO_BIN_EXE_aicx"))
            .arg("--help")
            .stdout(Stdio::from_raw_fd(fds[1]))
            .stderr(Stdio::piped())
            .output()
            .expect("spawn aicx --help with closed stdout reader")
    };

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        !stderr.contains("panicked") && !stderr.contains("Broken pipe"),
        "closed stdout must not render a Rust panic, stderr was:\n{stderr}"
    );
    assert_eq!(
        output.status.signal(),
        Some(libc::SIGPIPE),
        "closed stdout should terminate the CLI through SIGPIPE"
    );
}
