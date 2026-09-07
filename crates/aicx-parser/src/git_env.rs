//! Repo-scoped git spawning that cannot be hijacked by the caller's session.
//!
//! Git hooks (pre-push, pre-commit) and some IDE terminals export `GIT_DIR`,
//! `GIT_WORK_TREE`, `GIT_INDEX_FILE` and friends. A child `git -C <path> …`
//! inherits them and silently operates on the *calling* repository instead of
//! `<path>`, so repo-identity inference misattributes or fails. Every git spawn
//! in this crate goes through [`git_command_isolated`]; the application crate
//! re-exports it so there is exactly one list of variables to keep honest.

use std::process::Command;

/// Environment variables that redirect git away from `-C <path>`.
pub const GIT_SESSION_VARS: &[&str] = &[
    "GIT_DIR",
    "GIT_COMMON_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_QUARANTINE_PATH",
    "GIT_PREFIX",
];

/// A `git` command with every session-scoped repository variable removed.
pub fn git_command_isolated() -> Command {
    let mut cmd = Command::new("git");
    for var in GIT_SESSION_VARS {
        cmd.env_remove(var);
    }
    cmd
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn isolated_command_clears_every_session_variable() {
        let cmd = git_command_isolated();
        let removed: Vec<&OsStr> = cmd
            .get_envs()
            .filter(|(_, value)| value.is_none())
            .map(|(key, _)| key)
            .collect();
        for var in GIT_SESSION_VARS {
            assert!(
                removed.iter().any(|key| *key == OsStr::new(var)),
                "{var} must be removed from the child environment"
            );
        }
        assert_eq!(cmd.get_program(), OsStr::new("git"));
    }
}
