//! Process spawning helpers shared across feature gates.

use std::process::Command;

/// Git session env vars (set by hooks like pre-push) must never leak into
/// repo-scoped queries: `git -C <path>` with `GIT_DIR` present operates on
/// the *calling* repo, misattributing or failing outright.
pub fn git_command_isolated() -> Command {
    let mut cmd = Command::new("git");
    for var in [
        "GIT_DIR",
        "GIT_COMMON_DIR",
        "GIT_WORK_TREE",
        "GIT_INDEX_FILE",
        "GIT_OBJECT_DIRECTORY",
        "GIT_ALTERNATE_OBJECT_DIRECTORIES",
        "GIT_QUARANTINE_PATH",
        "GIT_PREFIX",
    ] {
        cmd.env_remove(var);
    }
    cmd
}
