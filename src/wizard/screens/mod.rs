pub mod corpus;
pub mod doctor;
pub mod intents;
pub mod rebuild;
pub mod search;

pub(crate) fn catalog_projects() -> Vec<String> {
    crate::aicx_home::resolve()
        .and_then(|home| crate::catalog::project_identities_from_catalog_at(&home))
        .unwrap_or_default()
}

pub(crate) fn cycle_catalog_project(current: Option<&str>) -> Option<String> {
    let projects = catalog_projects();
    match current {
        None => projects.first().cloned(),
        Some(selected) => projects
            .iter()
            .position(|project| project.eq_ignore_ascii_case(selected))
            .and_then(|index| projects.get(index + 1).cloned()),
    }
}

pub(crate) fn catalog_agents() -> Vec<String> {
    let Ok(home) = crate::aicx_home::resolve() else {
        return Vec::new();
    };
    let mut agents = crate::catalog::read_entries_at(&home)
        .unwrap_or_default()
        .into_iter()
        .map(|entry| entry.agent)
        .collect::<Vec<_>>();
    agents.sort();
    agents.dedup();
    agents
}

pub(crate) fn cycle_catalog_agent(current: Option<&str>) -> Option<String> {
    let agents = catalog_agents();
    match current {
        None => agents.first().cloned(),
        Some(selected) => agents
            .iter()
            .position(|agent| agent.eq_ignore_ascii_case(selected))
            .and_then(|index| agents.get(index + 1).cloned()),
    }
}

/// Clamped list-selection arithmetic shared by every wizard screen.
/// Empty lists collapse the cursor to `0`; movement saturates at both
/// ends instead of wrapping.
pub(crate) fn move_index(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }
    if delta < 0 {
        current.saturating_sub(delta.unsigned_abs()).min(len - 1)
    } else {
        current.saturating_add(delta as usize).min(len - 1)
    }
}
