//! Diagnostic and self-healing layer for aicx.
//!
//! `aicx doctor` performs an integrity audit of the canonical store, the
//! steer index (Lance + BM25), state.json, sidecar coverage, and corpus
//! bucket names. With
//! `--rebuild-steer-index` (formerly `--fix`, kept as a deprecated alias),
//! the steer index is deleted and rebuilt from the canonical store via
//! `steer_index::rebuild_steer_index_if_needed`. The flag was renamed in
//! 2026-05-25 (Wave D / Cut D1) because the original `--fix` was a no-op
//! for most warning classes (sidecars, index consistency, empty bodies),
//! addressing only the corrupted-steer-index case. The narrower name
//! matches the actual contract; other remediations live behind their
//! dedicated flags (`--prune-empty-bodies`, `--fix-buckets`,
//! `aicx store --full-rescan`).
//!
//! With `--fix-buckets`, suspicious top-level store buckets are moved to
//! timestamped quarantine. With `--prune-empty-bodies --apply`, empty-body
//! chunks and their sidecars are moved to a recoverable empty-body quarantine.
//!
//! The canonical store (`~/.aicx/store/`) is treated as ground truth: doctor
//! never deletes store contents. Bucket quarantine is a rename into
//! `~/.aicx/quarantine/<timestamp>/`, preserving the original payload.
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

mod checks;
mod cleanup;
mod quarantine;
mod report;
mod types;

pub use checks::{run, run_at};
pub use cleanup::{format_cleanup_run_text, run_automated_cleanup_at, run_interactive_cleanup_at};
pub use quarantine::{
    format_restore_text, render_prune_empty_bodies_script, render_rebuild_sidecars_script,
    restore_quarantine, restore_quarantine_at,
};
pub use report::{format_oracle_readiness_text, format_report_text, oracle_readiness};
pub use types::{
    CheckResult, DoctorApplyPhase, DoctorCleanupRunReport, DoctorDryRunPreview, DoctorFixId,
    DoctorOptions, DoctorReport, OracleReadinessReport, QuarantineManifest, QuarantineManifestItem,
    QuarantineRestoreReport, Severity,
};

#[cfg(test)]
pub(crate) use checks::*;
#[cfg(test)]
pub(crate) use quarantine::*;
#[cfg(test)]
pub(crate) use report::*;

#[cfg(test)]
mod tests;
