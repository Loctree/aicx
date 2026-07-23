use anyhow::Result;
#[cfg(feature = "app")]
use chrono::TimeZone;
use chrono::Utc;
use regex::Regex;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use super::{read_store_dir, store_semantic_segments_at_forced};
use crate::chunker::ChunkerConfig;
use crate::sanitize;
use crate::store::atomic_write::atomic_write;
use crate::store::paths::{
    legacy_salvage_dir, legacy_store_base_dir, migration_manifest_path, migration_report_path,
    store_base_dir,
};
#[cfg(feature = "app")]
use crate::timeline::ExtractionConfig;
use crate::timeline::TimelineEntry;

// ============================================================================
// Migration
mod cards_v2;
mod identity;
mod report;
mod source_locator;
mod types;

pub use cards_v2::{
    CardsV2Action, CardsV2Item, CardsV2Manifest, CardsV2Totals, run_cards_v2_migration,
};
#[allow(unused_imports)]
pub use identity::{
    compute_store_recursive_hash, rollback_identity_migration_at, run_identity_migration_at,
};
use report::{print_migration_summary, render_migration_report};
pub(crate) use source_locator::SourceLocator;
use source_locator::SourceLookupOutcome;
pub use types::{
    LegacyItemKind, MigrationAction, MigrationExecution, MigrationItem, MigrationManifest,
    MigrationTotals,
};

// ============================================================================

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

        // Manifest store paths are reported in canonical forward-slash form on
        // every OS (matching the store's canonical chunk refs); `\` -> `/` is a
        // no-op on Unix and keeps Windows manifests consistent + greppable.
        let mut canonical_paths: Vec<String> = plan
            .canonical_paths
            .iter()
            .map(|path| path.display().to_string().replace('\\', "/"))
            .collect();
        canonical_paths.sort();

        let mut salvage_paths: Vec<String> = plan
            .salvage_paths
            .iter()
            .map(|path| path.display().to_string().replace('\\', "/"))
            .collect();
        salvage_paths.sort();

        Self {
            // Manifest identifiers are canonical forward-slash on every OS, like
            // canonical_paths/salvage_paths above: `legacy_group`/`item_id` come
            // from a `Path::display()` (the bundle key), which is `\`-separated
            // on Windows. `\` -> `/` is a no-op on Unix.
            item_id: plan.item_id.replace('\\', "/"),
            legacy_kind: plan.legacy_kind,
            legacy_group: plan.legacy_group.replace('\\', "/"),
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
    Grok,
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

pub(crate) fn run_migration_at(
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

    // Windows absolute paths (`C:\…\file.jsonl`) are what `Path::display()` writes
    // into legacy bundles on Windows runners; the forward-slash `absolute_path_re`
    // above never matches a drive-letter path, so without this pass migration
    // extracts zero direct source candidates and rebuilds nothing (rebuild_items:
    // 0). Accept both separators because serialized JSON content (codex `cwd`,
    // gemini `projectRoot`) can carry forward-slash drive paths too. The leading
    // `(?:^|[^A-Za-z0-9])` guard keeps a URL scheme like `https:` from being read
    // as a drive letter; Unix text has no `C:\`/`C:/` segments so this is inert.
    let windows_path_re = Regex::new(
        r"(?:^|[^A-Za-z0-9])([A-Za-z]:[\\/](?:[A-Za-z0-9._~\-]+[\\/])*[A-Za-z0-9._~\-]+(?:\.[A-Za-z0-9._~-]+)?)",
    )
    .expect("windows legacy source hint regex should compile");

    for capture in windows_path_re.captures_iter(text) {
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
    // Source detection matches provider markers (`/.claude/`, `/antigravity/brain/`,
    // `/.gemini/tmp/`, …) as forward-slash substrings. On Windows the native path
    // separator is `\`, so normalize before matching or every provider check
    // misses and migration classifies no source files.
    let path_str = path
        .to_string_lossy()
        .replace('\\', "/")
        .to_ascii_lowercase();
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
        Some("grok") => {
            if extension.as_deref() == Some("jsonl") && path_str.contains("/.grok/") {
                return Some(SourceFormat::Grok);
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

    if extension.as_deref() == Some("jsonl") && path_str.contains("/.grok/") {
        return Some(SourceFormat::Grok);
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

    // Migration is explicit salvage into a transitional store tree — force
    // the card mill so operator migrate still works while the default CLI
    // path stays dual-body silent.
    let summary =
        store_semantic_segments_at_forced(store_root, &entries, chunker_config, |_, _| {})?;
    Ok(summary.written_paths)
}

/// Slim profile: raw-source rebuild needs the app-only parser dispatch and
/// timeline projection. Failing here routes the item into the existing
/// error path (`SourceProcessingOutcome.error` → `item.errors` →
/// `should_salvage`), so legacy files are preserved via salvage instead of
/// silently dropped.
#[cfg(not(feature = "app"))]
fn extract_entries_from_source(source: &ResolvedSource) -> Result<Vec<TimelineEntry>> {
    anyhow::bail!(
        "legacy {:?} source rebuild ({}) requires the aicx `app` feature; slim loctree-consumer builds preserve legacy items via salvage instead",
        source.format,
        source.path.display()
    )
}

#[cfg(feature = "app")]
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

    let agent = match source.format {
        SourceFormat::Claude => aicx_parser::engine::AgentKind::Claude,
        SourceFormat::Codex => aicx_parser::engine::AgentKind::Codex,
        SourceFormat::Gemini | SourceFormat::GeminiAntigravity => {
            aicx_parser::engine::AgentKind::Gemini
        }
        SourceFormat::Junie => aicx_parser::engine::AgentKind::Junie,
        SourceFormat::Grok => aicx_parser::engine::AgentKind::Grok,
    };
    if source.path.is_dir() {
        anyhow::bail!("migration source must resolve to a concrete session artifact");
    }
    let source_id = source
        .path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("migration-source");
    let session = crate::parser_dispatch::parse_file(agent, source_id, None, &source.path)?;
    let mut entries = crate::output::timeline_entries_from_model(session.model());
    entries.retain(|entry| {
        entry.timestamp >= config.cutoff && (config.include_assistant || entry.role == "user")
    });
    Ok(entries)
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
        && let Some(home) = crate::os_user_home()
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

#[cfg(test)]
mod hint_tests {
    use super::*;

    fn collect(text: &str, agent: Option<&str>) -> (BTreeSet<PathBuf>, BTreeSet<String>) {
        let mut direct = BTreeSet::new();
        let mut lookups = BTreeSet::new();
        let mut hints = BTreeSet::new();
        collect_source_hints_from_text(text, agent, &mut direct, &mut lookups, &mut hints);
        (direct, hints)
    }

    #[test]
    fn windows_drive_path_is_extracted_as_direct_source_candidate() {
        // Regression for the windows-msvc migration build: a legacy bundle line
        // `input: C:\…\rollout-….jsonl` (what `Path::display()` emits on Windows)
        // must surface as a direct candidate, or migration resolves no source and
        // rebuild_items stays 0. Pure string logic — verifiable on any platform.
        let text = r"input: C:\Users\runner\sources\rollout-rebuild-canonical-019be5e4.jsonl";
        let (direct, hints) = collect(text, Some("codex"));
        assert!(
            direct
                .iter()
                .any(|path| path.to_string_lossy().contains("rollout-rebuild-canonical")),
            "windows drive path not captured: {direct:?}"
        );
        assert!(
            hints
                .iter()
                .any(|hint| hint.contains("rollout-rebuild-canonical"))
        );
    }

    #[test]
    fn windows_forward_slash_drive_path_is_extracted() {
        // Serialized JSON content can carry forward-slash drive paths.
        let text = r#"{"input":"D:/data/aicx/rollout-non-repo-019be5e4.jsonl"}"#;
        let (direct, _) = collect(text, Some("codex"));
        assert!(
            direct
                .iter()
                .any(|path| path.to_string_lossy().contains("rollout-non-repo")),
            "forward-slash drive path not captured: {direct:?}"
        );
    }

    #[test]
    fn url_scheme_is_not_mistaken_for_a_drive_letter() {
        // The `https:` in a URL must not be read as drive `s:`. The path tail
        // here carries no source extension, so neither the Unix nor the Windows
        // pass yields a candidate — and crucially none starts with a bogus
        // `s:` drive captured out of `https:`.
        let text = "visit https://example/readme for context";
        let (direct, _) = collect(text, Some("codex"));
        assert!(
            !direct
                .iter()
                .any(|path| path.to_string_lossy().starts_with("s:")),
            "https scheme misread as drive letter: {direct:?}"
        );
    }
}
