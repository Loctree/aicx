use std::collections::BTreeMap;

use crate::corpus::types::{CorpusAuditReport, CorpusRepairManifest};

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
