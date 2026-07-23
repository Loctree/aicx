use super::{MigrationAction, MigrationItem, MigrationManifest, is_unclassified_item};

pub(super) fn render_migration_report(manifest: &MigrationManifest) -> String {
    let mut report = String::new();
    report.push_str("# AICX Migration Report\n\n");
    report.push_str(&format!(
        "- Generated at: `{}`\n",
        manifest.generated_at.to_rfc3339()
    ));
    report.push_str(&format!("- Dry run: `{}`\n", manifest.dry_run));
    report.push_str(&format!("- Legacy root: `{}`\n", manifest.legacy_root));
    report.push_str(&format!("- Store root: `{}`\n", manifest.store_root));
    report.push_str(&format!("- Manifest: `{}`\n", manifest.manifest_path));
    report.push_str(&format!("- Report: `{}`\n\n", manifest.report_path));

    report.push_str("## Summary\n\n");
    report.push_str(&format!(
        "- Swept `{}` migration item(s) from `{}` legacy file(s)\n",
        manifest.totals.total_items, manifest.totals.total_legacy_files
    ));
    report.push_str(&format!(
        "- Planned rebuild-only items: `{}`\n",
        manifest.totals.rebuild_items
    ));
    report.push_str(&format!(
        "- Planned rebuild+salvage items: `{}`\n",
        manifest.totals.rebuild_and_salvage_items
    ));
    report.push_str(&format!(
        "- Planned salvage-only items: `{}`\n",
        manifest.totals.salvage_items
    ));
    report.push_str(&format!(
        "- Unclassified legacy items: `{}`\n",
        manifest.totals.unclassified_items
    ));
    report.push_str(&format!(
        "- Resolved source matches: `{}`\n",
        manifest.totals.resolved_sources
    ));
    report.push_str(&format!(
        "- Missing source hints: `{}`\n",
        manifest.totals.missing_source_hints
    ));
    report.push_str(&format!(
        "- Ambiguous source hints: `{}`\n",
        manifest.totals.ambiguous_source_hints
    ));
    report.push_str(&format!(
        "- Rebuilt items: `{}` (`{}` canonical path(s))\n",
        manifest.totals.rebuilt_items, manifest.totals.rebuilt_paths
    ));
    report.push_str(&format!(
        "- Salvaged items: `{}` (`{}` preserved path(s))\n",
        manifest.totals.salvaged_items, manifest.totals.salvaged_paths
    ));
    report.push_str(&format!(
        "- Items with execution errors: `{}`\n\n",
        manifest.totals.failed_items
    ));

    push_report_section(
        &mut report,
        if manifest.dry_run {
            "Planned Rebuild"
        } else {
            "Rebuilt"
        },
        manifest.items.iter().filter(|item| {
            matches!(
                item.action,
                MigrationAction::Rebuild | MigrationAction::RebuildAndSalvage
            )
        }),
    );
    push_report_section(
        &mut report,
        if manifest.dry_run {
            "Planned Salvage"
        } else {
            "Salvaged"
        },
        manifest.items.iter().filter(|item| {
            item.action == MigrationAction::Salvage
                || item.action == MigrationAction::RebuildAndSalvage
                || !item.salvage_paths.is_empty()
        }),
    );
    push_report_section(
        &mut report,
        "Unclassified Legacy Items",
        manifest
            .items
            .iter()
            .filter(|item| is_unclassified_item(item)),
    );

    report
}

fn push_report_section<'a, I>(report: &mut String, title: &str, items: I)
where
    I: Iterator<Item = &'a MigrationItem>,
{
    report.push_str(&format!("## {}\n\n", title));
    let mut wrote = false;

    for item in items {
        wrote = true;
        report.push_str(&format!(
            "- `{}` [{}]\n",
            item.legacy_group, item.action_reason
        ));
        if !item.existing_sources.is_empty() {
            report.push_str(&format!(
                "  sources: `{}`\n",
                item.existing_sources.join("`, `")
            ));
        }
        if !item.canonical_paths.is_empty() {
            report.push_str(&format!(
                "  canonical: `{}`\n",
                item.canonical_paths.join("`, `")
            ));
        }
        if !item.salvage_paths.is_empty() {
            report.push_str(&format!(
                "  legacy: `{}`\n",
                item.salvage_paths.join("`, `")
            ));
        }
        if !item.missing_sources.is_empty() {
            report.push_str(&format!(
                "  missing: `{}`\n",
                item.missing_sources.join("`, `")
            ));
        }
        if !item.ambiguous_sources.is_empty() {
            report.push_str(&format!(
                "  ambiguous: `{}`\n",
                item.ambiguous_sources.join("`, `")
            ));
        }
        if !item.errors.is_empty() {
            report.push_str(&format!("  errors: `{}`\n", item.errors.join("`, `")));
        }
    }

    if !wrote {
        report.push_str("- none\n");
    }

    report.push('\n');
}

pub(super) fn print_migration_summary(manifest: &MigrationManifest) {
    println!(
        "Legacy sweep: {} item(s) from {} file(s).",
        manifest.totals.total_items, manifest.totals.total_legacy_files
    );
    println!("Legacy root: {}", manifest.legacy_root);
    println!("Store root: {}", manifest.store_root);
    println!(
        "Plan: {} rebuild-only, {} rebuild+salvage, {} salvage-only, {} unclassified.",
        manifest.totals.rebuild_items,
        manifest.totals.rebuild_and_salvage_items,
        manifest.totals.salvage_items,
        manifest.totals.unclassified_items
    );
    println!(
        "Source hints: {} resolved, {} missing, {} ambiguous.",
        manifest.totals.resolved_sources,
        manifest.totals.missing_source_hints,
        manifest.totals.ambiguous_source_hints
    );

    if !manifest.dry_run {
        println!(
            "Executed: {} rebuilt item(s) -> {} canonical path(s); {} salvaged item(s) -> {} preserved path(s); {} item(s) with errors.",
            manifest.totals.rebuilt_items,
            manifest.totals.rebuilt_paths,
            manifest.totals.salvaged_items,
            manifest.totals.salvaged_paths,
            manifest.totals.failed_items
        );
    }
}
