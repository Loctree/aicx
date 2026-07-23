//! AICX library crate.
//!
//! Use [`Aicx`] for the supported in-process boundary.
//!
//! Contract:
//! - default features expose the full app-adjacent library surface used by the
//!   CLI, MCP server, dashboard, semantic search, doctor, and release tooling.
//! - `default-features = false, features = ["loctree-consumer"]` exposes the
//!   stable read core for in-process consumers: scan/read canonical chunks,
//!   typed chunk references, session types, timeline/parser types, and pure
//!   intent stages.
//!
//! Everything behind `feature = "app"` is internal product surface, not the slim
//! consumer contract.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

/// Public identity of the exact checkout used to build this AICX artifact.
pub const BUILD_VERSION: &str = env!("AICX_BUILD_VERSION");
pub const GIT_COMMIT: &str = env!("AICX_GIT_COMMIT");
pub const GIT_DIRTY: bool = env!("AICX_GIT_DIRTY").as_bytes()[0] == b'1';

pub mod api;
#[cfg(feature = "app")]
pub mod auth;
/// Durable extract-era session identity catalog (`~/.aicx/catalog/sessions.jsonl`).
/// Replaces per-frame card mill for identity + project attribution.
#[cfg(feature = "app")]
pub mod catalog;
#[cfg(feature = "app")]
pub mod cli;
#[cfg(feature = "app")]
pub mod corpus;
#[cfg(feature = "app")]
pub mod dashboard;
#[cfg(feature = "app")]
pub mod dashboard_server;
#[cfg(feature = "app")]
pub mod diagnostics;
#[cfg(not(feature = "app"))]
#[allow(dead_code)]
mod diagnostics;
#[cfg(feature = "app")]
pub mod doctor;
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub mod embedder;
#[cfg(feature = "app")]
pub mod evidence;
#[cfg(feature = "app")]
pub mod extraction;
#[cfg(not(feature = "app"))]
#[allow(dead_code, unused_imports)]
mod extraction;
#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub mod hf_cache;
#[cfg(feature = "app")]
pub mod importers;
#[cfg(not(feature = "app"))]
#[allow(dead_code, unused_imports)]
mod importers;
pub mod intents;
#[cfg(feature = "app")]
pub mod locks;
#[cfg(not(feature = "app"))]
#[allow(dead_code)]
mod locks;
#[cfg(feature = "app")]
pub mod mcp;
#[cfg(feature = "app")]
pub mod oracle;
#[cfg(not(feature = "app"))]
#[allow(dead_code)]
mod oracle;
#[cfg(feature = "app")]
pub mod output;
#[cfg(feature = "app")]
pub mod overlay;
#[cfg(feature = "app")]
pub mod parser_dispatch;
#[cfg(feature = "app")]
pub mod progress;
#[cfg(feature = "app")]
pub mod rank;
#[cfg(feature = "app")]
pub mod redact;
#[cfg(feature = "app")]
pub mod reports_extractor;
#[cfg(feature = "app")]
pub mod search_engine;
#[cfg(feature = "app")]
pub mod search_eval;
/// Deterministic locate-before-parse session catalog (C1L line). Registered
/// here for the CLI extraction dispatch; final shared runtime wiring stays
/// with the C5X cut.
#[cfg(feature = "app")]
pub mod session_catalog;
pub mod sessions;
#[cfg(feature = "app")]
pub mod state;
#[cfg(feature = "app")]
pub mod steer_index;
#[cfg(feature = "app")]
mod steer_index_contract;
pub mod store;
#[cfg(feature = "app")]
pub mod validation;
#[cfg(not(feature = "app"))]
mod validation;
#[cfg(feature = "app")]
pub mod vector_index;
#[cfg(feature = "app")]
pub mod wizard;

/// Test-only shared tracing capture (deterministic under parallel `cargo test`).
#[cfg(all(test, feature = "app"))]
mod test_support;

pub use aicx_parser as parser;
pub use aicx_parser::{card_header, chunker, frontmatter, sanitize, segmentation, timeline, types};
pub use api::{Aicx, AicxConfig, IndexReadiness, IndexStatus, StoreOptions};
#[cfg(feature = "app")]
pub use api::{SearchOptions, SearchResults};

#[cfg(any(feature = "native-embedder", feature = "cloud-embedder"))]
pub use aicx_embeddings as embeddings;

/// Resolve the OS user home directory, honoring `HOME`/`USERPROFILE` env
/// overrides before the platform default.
///
/// `dirs::home_dir()` on Windows reads the profile through the Win32
/// known-folder API and ignores environment variables, so a spawned process
/// cannot be redirected to a sandbox home the way `HOME` does on Unix — which
/// breaks every test (and any caller) that sets `HOME`/`USERPROFILE` to isolate
/// session/source/store discovery. Honor those vars first so redirection works
/// on every platform; for a real user `USERPROFILE` already equals the
/// known-folder profile, so this is behavior-preserving outside an explicit
/// override.
pub fn os_user_home() -> Option<std::path::PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(value) = std::env::var_os(key)
            && !value.is_empty()
        {
            return Some(std::path::PathBuf::from(value));
        }
    }
    dirs::home_dir()
}

pub mod prelude {
    #[cfg(feature = "app")]
    pub use crate::api::SearchOptions;
    pub use crate::api::{Aicx, AicxConfig, StoreOptions};
    #[cfg(feature = "app")]
    pub use crate::doctor::{DoctorOptions, DoctorReport};
    pub use crate::intents::{IntentExtraction, IntentRecord, IntentsConfig};
    #[cfg(feature = "app")]
    pub use crate::rank::FuzzyResult;
    pub use crate::store::{ChunkRefSpec, ReadContextChunk, StoreWriteSummary, StoredContextFile};
    pub use crate::timeline::TimelineEntry;
}

#[cfg(all(test, feature = "loctree-consumer", not(feature = "app")))]
mod loctree_consumer_contract_tests {
    use super::*;

    #[test]
    fn slim_profile_exposes_read_core_contract() {
        fn assert_send_sync<T: Send + Sync>() {}

        assert_send_sync::<Aicx>();
        assert_send_sync::<store::StoredContextFile>();
        assert_send_sync::<store::ReadContextChunk>();
        assert_send_sync::<sessions::SessionInfo>();
        assert_send_sync::<intents::IntentExtraction>();

        let root = std::env::temp_dir().join(format!(
            "aicx-loctree-consumer-contract-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("create slim store root");

        let client = Aicx::with_store_root(&root);
        assert!(client.list_chunks().expect("list chunks").is_empty());
        assert!(client.read_chunk("chunk:abcdef12", Some(16)).is_err());

        let parsed = store::ChunkRefSpec::parse("chunk:abcdef12").expect("typed chunk ref");
        assert_eq!(parsed, store::ChunkRefSpec::Id("abcdef12".to_string()));

        let config = intents::IntentsConfig {
            project: String::new(),
            hours: 0,
            strict: false,
            min_confidence: None,
            kind_filter: None,
            frame_kind: None,
        };
        assert_eq!(
            config.effective_frame_kind(),
            timeline::FrameKind::UserMsg,
            "intent defaults stay available in the slim profile"
        );

        let _session_type: Option<sessions::SessionInfo> = None;
        let _ = std::fs::remove_dir_all(root);
    }
}
