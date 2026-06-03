//! Corpus audit and deterministic derived-markdown repair.
//!
//! Raw agent logs remain provenance. This module only inspects or rewrites
//! derived markdown artifacts that feed retrieval.

mod audit;
mod format;
mod inference;
mod io;
mod noise;
mod repair;
mod roots;
#[cfg(test)]
mod tests;
mod types;

pub use audit::audit;
pub use format::{format_audit_text, format_repair_text};
pub use repair::repair;
pub use roots::default_roots;
pub use types::{
    CorpusAuditOptions, CorpusAuditReport, CorpusAuditTotals, CorpusFileFinding, CorpusRepairItem,
    CorpusRepairManifest, CorpusRepairOptions, RootAuditReport,
};

pub(crate) const REPAIR_VERSION: &str = "aicx-corpus-repair-v1";
pub(crate) const REPAIR_MANIFEST_DIR: &str = "repair-manifests";
