use anyhow::{Context, Result};
use std::path::PathBuf;

pub fn default_roots() -> Result<Vec<PathBuf>> {
    let home = dirs::home_dir().context("No home directory")?;
    let aicx_home = crate::store::resolve_aicx_home()?;
    Ok(vec![
        aicx_home,
        home.join(".ai-contexters"),
        home.join(".xcia"),
    ])
}
