use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn default_roots() -> Result<Vec<PathBuf>> {
    let home = crate::os_user_home().context("No home directory")?;
    let aicx_home = crate::aicx_home::resolve()?;
    Ok(vec![
        aicx_home,
        home.join(".ai-contexters"),
        home.join(".xcia"),
    ])
}
