//! Diagnostic and self-healing layer for aicx.
//!
//! `aicx doctor` audits AICX-home state, the published retrieval index,
//! state.json, source protection, and residual legacy card archives. With
//! `--clean-retired-steer` removes retired steer-only artifacts; the old
//! `--rebuild-steer-index` and `--fix` spellings remain deprecated aliases.
//! The published CURRENT generation is owned exclusively by `aicx index`;
//! doctor never grows a parallel retrieval index. Other remediations live
//! behind their dedicated flags (`--prune-empty-bodies`, `--fix-buckets`) or
//! the extract-era rebuild path
//! (`aicx catalog rebuild && aicx index --cache-extracts`).
//!
//! With `--fix-buckets`, suspicious top-level legacy archive buckets are moved to
//! timestamped quarantine. With `--prune-empty-bodies --apply`, empty-body
//! chunks and their sidecars are moved to a recoverable empty-body quarantine.
//!
//! Legacy card mill paths under `~/.aicx/store/` may still be inspected by
//! doctor for migration hygiene, but the live corpus is catalog + sources +
//! optional extracts. Bucket quarantine is a rename into
//! `~/.aicx/quarantine/<timestamp>/`, preserving the original payload.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

mod checks;
mod cleanup;
mod quarantine;
mod report;
#[cfg(feature = "app")]
mod runtime;
mod types;

pub use checks::{run, run_at};
pub use cleanup::{format_cleanup_run_text, run_automated_cleanup_at, run_interactive_cleanup_at};
pub use quarantine::{
    format_restore_text, render_prune_empty_bodies_script, render_rebuild_sidecars_script,
    restore_quarantine, restore_quarantine_at,
};
pub use report::{format_oracle_readiness_text, format_report_text, oracle_readiness};
#[cfg(feature = "app")]
pub use runtime::{RuntimeRepairReport, format_runtime_repair_text, repair_runtime};
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
