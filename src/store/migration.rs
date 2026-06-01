use super::{
    atomic_write, legacy_salvage_dir, legacy_store_base_dir, migration_manifest_path,
    migration_report_path, read_store_dir, store_base_dir, store_semantic_segments_at,
};
use crate::chunker::ChunkerConfig;
use crate::sanitize;
use crate::sources::{self, ExtractionConfig};
use crate::timeline::TimelineEntry;
use anyhow::Result;
use chrono::{DateTime, TimeZone, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

// ============================================================================
// Migration
// ============================================================================

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LegacyItemKind {
    ContextBundle,
    LooseFile,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationAction {
    Rebuild,
    RebuildAndSalvage,
    Salvage,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MigrationExecution {
    Planned,
    Executed,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MigrationTotals {
    pub total_items: usize,
    pub total_legacy_files: usize,
    pub rebuild_items: usize,
    pub rebuild_and_salvage_items: usize,
    pub salvage_items: usize,
    pub unclassified_items: usize,
    pub resolved_sources: usize,
    pub missing_source_hints: usize,
    pub ambiguous_source_hints: usize,
    pub rebuilt_items: usize,
    pub salvaged_items: usize,
    pub rebuilt_paths: usize,
    pub salvaged_paths: usize,
    pub failed_items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationManifest {
    pub generated_at: DateTime<Utc>,
    pub legacy_root: String,
    pub store_root: String,
    pub manifest_path: String,
    pub report_path: String,
    pub dry_run: bool,
    pub totals: MigrationTotals,
    pub items: Vec<MigrationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MigrationItem {
    pub item_id: String,
    pub legacy_kind: LegacyItemKind,
    pub legacy_group: String,
    pub legacy_files: Vec<String>,
    pub agent_hint: Option<String>,
    pub date_hint: Option<String>,
    pub source_hints: Vec<String>,
    pub existing_sources: Vec<String>,
    pub missing_sources: Vec<String>,
    pub ambiguous_sources: Vec<String>,
    pub action: MigrationAction,
    pub action_reason: String,
    pub execution: MigrationExecution,
    pub canonical_paths: Vec<String>,
    pub salvage_paths: Vec<String>,
    pub errors: Vec<String>,
}

impl MigrationItem {
    fn from_plan(plan: &LegacyItemPlan) -> Self {
        let mut legacy_files: Vec<String> = plan
            .legacy_files
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        legacy_files.sort();

        let mut existing_sources: Vec<String> = plan
            .resolved_sources
            .iter()
            .map(|source| source.path.display().to_string())
            .collect();
        existing_sources.sort();

        let mut canonical_paths: Vec<String> = plan
            .canonical_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        canonical_paths.sort();

        let mut salvage_paths: Vec<String> = plan
            .salvage_paths
            .iter()
            .map(|path| path.display().to_string())
            .collect();
        salvage_paths.sort();

        Self {
            item_id: plan.item_id.clone(),
            legacy_kind: plan.legacy_kind,
            legacy_group: plan.legacy_group.clone(),
            legacy_files,
            agent_hint: plan.agent_hint.clone(),
            date_hint: plan.date_hint.clone(),
            source_hints: plan.source_hints.clone(),
            existing_sources,
            missing_sources: plan.missing_sources.clone(),
            ambiguous_sources: plan.ambiguous_sources.clone(),
            action: plan.action,
            action_reason: plan.action_reason.clone(),
            execution: plan.execution,
            canonical_paths,
            salvage_paths,
            errors: plan.errors.clone(),
        }
    }
}

impl MigrationTotals {
    fn from_items(items: &[MigrationItem]) -> Self {
        Self {
            total_items: items.len(),
            total_legacy_files: items.iter().map(|item| item.legacy_files.len()).sum(),
            rebuild_items: items
                .iter()
                .filter(|item| item.action == MigrationAction::Rebuild)
                .count(),
            rebuild_and_salvage_items: items
                .iter()
                .filter(|item| item.action == MigrationAction::RebuildAndSalvage)
                .count(),
            salvage_items: items
                .iter()
                .filter(|item| item.action == MigrationAction::Salvage)
                .count(),
            unclassified_items: items
                .iter()
                .filter(|item| is_unclassified_item(item))
                .count(),
            resolved_sources: items.iter().map(|item| item.existing_sources.len()).sum(),
            missing_source_hints: items.iter().map(|item| item.missing_sources.len()).sum(),
            ambiguous_source_hints: items.iter().map(|item| item.ambiguous_sources.len()).sum(),
            rebuilt_items: items
                .iter()
                .filter(|item| !item.canonical_paths.is_empty())
                .count(),
            salvaged_items: items
                .iter()
                .filter(|item| !item.salvage_paths.is_empty())
                .count(),
            rebuilt_paths: items.iter().map(|item| item.canonical_paths.len()).sum(),
            salvaged_paths: items.iter().map(|item| item.salvage_paths.len()).sum(),
            failed_items: items.iter().filter(|item| !item.errors.is_empty()).count(),
        }
    }
}

#[derive(Debug, Clone)]
struct LegacyItemPlan {
    item_id: String,
    legacy_kind: LegacyItemKind,
    legacy_group: String,
    legacy_files: Vec<PathBuf>,
    agent_hint: Option<String>,
    date_hint: Option<String>,
    source_hints: Vec<String>,
    resolved_sources: Vec<ResolvedSource>,
    missing_sources: Vec<String>,
    ambiguous_sources: Vec<String>,
    action: MigrationAction,
    action_reason: String,
    execution: MigrationExecution,
    canonical_paths: Vec<PathBuf>,
    salvage_paths: Vec<PathBuf>,
    errors: Vec<String>,
}

#[derive(Debug, Clone)]
struct LegacyBundleDescriptor {
    bundle_key: PathBuf,
    agent_hint: Option<String>,
    date_hint: Option<String>,
}

#[derive(Debug, Clone, Copy)]
enum SourceFormat {
    Claude,
    Codex,
    Gemini,
    GeminiAntigravity,
    Junie,
}

#[derive(Debug, Clone)]
struct ResolvedSource {
    path: PathBuf,
    format: SourceFormat,
}

#[derive(Debug, Clone, Default)]
struct SourceResolution {
    source_hints: Vec<String>,
    resolved_sources: Vec<ResolvedSource>,
    missing_sources: Vec<String>,
    ambiguous_sources: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct SourceProcessingOutcome {
    canonical_paths: Vec<PathBuf>,
    error: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub(super) struct SourceLocator {
    index: HashMap<String, Vec<PathBuf>>,
}

#[derive(Debug, Clone)]
enum SourceLookupOutcome {
    Missing,
    Unique(PathBuf),
    Ambiguous(Vec<PathBuf>),
}

pub fn run_migration(dry_run: bool) -> Result<MigrationManifest> {
    run_migration_with_paths(dry_run, None, None)
}

pub fn run_migration_with_paths(
    dry_run: bool,
    legacy_root: Option<PathBuf>,
    store_root: Option<PathBuf>,
) -> Result<MigrationManifest> {
    let legacy_root = legacy_root.unwrap_or(legacy_store_base_dir()?);
    let store_root = store_root.unwrap_or(store_base_dir()?);
    let locator = SourceLocator::from_home();
    let manifest = run_migration_at(&legacy_root, &store_root, dry_run, &locator)?;

    print_migration_summary(&manifest);
    if dry_run {
        println!(
            "[DRY RUN] Would write migration manifest to {}",
            manifest.manifest_path
        );
        println!(
            "[DRY RUN] Would write migration report to {}",
            manifest.report_path
        );
    } else {
        println!("Wrote migration manifest to {}", manifest.manifest_path);
        println!("Wrote migration report to {}", manifest.report_path);
    }

    Ok(manifest)
}

pub(super) fn run_migration_at(
    legacy_root: &Path,
    store_root: &Path,
    dry_run: bool,
    locator: &SourceLocator,
) -> Result<MigrationManifest> {
    let normalized_legacy_root = if legacy_root.exists() {
        sanitize::validate_dir_path(legacy_root)?
    } else {
        legacy_root.to_path_buf()
    };
    let mut items = collect_legacy_items(&normalized_legacy_root, locator)?;

    if !dry_run {
        execute_migration_items(&normalized_legacy_root, store_root, &mut items)?;
    }

    let manifest =
        build_migration_manifest_at(&normalized_legacy_root, store_root, dry_run, &items);
    if !dry_run {
        write_migration_artifacts(store_root, &manifest)?;
    }

    Ok(manifest)
}

fn build_migration_manifest_at(
    legacy_root: &Path,
    store_root: &Path,
    dry_run: bool,
    items: &[LegacyItemPlan],
) -> MigrationManifest {
    let items: Vec<MigrationItem> = items.iter().map(MigrationItem::from_plan).collect();
    let totals = MigrationTotals::from_items(&items);

    MigrationManifest {
        generated_at: Utc::now(),
        legacy_root: legacy_root.display().to_string(),
        store_root: store_root.display().to_string(),
        manifest_path: migration_manifest_path(store_root).display().to_string(),
        report_path: migration_report_path(store_root).display().to_string(),
        dry_run,
        totals,
        items,
    }
}

fn write_migration_artifacts(store_root: &Path, manifest: &MigrationManifest) -> Result<()> {
    let manifest_path = migration_manifest_path(store_root);
    let manifest_path = sanitize::validate_write_path(&manifest_path)?;
    let manifest_json = serde_json::to_string_pretty(manifest)?;
    atomic_write(&manifest_path, manifest_json.as_bytes())?;

    let report_path = migration_report_path(store_root);
    let report_path = sanitize::validate_write_path(&report_path)?;
    let report = render_migration_report(manifest);
    atomic_write(&report_path, report.as_bytes())?;

    Ok(())
}

fn render_migration_report(manifest: &MigrationManifest) -> String {
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

fn print_migration_summary(manifest: &MigrationManifest) {
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

fn collect_legacy_items(
    legacy_root: &Path,
    locator: &SourceLocator,
) -> Result<Vec<LegacyItemPlan>> {
    let mut files = Vec::new();
    collect_legacy_files(legacy_root, &mut files)?;

    let mut bundles: BTreeMap<String, (LegacyBundleDescriptor, Vec<PathBuf>)> = BTreeMap::new();
    let mut loose_files = Vec::new();

    for file in files {
        let relative = match file.strip_prefix(legacy_root) {
            Ok(relative) => relative.to_path_buf(),
            Err(_) => continue,
        };

        if let Some(descriptor) = legacy_bundle_descriptor(&relative) {
            bundles
                .entry(descriptor.bundle_key.display().to_string())
                .or_insert_with(|| (descriptor.clone(), Vec::new()))
                .1
                .push(file);
        } else {
            loose_files.push(file);
        }
    }

    let mut items = Vec::new();

    for (_, (descriptor, mut bundle_files)) in bundles {
        bundle_files.sort();
        items.push(build_context_bundle_plan(
            &bundle_files,
            &descriptor,
            locator,
        )?);
    }

    loose_files.sort();
    for file in loose_files {
        let relative = file
            .strip_prefix(legacy_root)
            .unwrap_or(file.as_path())
            .display()
            .to_string();
        items.push(LegacyItemPlan {
            item_id: relative.clone(),
            legacy_kind: LegacyItemKind::LooseFile,
            legacy_group: relative,
            legacy_files: vec![file],
            agent_hint: None,
            date_hint: None,
            source_hints: Vec::new(),
            resolved_sources: Vec::new(),
            missing_sources: Vec::new(),
            ambiguous_sources: Vec::new(),
            action: MigrationAction::Salvage,
            action_reason: "non_context_legacy_file".to_string(),
            execution: MigrationExecution::Planned,
            canonical_paths: Vec::new(),
            salvage_paths: Vec::new(),
            errors: Vec::new(),
        });
    }

    items.sort_by(|left, right| left.legacy_group.cmp(&right.legacy_group));
    Ok(items)
}

fn collect_legacy_files(root: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    if !root.exists() || !root.is_dir() {
        return Ok(());
    }

    for entry in read_store_dir(root)?.filter_map(|entry| entry.ok()) {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }

        let file_type = match entry.file_type() {
            Ok(file_type) => file_type,
            Err(_) => continue,
        };

        if file_type.is_dir() {
            collect_legacy_files(&path, files)?;
        } else if file_type.is_file() {
            files.push(path);
        }
    }

    Ok(())
}

fn legacy_bundle_descriptor(relative: &Path) -> Option<LegacyBundleDescriptor> {
    let file_name = relative.file_name()?.to_str()?;
    let stem = Path::new(file_name).file_stem()?.to_str()?;
    let parent = relative.parent()?;
    let date_hint = parent.file_name()?.to_str()?.to_string();
    if !looks_like_iso_date(&date_hint) {
        return None;
    }

    let bundle_re =
        Regex::new(r"^(?P<time>\d{6})_(?P<agent>[A-Za-z0-9_]+)(?:-(?P<tail>\d{3}|context))?$")
            .expect("legacy bundle regex should compile");
    let captures = bundle_re.captures(stem)?;
    let bundle_name = format!("{}_{}", &captures["time"], &captures["agent"]);

    Some(LegacyBundleDescriptor {
        bundle_key: parent.join(bundle_name),
        agent_hint: Some(captures["agent"].to_string()),
        date_hint: Some(date_hint),
    })
}

fn build_context_bundle_plan(
    bundle_files: &[PathBuf],
    descriptor: &LegacyBundleDescriptor,
    locator: &SourceLocator,
) -> Result<LegacyItemPlan> {
    let resolution =
        resolve_sources_for_legacy_files(bundle_files, descriptor.agent_hint.as_deref(), locator);
    let has_resolved = !resolution.resolved_sources.is_empty();
    let has_unresolved =
        !resolution.missing_sources.is_empty() || !resolution.ambiguous_sources.is_empty();

    let (action, action_reason) = if has_resolved && has_unresolved {
        (
            MigrationAction::RebuildAndSalvage,
            "partial_source_recovery".to_string(),
        )
    } else if has_resolved {
        (MigrationAction::Rebuild, "rebuild_from_source".to_string())
    } else if !resolution.ambiguous_sources.is_empty() {
        (
            MigrationAction::Salvage,
            "ambiguous_source_hints".to_string(),
        )
    } else if !resolution.source_hints.is_empty() {
        (MigrationAction::Salvage, "missing_source".to_string())
    } else {
        (MigrationAction::Salvage, "no_source_hints".to_string())
    };

    Ok(LegacyItemPlan {
        item_id: descriptor.bundle_key.display().to_string(),
        legacy_kind: LegacyItemKind::ContextBundle,
        legacy_group: descriptor.bundle_key.display().to_string(),
        legacy_files: bundle_files.to_vec(),
        agent_hint: descriptor.agent_hint.clone(),
        date_hint: descriptor.date_hint.clone(),
        source_hints: resolution.source_hints,
        resolved_sources: resolution.resolved_sources,
        missing_sources: resolution.missing_sources,
        ambiguous_sources: resolution.ambiguous_sources,
        action,
        action_reason,
        execution: MigrationExecution::Planned,
        canonical_paths: Vec::new(),
        salvage_paths: Vec::new(),
        errors: Vec::new(),
    })
}

fn resolve_sources_for_legacy_files(
    bundle_files: &[PathBuf],
    agent_hint: Option<&str>,
    locator: &SourceLocator,
) -> SourceResolution {
    let mut direct_candidates = BTreeSet::new();
    let mut lookup_hints = BTreeSet::new();
    let mut source_hints = BTreeSet::new();

    for file in bundle_files {
        let content = sanitize::read_to_string_validated(file).unwrap_or_default();
        collect_source_hints_from_text(
            &content,
            agent_hint,
            &mut direct_candidates,
            &mut lookup_hints,
            &mut source_hints,
        );
    }

    let mut resolved_sources = BTreeMap::new();
    let mut missing_sources = BTreeSet::new();
    let mut ambiguous_sources = BTreeSet::new();
    let mut handled_lookup_hints = BTreeSet::new();

    for direct in direct_candidates {
        source_hints.insert(direct.display().to_string());

        if direct.exists() {
            if let Some(format) = source_format_hint(&direct, agent_hint) {
                register_lookup_keys(&direct, &mut handled_lookup_hints);
                resolved_sources.insert(
                    direct.clone(),
                    ResolvedSource {
                        path: direct,
                        format,
                    },
                );
            } else {
                missing_sources.insert(format!("unsupported: {}", direct.display()));
            }
            continue;
        }

        let lookup_key = direct
            .file_name()
            .and_then(|name| name.to_str())
            .map(|name| name.to_ascii_lowercase());
        match lookup_key
            .as_deref()
            .map(|key| locator.lookup(key))
            .unwrap_or(SourceLookupOutcome::Missing)
        {
            SourceLookupOutcome::Unique(path) => {
                if let Some(format) = source_format_hint(&path, agent_hint) {
                    register_lookup_keys(&direct, &mut handled_lookup_hints);
                    resolved_sources.insert(path.clone(), ResolvedSource { path, format });
                } else {
                    missing_sources.insert(format!("unsupported: {}", direct.display()));
                }
            }
            SourceLookupOutcome::Ambiguous(paths) => {
                ambiguous_sources.insert(format!(
                    "{} -> {}",
                    direct.display(),
                    display_paths(&paths)
                ));
            }
            SourceLookupOutcome::Missing => {
                missing_sources.insert(direct.display().to_string());
            }
        }
    }

    for hint in lookup_hints {
        if handled_lookup_hints.contains(&hint) {
            continue;
        }
        source_hints.insert(hint.clone());
        match locator.lookup(&hint) {
            SourceLookupOutcome::Unique(path) => {
                if let Some(format) = source_format_hint(&path, agent_hint) {
                    resolved_sources.insert(path.clone(), ResolvedSource { path, format });
                } else {
                    missing_sources.insert(format!("unsupported: {}", hint));
                }
            }
            SourceLookupOutcome::Ambiguous(paths) => {
                ambiguous_sources.insert(format!("{} -> {}", hint, display_paths(&paths)));
            }
            SourceLookupOutcome::Missing => {
                missing_sources.insert(hint);
            }
        }
    }

    SourceResolution {
        source_hints: source_hints.into_iter().collect(),
        resolved_sources: resolved_sources.into_values().collect(),
        missing_sources: missing_sources.into_iter().collect(),
        ambiguous_sources: ambiguous_sources.into_iter().collect(),
    }
}

fn collect_source_hints_from_text(
    text: &str,
    agent_hint: Option<&str>,
    direct_candidates: &mut BTreeSet<PathBuf>,
    lookup_hints: &mut BTreeSet<String>,
    source_hints: &mut BTreeSet<String>,
) {
    let absolute_path_re = Regex::new(
        r"(?:file://)?(/(?:[A-Za-z0-9._~\-]+/)*[A-Za-z0-9._~\-]+(?:\.[A-Za-z0-9._~-]+)?)",
    )
    .expect("absolute legacy source hint regex should compile");
    let tilde_path_re =
        Regex::new(r"(~/(?:[A-Za-z0-9._~\-]+/)*[A-Za-z0-9._~\-]+(?:\.[A-Za-z0-9._~-]+)?)")
            .expect("tilde legacy source hint regex should compile");

    for capture in absolute_path_re.captures_iter(text) {
        if let Some(raw) = capture.get(1) {
            let candidate = PathBuf::from(raw.as_str());
            if source_format_hint(&candidate, agent_hint).is_some() {
                source_hints.insert(candidate.display().to_string());
                direct_candidates.insert(candidate);
            }
        }
    }

    for capture in tilde_path_re.captures_iter(text) {
        if let Some(raw) = capture.get(1) {
            let expanded = expand_tilde(raw.as_str());
            if source_format_hint(&expanded, agent_hint).is_some() {
                source_hints.insert(raw.as_str().to_string());
                direct_candidates.insert(expanded);
            }
        }
    }

    for hint_re in [
        Regex::new(r"\brollout-[A-Za-z0-9T._:-]+\.jsonl\b")
            .expect("codex rollout hint regex should compile"),
        Regex::new(r"\bsession-[A-Za-z0-9._-]+\.json\b")
            .expect("gemini session hint regex should compile"),
        Regex::new(r"\b[0-9a-fA-F-]{16,}\.pb\b").expect("antigravity pb hint regex should compile"),
        Regex::new(r"\b[0-9a-fA-F-]{16,}\.jsonl\b")
            .expect("claude jsonl hint regex should compile"),
    ] {
        for capture in hint_re.captures_iter(text) {
            if let Some(raw) = capture.get(0) {
                let hint = raw.as_str().to_ascii_lowercase();
                source_hints.insert(raw.as_str().to_string());
                lookup_hints.insert(hint);
            }
        }
    }
}

fn source_format_hint(path: &Path, agent_hint: Option<&str>) -> Option<SourceFormat> {
    let path_str = path.to_string_lossy().to_ascii_lowercase();
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .map(|name| name.to_ascii_lowercase())
        .unwrap_or_default();
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .map(|ext| ext.to_ascii_lowercase());

    match agent_hint {
        Some("claude") => {
            if extension.as_deref() == Some("jsonl")
                || extension.as_deref() == Some("output")
                || path_str.contains("/.claude/")
            {
                return Some(SourceFormat::Claude);
            }
            return None;
        }
        Some("codex") => {
            if extension.as_deref() == Some("jsonl")
                || file_name == "history.jsonl"
                || file_name.starts_with("rollout-")
                || path_str.contains("/.codex/")
            {
                return Some(SourceFormat::Codex);
            }
            return None;
        }
        Some("gemini") => {
            if extension.as_deref() == Some("pb")
                || path_str.contains("/antigravity/brain/")
                || path_str.contains("/antigravity/conversations/")
            {
                return Some(SourceFormat::GeminiAntigravity);
            }
            if extension.as_deref() == Some("json")
                && (file_name.starts_with("session-") || path_str.contains("/.gemini/tmp/"))
            {
                return Some(SourceFormat::Gemini);
            }
            return None;
        }
        Some("junie") => {
            if extension.as_deref() == Some("jsonl")
                && file_name == "events.jsonl"
                && (path_str.contains("/.junie/sessions/")
                    || path
                        .parent()
                        .and_then(|parent| parent.file_name())
                        .and_then(|name| name.to_str())
                        .is_some_and(|name| name.starts_with("session-")))
            {
                return Some(SourceFormat::Junie);
            }
            return None;
        }
        Some(_) => return None,
        None => {}
    }

    if extension.as_deref() == Some("pb")
        || path_str.contains("/antigravity/brain/")
        || path_str.contains("/antigravity/conversations/")
    {
        return Some(SourceFormat::GeminiAntigravity);
    }

    if extension.as_deref() == Some("json")
        && (file_name.starts_with("session-") || path_str.contains("/.gemini/tmp/"))
    {
        return Some(SourceFormat::Gemini);
    }

    if extension.as_deref() == Some("jsonl")
        && file_name == "events.jsonl"
        && (path_str.contains("/.junie/sessions/")
            || path
                .parent()
                .and_then(|parent| parent.file_name())
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("session-")))
    {
        return Some(SourceFormat::Junie);
    }

    if extension.as_deref() == Some("jsonl")
        && (file_name.starts_with("rollout-")
            || file_name == "history.jsonl"
            || path_str.contains("/.codex/"))
    {
        return Some(SourceFormat::Codex);
    }

    if (extension.as_deref() == Some("jsonl") || extension.as_deref() == Some("output"))
        && path_str.contains("/.claude/")
    {
        return Some(SourceFormat::Claude);
    }

    None
}

fn execute_migration_items(
    legacy_root: &Path,
    store_root: &Path,
    items: &mut [LegacyItemPlan],
) -> Result<()> {
    let chunker_config = ChunkerConfig::default();
    let mut source_cache: HashMap<PathBuf, SourceProcessingOutcome> = HashMap::new();

    for item in items {
        item.execution = MigrationExecution::Executed;

        if matches!(
            item.action,
            MigrationAction::Rebuild | MigrationAction::RebuildAndSalvage
        ) {
            for source in item.resolved_sources.clone() {
                let outcome = source_cache.entry(source.path.clone()).or_insert_with(|| {
                    rebuild_source_into_store(store_root, &source, &chunker_config)
                });

                for path in &outcome.canonical_paths {
                    push_unique_path(&mut item.canonical_paths, path.clone());
                }
                if let Some(error) = &outcome.error {
                    item.errors
                        .push(format!("{}: {}", source.path.display(), error));
                }
            }
        }

        if item.action == MigrationAction::Rebuild && item.canonical_paths.is_empty() {
            item.errors
                .push("No canonical paths were written from resolved sources.".to_string());
        }

        let should_salvage = matches!(
            item.action,
            MigrationAction::Salvage | MigrationAction::RebuildAndSalvage
        ) || !item.errors.is_empty()
            || (item.action == MigrationAction::Rebuild && item.canonical_paths.is_empty());

        if should_salvage {
            let salvaged = preserve_legacy_item(legacy_root, store_root, item)?;
            item.salvage_paths = salvaged;
        }
    }

    Ok(())
}

fn rebuild_source_into_store(
    store_root: &Path,
    source: &ResolvedSource,
    chunker_config: &ChunkerConfig,
) -> SourceProcessingOutcome {
    match rebuild_source_into_store_impl(store_root, source, chunker_config) {
        Ok(canonical_paths) => SourceProcessingOutcome {
            canonical_paths,
            error: None,
        },
        Err(error) => SourceProcessingOutcome {
            canonical_paths: Vec::new(),
            error: Some(error.to_string()),
        },
    }
}

fn rebuild_source_into_store_impl(
    store_root: &Path,
    source: &ResolvedSource,
    chunker_config: &ChunkerConfig,
) -> Result<Vec<PathBuf>> {
    let entries = extract_entries_from_source(source)?;
    if entries.is_empty() {
        anyhow::bail!("source produced no timeline entries");
    }

    let summary = store_semantic_segments_at(store_root, &entries, chunker_config, |_, _| {})?;
    Ok(summary.written_paths)
}

fn extract_entries_from_source(source: &ResolvedSource) -> Result<Vec<TimelineEntry>> {
    let config = ExtractionConfig {
        project_filter: Vec::new(),
        cutoff: Utc
            .timestamp_opt(0, 0)
            .single()
            .expect("unix epoch should be representable"),
        include_assistant: true,
        watermark: None,
    };

    match source.format {
        SourceFormat::Claude => sources::extract_claude_file(&source.path, &config),
        SourceFormat::Codex => sources::extract_codex_file(&source.path, &config),
        SourceFormat::Gemini => sources::extract_gemini_file(&source.path, &config),
        SourceFormat::GeminiAntigravity => {
            sources::extract_gemini_antigravity_file(&source.path, &config)
        }
        SourceFormat::Junie => sources::extract_junie_file(&source.path, &config),
    }
}

fn preserve_legacy_item(
    legacy_root: &Path,
    store_root: &Path,
    item: &LegacyItemPlan,
) -> Result<Vec<PathBuf>> {
    let salvage_root = legacy_salvage_dir(store_root);
    fs::create_dir_all(&salvage_root)?;

    let mut preserved = Vec::new();
    for legacy_file in &item.legacy_files {
        let legacy_file = sanitize::validate_read_path(legacy_file)?;
        let relative = legacy_file
            .strip_prefix(legacy_root)
            .unwrap_or(legacy_file.as_path());
        let destination = salvage_root.join(relative);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        let destination = sanitize::validate_write_path(&destination)?;
        let mut src = sanitize::open_file_validated(&legacy_file)?;
        let mut dst = sanitize::create_file_validated(&destination)?;
        std::io::copy(&mut src, &mut dst)?;
        preserved.push(destination);
    }

    let provenance_path = provenance_path_for_item(&salvage_root, legacy_root, item);
    if let Some(parent) = provenance_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let provenance_path = sanitize::validate_write_path(&provenance_path)?;
    let provenance = serde_json::json!({
        "generated_at": Utc::now().to_rfc3339(),
        "item_id": item.item_id.clone(),
        "legacy_group": item.legacy_group.clone(),
        "action": item.action,
        "action_reason": item.action_reason.clone(),
        "legacy_files": item.legacy_files.iter().map(|path| path.display().to_string()).collect::<Vec<_>>(),
        "source_hints": item.source_hints.clone(),
        "existing_sources": item.resolved_sources.iter().map(|source| source.path.display().to_string()).collect::<Vec<_>>(),
        "missing_sources": item.missing_sources.clone(),
        "ambiguous_sources": item.ambiguous_sources.clone(),
        "errors": item.errors.clone(),
    });
    atomic_write(
        &provenance_path,
        serde_json::to_string_pretty(&provenance)?.as_bytes(),
    )?;
    preserved.push(provenance_path);

    Ok(preserved)
}

fn provenance_path_for_item(
    salvage_root: &Path,
    legacy_root: &Path,
    item: &LegacyItemPlan,
) -> PathBuf {
    let anchor = if item.legacy_kind == LegacyItemKind::ContextBundle {
        PathBuf::from(&item.legacy_group)
    } else {
        item.legacy_files
            .first()
            .and_then(|path| path.strip_prefix(legacy_root).ok())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(&item.legacy_group))
    };

    let parent = anchor.parent().map(Path::to_path_buf).unwrap_or_default();
    let file_name = anchor
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| "legacy-item".to_string());

    salvage_root
        .join(parent)
        .join(format!("{}.migration-provenance.json", file_name))
}

fn push_unique_path(paths: &mut Vec<PathBuf>, candidate: PathBuf) {
    if !paths.iter().any(|path| path == &candidate) {
        paths.push(candidate);
    }
}

fn is_unclassified_item(item: &MigrationItem) -> bool {
    item.legacy_kind == LegacyItemKind::LooseFile
        || item.action_reason == "no_source_hints"
        || item.action_reason == "non_context_legacy_file"
}

fn looks_like_iso_date(value: &str) -> bool {
    value.len() == 10
        && value.chars().enumerate().all(|(idx, ch)| match idx {
            4 | 7 => ch == '-',
            _ => ch.is_ascii_digit(),
        })
}

fn expand_tilde(raw: &str) -> PathBuf {
    if let Some(rest) = raw.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }

    PathBuf::from(raw)
}

fn display_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

fn register_lookup_keys(path: &Path, handled_lookup_hints: &mut BTreeSet<String>) {
    if let Some(file_name) = path.file_name().and_then(|name| name.to_str()) {
        handled_lookup_hints.insert(file_name.to_ascii_lowercase());
    }
    if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
        handled_lookup_hints.insert(stem.to_ascii_lowercase());
    }
}

impl SourceLocator {
    fn from_home() -> Self {
        let Some(home) = dirs::home_dir() else {
            return Self::default();
        };

        let mut locator = Self::default();
        locator.index_recursive(home.join(".claude").join("projects"), |path| {
            matches!(
                path.extension().and_then(|ext| ext.to_str()),
                Some("jsonl" | "output")
            )
        });
        locator.index_recursive(home.join(".codex").join("sessions"), |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
        });
        locator.index_file(home.join(".codex").join("history.jsonl"));
        locator.index_recursive(home.join(".gemini").join("tmp"), |path| {
            path.extension().and_then(|ext| ext.to_str()) == Some("json")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("session-"))
        });
        locator.index_recursive(
            home.join(".gemini")
                .join("antigravity")
                .join("conversations"),
            |path| path.extension().and_then(|ext| ext.to_str()) == Some("pb"),
        );
        locator.index_directories(home.join(".gemini").join("antigravity").join("brain"));
        locator
    }

    fn lookup(&self, hint: &str) -> SourceLookupOutcome {
        let key = hint.to_ascii_lowercase();
        let Some(paths) = self.index.get(&key) else {
            return SourceLookupOutcome::Missing;
        };

        let unique: Vec<PathBuf> = paths
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        match unique.as_slice() {
            [] => SourceLookupOutcome::Missing,
            [only] => SourceLookupOutcome::Unique(only.clone()),
            many => SourceLookupOutcome::Ambiguous(many.to_vec()),
        }
    }

    fn index_recursive<F>(&mut self, root: PathBuf, include: F)
    where
        F: Fn(&Path) -> bool + Copy,
    {
        if !root.exists() {
            return;
        }

        let Ok(read_dir) = fs::read_dir(&root) else {
            return;
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.index_recursive(path, include);
                continue;
            }

            if include(&path) {
                self.add_path(&path);
            }
        }
    }

    fn index_directories(&mut self, root: PathBuf) {
        if !root.exists() {
            return;
        }

        let Ok(read_dir) = fs::read_dir(&root) else {
            return;
        };

        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_dir() {
                self.add_path(&path);
            }
        }
    }

    fn index_file(&mut self, path: PathBuf) {
        if path.exists() {
            self.add_path(&path);
        }
    }

    fn add_path(&mut self, path: &Path) {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return;
        };

        let lower_name = name.to_ascii_lowercase();
        self.index
            .entry(lower_name.clone())
            .or_default()
            .push(path.to_path_buf());

        if let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) {
            let lower_stem = stem.to_ascii_lowercase();
            if lower_stem != lower_name {
                self.index
                    .entry(lower_stem)
                    .or_default()
                    .push(path.to_path_buf());
            }
        }
    }
}
