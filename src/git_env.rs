//! Process spawning helpers shared across feature gates.
//!
//! The isolation primitive lives in `aicx_parser::git_env` so the parser crate
//! (repo-identity inference in `segmentation.rs`) and the application share
//! one list of session variables to strip. This module only re-exports it.

pub use aicx_parser::git_env::{GIT_SESSION_VARS, git_command_isolated};
