//! ai-contexters library crate.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

pub mod chunker;
pub mod dashboard;
pub mod dashboard_server;
pub mod init;
pub mod memex;
pub mod output;
pub mod redact;
pub mod sanitize;
pub mod sources;
pub mod state;
pub mod store;

#[cfg(test)]
pub(crate) mod test_util {
    use chrono::Utc;
    use std::fs;
    use std::path::PathBuf;

    pub fn mk_tmp_dir(name: &str) -> PathBuf {
        let dir = std::env::current_dir()
            .expect("cwd")
            .join("target")
            .join("test-tmp")
            .join(format!("{}_{}", name, Utc::now().timestamp_micros()));
        fs::create_dir_all(&dir).expect("create dir");
        dir
    }
}
