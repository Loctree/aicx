//! Cross-platform, fail-closed process-liveness probing.

/// Probe whether `pid` still belongs to a running process.
///
/// `Some(true)` means the process is running, `Some(false)` means it is known
/// to have exited, and `None` means the platform could not decide safely.
#[cfg(unix)]
pub(crate) fn probe_pid_liveness(pid: u32) -> Option<bool> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    // SAFETY: kill(pid, 0) performs existence/permission probing and does not
    // deliver a signal.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if result == 0 {
        return Some(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(libc::ESRCH) => Some(false),
        // EPERM means the process exists under another uid.
        Some(libc::EPERM) => Some(true),
        _ => None,
    }
}

#[cfg(windows)]
pub(crate) fn probe_pid_liveness(pid: u32) -> Option<bool> {
    if pid == 0 {
        return None;
    }

    const PROCESS_QUERY_LIMITED_INFORMATION: u32 = 0x1000;
    const SYNCHRONIZE: u32 = 0x0010_0000;
    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_INVALID_PARAMETER: i32 = 87;
    const WAIT_OBJECT_0: u32 = 0;
    const WAIT_TIMEOUT: u32 = 258;

    // SAFETY: FFI call; a non-null handle is closed before returning.
    let handle = unsafe {
        windows_ffi::OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZE, 0, pid)
    };
    if handle.is_null() {
        return match std::io::Error::last_os_error().raw_os_error() {
            // Protected system processes exist but cannot be opened here.
            Some(ERROR_ACCESS_DENIED) => Some(true),
            // For a non-zero pid, Windows reports a missing process this way.
            Some(ERROR_INVALID_PARAMETER) => Some(false),
            _ => None,
        };
    }

    // A zero-timeout wait disambiguates "running" from "exited with code 259
    // (STILL_ACTIVE)": the process handle is signaled if and only if the
    // process has terminated, regardless of the exit code it chose.
    // SAFETY: `handle` is valid; zero timeout never blocks.
    let wait = unsafe { windows_ffi::WaitForSingleObject(handle, 0) };
    // SAFETY: `handle` was returned by OpenProcess and is closed exactly once.
    unsafe { windows_ffi::CloseHandle(handle) };
    match wait {
        WAIT_OBJECT_0 => Some(false),
        WAIT_TIMEOUT => Some(true),
        _ => None,
    }
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn probe_pid_liveness(_pid: u32) -> Option<bool> {
    None
}

#[cfg(windows)]
mod windows_ffi {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        pub fn OpenProcess(
            dw_desired_access: u32,
            b_inherit_handle: i32,
            dw_process_id: u32,
        ) -> *mut c_void;
        pub fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        pub fn CloseHandle(object: *mut c_void) -> i32;
    }
}

#[cfg(test)]
pub(crate) fn exited_test_process() -> (u32, std::process::Child) {
    use std::process::{Command, Stdio};

    let mut child = Command::new(std::env::current_exe().expect("resolve current test binary"))
        .arg("process_liveness::tests::child_noop")
        .arg("--exact")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn liveness test child");
    let pid = child.id();
    let status = child.wait().expect("wait for liveness test child");
    assert!(status.success(), "liveness test child failed: {status}");
    (pid, child)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_process_is_alive() {
        assert_eq!(probe_pid_liveness(std::process::id()), Some(true));
    }

    #[test]
    fn exited_process_is_dead() {
        let (pid, _child) = exited_test_process();
        assert_eq!(probe_pid_liveness(pid), Some(false));
    }

    #[test]
    fn child_noop() {}
}
