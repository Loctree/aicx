use std::collections::BTreeMap;

use crate::corpus::types::{CorpusAuditReport, CorpusRepairManifest, CorpusValidateReport};

pub fn format_audit_text(report: &CorpusAuditReport) -> String {
    let mut out = String::new();
    out.push_str("=== AICX Corpus Audit ===\n\n");
    out.push_str(&format!(
        "roots: {} present, {} missing\n",
        report.totals.roots_present, report.totals.roots_missing
    ));
    out.push_str(&format!(
        "markdown_files: {}\nfiles_with_noise: {}\n\n",
        report.totals.markdown_files, report.totals.files_with_noise
    ));
    push_counts(&mut out, "noise_classes", &report.totals.noise_classes);
    push_counts(&mut out, "agents", &report.totals.agents);
    push_counts(&mut out, "frame_kinds", &report.totals.frame_kinds);
    push_counts(&mut out, "path_dates", &report.totals.path_dates);

    out.push_str("\nroots:\n");
    for root in &report.roots {
        out.push_str(&format!(
            "- {}: {} (markdown={}, noisy={})\n",
            root.root.display(),
            if root.present { "present" } else { "missing" },
            root.markdown_files,
            root.files_with_noise
        ));
        for example in &root.examples {
            out.push_str(&format!(
                "  example: {} [{}]\n",
                example.path.display(),
                example.noise_classes.join(", ")
            ));
        }
    }
    out
}

pub fn format_repair_text(manifest: &CorpusRepairManifest) -> String {
    let mut out = String::new();
    out.push_str("=== AICX Corpus Repair ===\n\n");
    out.push_str(&format!(
        "mode: {}\n",
        if manifest.apply { "apply" } else { "dry-run" }
    ));
    out.push_str(&format!(
        "scanned_markdown_files: {}\ncandidates: {}\nrepaired_files: {}\nskipped_files: {}\n",
        manifest.scanned_markdown_files,
        manifest.candidates,
        manifest.repaired_files,
        manifest.skipped_files
    ));
    out.push_str(&format!(
        "  skipped_charter_protected: {}\n  skipped_other: {}\n\n",
        manifest.skipped_charter_protected, manifest.skipped_other
    ));
    if manifest.skipped_charter_protected > 0 {
        out.push_str(&format!(
            "note: {} files skipped because charter requires human review \
             (internal_thought_frame and similar). This is by design, not an error.\n\n",
            manifest.skipped_charter_protected
        ));
    }
    for item in &manifest.items {
        out.push_str(&format!(
            "- {} {} [{}]\n",
            item.action,
            item.path.display(),
            item.removed_noise_classes.join(", ")
        ));
    }
    out
}

pub fn format_validate_cards_text(report: &CorpusValidateReport) -> String {
    let mut out = String::new();
    out.push_str("=== AICX Corpus Card Validation ===\n\n");
    out.push_str(&format!(
        "verdict: {}\n",
        if report.passed {
            "pass"
        } else if report.strict {
            "fail"
        } else {
            "warn"
        }
    ));
    out.push_str(&format!(
        "strict: {}\nroots: {} present, {} missing\ncards: {}\nok: {}\nwarn: {}\nerror: {}\nhard_violations: {}\nwarnings: {}\n\n",
        report.strict,
        report.totals.roots_present,
        report.totals.roots_missing,
        report.totals.cards,
        report.totals.ok,
        report.totals.warn,
        report.totals.error,
        report.totals.hard_violations,
        report.totals.warnings
    ));
    push_counts(
        &mut out,
        "violations_by_class",
        &report.totals.violations_by_class,
    );
    push_counts(
        &mut out,
        "warnings_by_class",
        &report.totals.warnings_by_class,
    );
    push_counts(&mut out, "verdicts", &report.totals.verdicts);

    out.push_str("\nroots:\n");
    for root in &report.roots {
        out.push_str(&format!(
            "- {}: {} (cards={}, ok={}, warn={}, error={}, hard={}, warnings={})\n",
            root.root.display(),
            if root.present { "present" } else { "missing" },
            root.cards,
            root.ok,
            root.warn,
            root.error,
            root.hard_violations,
            root.warnings
        ));
        for sample in &root.samples {
            out.push_str(&format!(
                "  {} {}: {} ({})\n",
                sample.severity,
                sample.class,
                sample.path.display(),
                sample.message
            ));
        }
    }
    out
}

fn push_counts(out: &mut String, label: &str, counts: &BTreeMap<String, usize>) {
    out.push_str(&format!("{label}:\n"));
    if counts.is_empty() {
        out.push_str("  (none)\n");
    } else {
        for (key, value) in counts {
            out.push_str(&format!("  {key}: {value}\n"));
        }
    }
}
