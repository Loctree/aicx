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
mod validate;

pub use audit::audit;
pub use format::{format_audit_text, format_repair_text, format_validate_cards_text};
pub use repair::repair;
pub use roots::default_roots;
pub use types::{
    CorpusAuditOptions, CorpusAuditReport, CorpusAuditTotals, CorpusCardFinding, CorpusFileFinding,
    CorpusRepairItem, CorpusRepairManifest, CorpusRepairOptions, CorpusValidateOptions,
    CorpusValidateReport, CorpusValidateTotals, RootAuditReport, RootValidateReport,
};
pub use validate::validate_cards;

pub(crate) const REPAIR_VERSION: &str = "aicx-corpus-repair-v1";
pub(crate) const REPAIR_MANIFEST_DIR: &str = "repair-manifests";
