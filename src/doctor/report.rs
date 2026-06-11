//! Severity aggregation and operator-facing rendering of doctor
//! reports and oracle readiness.

use crate::oracle::OracleReadiness;

use super::types::{DoctorReport, OracleReadinessReport, Severity};

pub(crate) fn max_severity(items: &[Severity]) -> Severity {
    let mut max = Severity::Green;
    for s in items {
        match s {
            Severity::Critical => return Severity::Critical,
            Severity::Warning if max == Severity::Green => max = Severity::Warning,
            _ => {}
        }
    }
    max
}

pub fn format_report_text(report: &DoctorReport, verbose: bool) -> String {
    let mut out = String::new();
    out.push_str("aicx doctor report\n");
    out.push_str(&format!("Overall: {:?}\n\n", report.overall));
    // B-P3-27 (Wave A3 §11 P1.6): `sidecar_coverage` is literally
    // `sidecars.clone()` (see `populate_doctor_report`), so emitting it
    // again here produced two identical `[Severity] sidecars: ...`
    // rows. The JSON serializer still carries both fields for
    // back-compat with the previous schema; the text path renders
    // sidecars exactly once.
    let checks = [
        &report.canonical_store,
        &report.steer_lance,
        &report.steer_bm25,
        &report.state,
        &report.sidecars,
        &report.corpus_buckets,
        &report.noise_health,
        &report.semantic_health,
        &report.index_freshness,
        &report.index_consistency,
        &report.embedder_warmth,
        &report.empty_body_chunks,
        &report.content_dedup,
    ];
    for check in checks {
        out.push_str(&format!(
            "[{:?}] {}: {}\n",
            check.severity, check.name, check.detail
        ));
        if let Some(rec) = &check.recommendation
            && (verbose || check.severity != Severity::Green)
        {
            out.push_str(&format!("    -> {}\n", rec));
        }
    }
    if !report.fixes_applied.is_empty() {
        out.push_str("\nFixes applied:\n");
        for fix in &report.fixes_applied {
            out.push_str(&format!("  + {}\n", fix));
        }
    }
    if let Some(script) = &report.rebuild_sidecars_script {
        out.push_str("\nRebuild sidecars script:\n");
        out.push_str(script);
    }
    if let Some(script) = &report.prune_empty_bodies_script {
        out.push_str("\nPrune empty bodies script:\n");
        out.push_str(script);
    }
    out
}

pub fn oracle_readiness(report: &DoctorReport) -> OracleReadinessReport {
    let canonical = report.canonical_store.severity;
    let metadata = max_severity(&[report.steer_lance.severity, report.steer_bm25.severity]);
    let content = content_semantic_severity(report);

    // TODO: Actually check dashboard port if configured.
    let dashboard = Severity::NotConfigured;

    let readiness = if canonical == Severity::Critical
        || report.sidecars.severity == Severity::Critical
        || content == Severity::Critical
    {
        OracleReadiness::UnsafeForLoctreeScope
    } else if metadata != Severity::Green
        || content != Severity::Green
        || (dashboard != Severity::Green && dashboard != Severity::NotConfigured)
    {
        OracleReadiness::Degraded
    } else {
        OracleReadiness::Ready
    };

    let reason = match readiness {
        OracleReadiness::Ready => "canonical corpus, metadata steer index, and semantic route are healthy".to_string(),
        OracleReadiness::Degraded => "oracle usable with explicit degradation; metadata or dashboard route needs attention".to_string(),
        OracleReadiness::UnsafeForLoctreeScope => "content semantic index is unavailable or corpus health is unsafe; Loctree must not use AICX to narrow scope".to_string(),
    };

    OracleReadinessReport {
        readiness,
        readiness_label: match readiness {
            OracleReadiness::Ready => "ready",
            OracleReadiness::Degraded => "degraded",
            OracleReadiness::UnsafeForLoctreeScope => "unsafe_for_loctree_scope",
        },
        canonical_corpus_health: canonical,
        metadata_steer_index_health: metadata,
        content_semantic_index_health: content,
        dashboard_semantic_route_health: dashboard,
        loctree_oracle_readiness: readiness,
        reason,
    }
}

pub(crate) fn content_semantic_severity(report: &DoctorReport) -> Severity {
    match (
        report.semantic_health.severity,
        report.index_freshness.severity,
    ) {
        (Severity::Critical, Severity::Critical) => Severity::Critical,
        (Severity::Green, Severity::Green) => Severity::Green,
        _ => Severity::Warning,
    }
}

pub fn format_oracle_readiness_text(report: &OracleReadinessReport) -> String {
    format!(
        "canonical corpus health: {:?}\nmetadata steer index health: {:?}\ncontent semantic index health: {:?}\ndashboard semantic route health: {:?}\nLoctree oracle readiness: {}\nreason: {}\n",
        report.canonical_corpus_health,
        report.metadata_steer_index_health,
        report.content_semantic_index_health,
        report.dashboard_semantic_route_health,
        report.readiness_label,
        report.reason
    )
}
