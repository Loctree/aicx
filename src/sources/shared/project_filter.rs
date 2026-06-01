//! Project filtering and identity resolution logic.
//!
//! Extracted during Faza 1 of the sources decomposition (2026-05-27).

// Placeholder - will be populated in subsequent edits of this wave.
pub use crate::store::project_filter_matches;

pub fn repo_name_from_cwd(cwd: Option<&str>, project_filter: &[String]) -> String {
    // Temporary delegation during Faza 1 transition.
    // This function will be moved into this file from legacy.rs shortly.
    crate::sources::repo_name_from_cwd(cwd, project_filter)
}
