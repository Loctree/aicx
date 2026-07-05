//! Cards v1 → v2 in-place store migration (`aicx migrate --cards-v2`).
//!
//! Upgrades existing canonical-store cards to the card schema v2 contract
//! (docs/CARD_CONTRACT.md): the `.meta.json` sidecar gains `schema_version: 2`
//! plus the canonical honesty constants, and the legacy single-line bracket
//! header in the `.md` is rewritten to the equivalent YAML frontmatter block.
//!
//! Safety posture, in order of importance:
//! - **Body bytes never change.** The rewrite replaces only the first
//!   bracket-header line; before any write the body is re-derived through the
//!   same reader (`card_body`) from both the old and the new text and the two
//!   SHA-256 digests must match, else the card is aborted untouched.
//! - **Dry-run is the default.** Nothing on disk moves without `--apply`.
//! - **Sidecar edits are additive.** The sidecar JSON is edited as a
//!   `serde_json::Value` so unknown fields survive; honesty fields are only
//!   set when absent, and `source` is only derived from existing
//!   `source_file` import provenance — never invented.
//! - **Nothing is deleted.** Orphan `.md` files and corrupted sidecars are
//!   skipped with a manifest note.
//!
//! Streaming: the walk visits one card at a time and holds only manifest
//! metadata (paths, old header line, field names) in memory — never file
//! contents — so memory does not scale with store size.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use super::MigrationExecution;
use crate::card_header::{HeaderForm, card_body, header_form, parse_card_header};
use crate::chunker::{
    CARD_CLAIM_SCOPE_SESSION_CLOSE, CARD_FRESHNESS_CONTRACT_HISTORICAL, CARD_SCHEMA_VERSION,
    CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX, ChunkMetadataSidecar,
};
use crate::sanitize;
use crate::store::atomic_write::atomic_write;
use crate::store::dedupe::content_sha256;
use crate::store::paths::canonical_store_dir;
use crate::store::sidecar::sidecar_path_for_chunk;
use crate::store::{is_context_corpus_sidecar, read_store_dir};

const CARDS_V2_MIGRATION_DIRNAME: &str = ".migration";
const CARDS_V2_SUBDIR: &str = "cards-v2";
const CARDS_V2_MANIFEST_FILENAME: &str = "manifest.json";
const CARDS_V2_REPORT_FILENAME: &str = "report.md";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CardsV2Action {
    /// Sidecar upgraded to schema v2; header rewritten when `old_header` is set.
    Upgrade,
    /// `.md` without a readable sidecar — warned and left untouched.
    SkipOrphanMd,
    /// Sidecar present but unreadable/unparseable — left untouched.
    SkipCorruptedSidecar,
    /// Card `.md` itself unreadable — left untouched.
    SkipUnreadableCard,
    /// Context-corpus sidecar; the session-close honesty frame does not apply.
    SkipContextCorpus,
    /// Header rewrite would have changed the body digest — card left untouched.
    AbortBodyHashMismatch,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardsV2Item {
    /// Canonical forward-slash card path (matches the legacy manifest idiom).
    pub card_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sidecar_path: Option<String>,
    pub action: CardsV2Action,
    pub execution: MigrationExecution,
    /// Exact original first line when the bracket header was (or would be)
    /// rewritten — recorded so a reverse pass stays possible.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_header: Option<String>,
    /// Sidecar fields this migration set, e.g. `schema_version=2`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_sidecar_fields: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_content_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_content_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CardsV2Totals {
    /// Every `.md` visited by the walk (including already-v2 skips).
    pub scanned_cards: usize,
    /// Cards whose sidecar was (or would be) upgraded to schema v2.
    pub upgraded_cards: usize,
    /// Subset of upgrades whose bracket header was (or would be) rewritten.
    pub rewritten_headers: usize,
    /// Cards skipped because the sidecar already reports `schema_version >= 2`.
    pub already_v2: usize,
    pub orphan_md: usize,
    pub corrupted_sidecars: usize,
    pub unreadable_cards: usize,
    pub context_corpus_skipped: usize,
    pub aborted_body_hash: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardsV2Manifest {
    pub generated_at: DateTime<Utc>,
    pub root: String,
    pub manifest_path: String,
    pub report_path: String,
    pub dry_run: bool,
    pub totals: CardsV2Totals,
    /// One entry per card that needed an action or a note. Already-v2 cards
    /// are counted in `totals` only — a manifest that re-lists the whole
    /// store on every idempotent re-run would drown the actionable entries.
    pub items: Vec<CardsV2Item>,
}

impl CardsV2Totals {
    fn from_scan(items: &[CardsV2Item], scanned_cards: usize, already_v2: usize) -> Self {
        Self {
            scanned_cards,
            upgraded_cards: items
                .iter()
                .filter(|item| item.action == CardsV2Action::Upgrade)
                .count(),
            rewritten_headers: items
                .iter()
                .filter(|item| item.action == CardsV2Action::Upgrade && item.old_header.is_some())
                .count(),
            already_v2,
            orphan_md: count_action(items, CardsV2Action::SkipOrphanMd),
            corrupted_sidecars: count_action(items, CardsV2Action::SkipCorruptedSidecar),
            unreadable_cards: count_action(items, CardsV2Action::SkipUnreadableCard),
            context_corpus_skipped: count_action(items, CardsV2Action::SkipContextCorpus),
            aborted_body_hash: count_action(items, CardsV2Action::AbortBodyHashMismatch),
        }
    }
}

fn count_action(items: &[CardsV2Item], action: CardsV2Action) -> usize {
    items.iter().filter(|item| item.action == action).count()
}

pub fn run_cards_v2_migration(dry_run: bool, root: Option<PathBuf>) -> Result<CardsV2Manifest> {
    let root = match root {
        Some(root) => root,
        None => canonical_store_dir()?,
    };
    let manifest = run_cards_v2_migration_at(&root, dry_run)?;

    print_cards_v2_summary(&manifest);
    if dry_run {
        println!(
            "[DRY RUN] Would write cards-v2 manifest to {}",
            manifest.manifest_path
        );
        println!(
            "[DRY RUN] Would write cards-v2 report to {}",
            manifest.report_path
        );
    } else {
        println!("Wrote cards-v2 manifest to {}", manifest.manifest_path);
        println!("Wrote cards-v2 report to {}", manifest.report_path);
    }

    Ok(manifest)
}

pub(crate) fn run_cards_v2_migration_at(root: &Path, dry_run: bool) -> Result<CardsV2Manifest> {
    let root = sanitize::validate_dir_path(root)?;

    let mut items = Vec::new();
    let mut scanned_cards = 0usize;
    let mut already_v2 = 0usize;

    walk_card_files(&root, &mut |card_path| {
        scanned_cards += 1;
        match migrate_one_card(card_path, dry_run)? {
            CardOutcome::AlreadyV2 => already_v2 += 1,
            CardOutcome::Item(item) => items.push(item),
        }
        Ok(())
    })?;

    let totals = CardsV2Totals::from_scan(&items, scanned_cards, already_v2);
    let manifest = CardsV2Manifest {
        generated_at: Utc::now(),
        root: display_forward_slash(&root),
        manifest_path: display_forward_slash(&cards_v2_manifest_path(&root)),
        report_path: display_forward_slash(&cards_v2_report_path(&root)),
        dry_run,
        totals,
        items,
    };

    if !dry_run {
        write_cards_v2_artifacts(&root, &manifest)?;
    }

    Ok(manifest)
}

fn cards_v2_migration_dir(root: &Path) -> PathBuf {
    // Dot-prefixed on purpose: the card walk below and the legacy sweep both
    // skip dot-directories, so migration artifacts written inside the walked
    // root can never be picked up as cards on a later run.
    root.join(CARDS_V2_MIGRATION_DIRNAME).join(CARDS_V2_SUBDIR)
}

fn cards_v2_manifest_path(root: &Path) -> PathBuf {
    cards_v2_migration_dir(root).join(CARDS_V2_MANIFEST_FILENAME)
}

fn cards_v2_report_path(root: &Path) -> PathBuf {
    cards_v2_migration_dir(root).join(CARDS_V2_REPORT_FILENAME)
}

fn write_cards_v2_artifacts(root: &Path, manifest: &CardsV2Manifest) -> Result<()> {
    std::fs::create_dir_all(cards_v2_migration_dir(root))?;

    let manifest_path = sanitize::validate_write_path(&cards_v2_manifest_path(root))?;
    atomic_write(
        &manifest_path,
        serde_json::to_string_pretty(manifest)?.as_bytes(),
    )?;

    let report_path = sanitize::validate_write_path(&cards_v2_report_path(root))?;
    atomic_write(&report_path, render_cards_v2_report(manifest).as_bytes())?;

    Ok(())
}

/// Depth-first walk over `.md` card files under `root`, sorted per directory
/// for deterministic manifests. Skips dot-entries (ignore files, migration
/// artifacts) and `quarantine/` directories (already-quarantined orphans are
/// not cards). Holds one directory listing at a time — never the whole store.
fn walk_card_files<F>(dir: &Path, visit: &mut F) -> Result<()>
where
    F: FnMut(&Path) -> Result<()>,
{
    if !dir.is_dir() {
        return Ok(());
    }

    let mut entries: Vec<(String, PathBuf, bool)> = Vec::new();
    for entry in read_store_dir(dir)?.filter_map(|entry| entry.ok()) {
        let name = entry.file_name().to_string_lossy().to_string();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        entries.push((name, entry.path(), file_type.is_dir()));
    }
    entries.sort();

    for (name, path, is_dir) in entries {
        if name.starts_with('.') {
            continue;
        }
        if is_dir {
            if name == "quarantine" {
                continue;
            }
            walk_card_files(&path, visit)?;
        } else if name.ends_with(".md") {
            visit(&path)?;
        }
    }

    Ok(())
}

enum CardOutcome {
    AlreadyV2,
    Item(CardsV2Item),
}

fn migrate_one_card(card_path: &Path, dry_run: bool) -> Result<CardOutcome> {
    let execution = if dry_run {
        MigrationExecution::Planned
    } else {
        MigrationExecution::Executed
    };
    let skip = |action: CardsV2Action, sidecar_path: Option<&Path>, note: String| {
        CardOutcome::Item(CardsV2Item {
            card_path: display_forward_slash(card_path),
            sidecar_path: sidecar_path.map(display_forward_slash),
            action,
            // A skip is a terminal fact of this run either way, but keep the
            // planned/executed distinction so dry-run manifests read as plans.
            execution,
            old_header: None,
            new_sidecar_fields: Vec::new(),
            old_content_sha256: None,
            new_content_sha256: None,
            note: Some(note),
        })
    };

    let sidecar_path = sidecar_path_for_chunk(card_path);
    if !sidecar_path.exists() {
        tracing::warn!(
            target: "aicx::store",
            card = %card_path.display(),
            "cards-v2: orphan .md without sidecar; skipped (never deleted)"
        );
        return Ok(skip(
            CardsV2Action::SkipOrphanMd,
            None,
            "no .meta.json sidecar found; card left untouched".to_string(),
        ));
    }

    let sidecar_raw = match sanitize::read_to_string_validated(&sidecar_path) {
        Ok(raw) => raw,
        Err(error) => {
            return Ok(skip(
                CardsV2Action::SkipCorruptedSidecar,
                Some(&sidecar_path),
                format!("sidecar unreadable: {error}"),
            ));
        }
    };

    // Typed parse decides (schema version, provenance, corpus check); the
    // Value parse is what gets edited and written so unknown fields survive.
    let typed: ChunkMetadataSidecar = match serde_json::from_str(&sidecar_raw) {
        Ok(typed) => typed,
        Err(error) => {
            return Ok(skip(
                CardsV2Action::SkipCorruptedSidecar,
                Some(&sidecar_path),
                format!("sidecar failed schema parse: {error}"),
            ));
        }
    };
    let mut sidecar_value: serde_json::Value = match serde_json::from_str(&sidecar_raw) {
        Ok(serde_json::Value::Object(map)) => serde_json::Value::Object(map),
        Ok(_) => {
            return Ok(skip(
                CardsV2Action::SkipCorruptedSidecar,
                Some(&sidecar_path),
                "sidecar JSON is not an object".to_string(),
            ));
        }
        Err(error) => {
            return Ok(skip(
                CardsV2Action::SkipCorruptedSidecar,
                Some(&sidecar_path),
                format!("sidecar JSON parse failed: {error}"),
            ));
        }
    };

    if typed.schema_version >= CARD_SCHEMA_VERSION {
        return Ok(CardOutcome::AlreadyV2);
    }

    if is_context_corpus_sidecar(&typed) {
        return Ok(skip(
            CardsV2Action::SkipContextCorpus,
            Some(&sidecar_path),
            "context-corpus sidecar; session-close honesty frame not applicable".to_string(),
        ));
    }

    let card_text = match sanitize::read_to_string_validated(card_path) {
        Ok(text) => text,
        Err(error) => {
            return Ok(skip(
                CardsV2Action::SkipUnreadableCard,
                Some(&sidecar_path),
                format!("card .md unreadable: {error}"),
            ));
        }
    };

    let rewrite = rewrite_bracket_header(&card_text);
    if let Some((new_text, old_header)) = &rewrite {
        // Hard body invariant: the same reader must recover byte-identical
        // bodies from the old and the new text. This catches degenerate
        // headers whose "equivalent" frontmatter would not parse as a card
        // header (and with it, any future reader/writer asymmetry).
        let old_body_sha = content_sha256(card_body(&card_text));
        let new_body_sha = content_sha256(card_body(new_text));
        if old_body_sha != new_body_sha {
            return Ok(CardOutcome::Item(CardsV2Item {
                card_path: display_forward_slash(card_path),
                sidecar_path: Some(display_forward_slash(&sidecar_path)),
                action: CardsV2Action::AbortBodyHashMismatch,
                execution,
                old_header: Some(old_header.clone()),
                new_sidecar_fields: Vec::new(),
                old_content_sha256: typed.content_sha256.clone(),
                new_content_sha256: None,
                note: Some(format!(
                    "body sha256 changed under header rewrite ({old_body_sha} -> {new_body_sha}); card left untouched"
                )),
            }));
        }
    }

    let old_content_sha256 = typed.content_sha256.clone();
    let new_card_text = rewrite
        .as_ref()
        .map(|(new_text, _)| new_text.as_str())
        .unwrap_or(&card_text);
    let new_content_sha256 = content_sha256(new_card_text);
    let new_sidecar_fields = upgrade_sidecar_value(&mut sidecar_value, &typed, &new_content_sha256);

    if !dry_run {
        // Write the .md before the sidecar: if the process dies between the
        // two writes, the sidecar still reports v1 and the next run converges
        // (sidecar-only upgrade). The reverse order would stamp v2 and make
        // the idempotence skip hide the unrewritten header forever.
        if let Some((new_text, _)) = &rewrite {
            let card_write = sanitize::validate_write_path(card_path)?;
            atomic_write(&card_write, new_text.as_bytes())
                .with_context(|| format!("cards-v2: rewrite {}", card_path.display()))?;
        }
        let sidecar_write = sanitize::validate_write_path(&sidecar_path)?;
        atomic_write(
            &sidecar_write,
            serde_json::to_string_pretty(&sidecar_value)?.as_bytes(),
        )
        .with_context(|| format!("cards-v2: upgrade sidecar {}", sidecar_path.display()))?;
    }

    Ok(CardOutcome::Item(CardsV2Item {
        card_path: display_forward_slash(card_path),
        sidecar_path: Some(display_forward_slash(&sidecar_path)),
        action: CardsV2Action::Upgrade,
        execution,
        old_header: rewrite.map(|(_, old_header)| old_header),
        new_sidecar_fields,
        old_content_sha256,
        new_content_sha256: Some(new_content_sha256),
        note: None,
    }))
}

/// Replace the first bracket-header line with the equivalent card v2 YAML
/// frontmatter, leaving every byte after that first line untouched. Returns
/// `None` when the card does not start with a bracket header (nothing to
/// rewrite; the sidecar-only upgrade still applies).
fn rewrite_bracket_header(text: &str) -> Option<(String, String)> {
    if !matches!(header_form(text), Some(HeaderForm::Bracket { .. })) {
        return None;
    }
    let header = parse_card_header(text)?;

    let (first_line, rest) = match text.split_once('\n') {
        Some((first_line, rest)) => (first_line, Some(rest)),
        None => (text, None),
    };

    let mut frontmatter = String::from("---\n");
    if let Some(project) = &header.project {
        frontmatter.push_str(&format!("project: {project}\n"));
    }
    if let Some(agent) = &header.agent {
        frontmatter.push_str(&format!("agent: {agent}\n"));
    }
    if let Some(date) = &header.date {
        frontmatter.push_str(&format!("date: {date}\n"));
    }
    if let Some(frame_kind) = header.frame_kind {
        frontmatter.push_str(&format!("frame_kind: {frame_kind}\n"));
    }
    frontmatter.push_str("schema: card.v2\n---");

    let new_text = match rest {
        Some(rest) => format!("{frontmatter}\n{rest}"),
        None => frontmatter,
    };
    Some((new_text, first_line.to_string()))
}

/// Additive schema v2 upgrade on the raw sidecar JSON object. Sets
/// `schema_version` to the current contract version, fills the honesty
/// constants only when absent, and derives `source.path` from existing
/// `source_file` import provenance only — sha256/span are never invented.
/// Returns the list of fields actually set, for the manifest.
fn upgrade_sidecar_value(
    value: &mut serde_json::Value,
    typed: &ChunkMetadataSidecar,
    new_content_sha256: &str,
) -> Vec<String> {
    let Some(object) = value.as_object_mut() else {
        return Vec::new();
    };
    let mut set_fields = Vec::new();

    object.insert(
        "schema_version".to_string(),
        serde_json::json!(CARD_SCHEMA_VERSION),
    );
    set_fields.push(format!("schema_version={CARD_SCHEMA_VERSION}"));

    object.insert(
        "migrated_from_schema".to_string(),
        serde_json::json!(typed.schema_version),
    );
    set_fields.push(format!("migrated_from_schema={}", typed.schema_version));

    for (key, canonical) in [
        ("claim_scope", CARD_CLAIM_SCOPE_SESSION_CLOSE),
        ("freshness_contract", CARD_FRESHNESS_CONTRACT_HISTORICAL),
        (
            "verification_state",
            CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX,
        ),
    ] {
        if !object.contains_key(key) {
            object.insert(key.to_string(), serde_json::json!(canonical));
            set_fields.push(format!("{key}={canonical}"));
        }
    }

    if !object.contains_key("source")
        && let Some(source_file) = &typed.source_file
    {
        object.insert(
            "source".to_string(),
            serde_json::json!({ "path": source_file }),
        );
        set_fields.push(format!("source.path={source_file}"));
    }

    object.insert(
        "content_sha256".to_string(),
        serde_json::json!(new_content_sha256),
    );
    set_fields.push(format!("content_sha256={new_content_sha256}"));

    set_fields
}

fn display_forward_slash(path: &Path) -> String {
    // Manifest paths are canonical forward-slash on every OS, matching the
    // legacy migration manifest idiom (`\` -> `/` is a no-op on Unix).
    path.display().to_string().replace('\\', "/")
}

fn render_cards_v2_report(manifest: &CardsV2Manifest) -> String {
    let mut report = String::new();
    report.push_str("# AICX Cards v2 Migration Report\n\n");
    report.push_str(&format!(
        "- Generated at: `{}`\n",
        manifest.generated_at.to_rfc3339()
    ));
    report.push_str(&format!("- Dry run: `{}`\n", manifest.dry_run));
    report.push_str(&format!("- Root: `{}`\n", manifest.root));
    report.push_str(&format!("- Manifest: `{}`\n", manifest.manifest_path));
    report.push_str(&format!("- Report: `{}`\n\n", manifest.report_path));

    report.push_str("## Summary\n\n");
    let totals = &manifest.totals;
    report.push_str(&format!("- Scanned cards: `{}`\n", totals.scanned_cards));
    report.push_str(&format!(
        "- Upgraded cards: `{}` (`{}` header rewrite(s))\n",
        totals.upgraded_cards, totals.rewritten_headers
    ));
    report.push_str(&format!("- Already v2: `{}`\n", totals.already_v2));
    report.push_str(&format!("- Orphan .md skipped: `{}`\n", totals.orphan_md));
    report.push_str(&format!(
        "- Corrupted sidecars skipped: `{}`\n",
        totals.corrupted_sidecars
    ));
    report.push_str(&format!(
        "- Unreadable cards skipped: `{}`\n",
        totals.unreadable_cards
    ));
    report.push_str(&format!(
        "- Context-corpus skipped: `{}`\n",
        totals.context_corpus_skipped
    ));
    report.push_str(&format!(
        "- Aborted on body-hash mismatch: `{}`\n\n",
        totals.aborted_body_hash
    ));

    push_cards_report_section(
        &mut report,
        if manifest.dry_run {
            "Planned Upgrades"
        } else {
            "Upgraded"
        },
        manifest
            .items
            .iter()
            .filter(|item| item.action == CardsV2Action::Upgrade),
    );
    push_cards_report_section(
        &mut report,
        "Skipped / Aborted",
        manifest
            .items
            .iter()
            .filter(|item| item.action != CardsV2Action::Upgrade),
    );

    report
}

fn push_cards_report_section<'a, I>(report: &mut String, title: &str, items: I)
where
    I: Iterator<Item = &'a CardsV2Item>,
{
    report.push_str(&format!("## {}\n\n", title));
    let mut wrote = false;

    for item in items {
        wrote = true;
        report.push_str(&format!("- `{}`\n", item.card_path));
        if let Some(old_header) = &item.old_header {
            report.push_str(&format!("  old header: `{}`\n", old_header));
        }
        if !item.new_sidecar_fields.is_empty() {
            report.push_str(&format!(
                "  set: `{}`\n",
                item.new_sidecar_fields.join("`, `")
            ));
        }
        if let (Some(old), Some(new)) = (&item.old_content_sha256, &item.new_content_sha256) {
            report.push_str(&format!("  content_sha256: `{old}` -> `{new}`\n"));
        }
        if let Some(note) = &item.note {
            report.push_str(&format!("  note: `{}`\n", note));
        }
    }

    if !wrote {
        report.push_str("- none\n");
    }

    report.push('\n');
}

fn print_cards_v2_summary(manifest: &CardsV2Manifest) {
    let totals = &manifest.totals;
    println!(
        "Cards-v2 sweep: {} card(s) scanned under {}.",
        totals.scanned_cards, manifest.root
    );
    let verb = if manifest.dry_run {
        "Planned"
    } else {
        "Executed"
    };
    println!(
        "{}: {} upgrade(s) ({} header rewrite(s)); {} already v2.",
        verb, totals.upgraded_cards, totals.rewritten_headers, totals.already_v2
    );
    println!(
        "Skipped: {} orphan .md, {} corrupted sidecar(s), {} unreadable card(s), {} context-corpus; {} aborted on body-hash mismatch.",
        totals.orphan_md,
        totals.corrupted_sidecars,
        totals.unreadable_cards,
        totals.context_corpus_skipped,
        totals.aborted_body_hash
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::time::SystemTime;

    fn cards_v2_test_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "aicx-cards-v2-{name}-{}-{}",
            std::process::id(),
            Utc::now().timestamp_nanos_opt().unwrap_or_default()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    const V1_BODY: &str = "\n[signals]\nDecision:\n- [decision] keep the seam\n[/signals]\n\n[13:44:56] user: kind: transcript\n[13:45:02] assistant: done\n";

    fn v1_card_text() -> String {
        format!(
            "[project: demo/repo | agent: claude | date: 2026-03-15 | frame_kind: user_msg]\n{V1_BODY}"
        )
    }

    fn v1_card_text_without_signals() -> String {
        "[project: demo/repo | agent: claude | date: 2026-03-15 | frame_kind: user_msg]\n\n[13:44:56] user: kind: transcript\n[13:45:02] assistant: done\n".to_string()
    }

    fn v1_sidecar_json(extra_unknown_field: bool, source_file: Option<&str>) -> String {
        v1_sidecar_json_with_hash(extra_unknown_field, source_file, "__AUTO__")
    }

    fn v1_sidecar_json_with_hash(
        extra_unknown_field: bool,
        source_file: Option<&str>,
        content_sha256: &str,
    ) -> String {
        let mut object = serde_json::json!({
            "id": "demo/repo_claude_2026-03-15_001",
            "project": "demo/repo",
            "agent": "claude",
            "date": "2026-03-15",
            "session_id": "sess-1",
            "kind": "conversations",
            "frame_kind": "user_msg",
            "content_sha256": content_sha256
        });
        if extra_unknown_field {
            object["future_field"] = serde_json::json!("survives");
        }
        if let Some(source_file) = source_file {
            object["source_file"] = serde_json::json!(source_file);
        }
        serde_json::to_string_pretty(&object).unwrap()
    }

    fn write_card(dir: &Path, stem: &str, md: &str, sidecar: Option<&str>) -> (PathBuf, PathBuf) {
        fs::create_dir_all(dir).unwrap();
        let md_path = dir.join(format!("{stem}.md"));
        fs::write(&md_path, md).unwrap();
        let sidecar_path = dir.join(format!("{stem}.meta.json"));
        if let Some(sidecar) = sidecar {
            let sidecar_bytes = match serde_json::from_str::<serde_json::Value>(sidecar) {
                Ok(mut value) => {
                    if value
                        .get("content_sha256")
                        .and_then(serde_json::Value::as_str)
                        == Some("__AUTO__")
                    {
                        value["content_sha256"] = serde_json::json!(content_sha256(md));
                    }
                    serde_json::to_vec_pretty(&value).unwrap()
                }
                Err(_) => sidecar.as_bytes().to_vec(),
            };
            fs::write(&sidecar_path, sidecar_bytes).unwrap();
        }
        (md_path, sidecar_path)
    }

    fn mtime(path: &Path) -> SystemTime {
        fs::metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn dry_run_plans_upgrade_and_mutates_nothing() {
        let root = cards_v2_test_root("dry-run");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let (md_path, sidecar_path) = write_card(
            &dir,
            "chunk_001",
            &v1_card_text(),
            Some(&v1_sidecar_json(false, None)),
        );

        let md_before = fs::read(&md_path).unwrap();
        let sidecar_before = fs::read(&sidecar_path).unwrap();
        let md_mtime = mtime(&md_path);
        let sidecar_mtime = mtime(&sidecar_path);

        let manifest = run_cards_v2_migration_at(&root, true).expect("dry run");

        assert_eq!(manifest.totals.scanned_cards, 1);
        assert_eq!(manifest.totals.upgraded_cards, 1);
        assert_eq!(manifest.totals.rewritten_headers, 1);
        let item = manifest.items.first().expect("planned item");
        assert_eq!(item.action, CardsV2Action::Upgrade);
        assert_eq!(item.execution, MigrationExecution::Planned);
        assert!(
            item.old_header
                .as_deref()
                .is_some_and(|header| header.starts_with("[project: demo/repo"))
        );

        // Hash + mtime proof: nothing on disk moved, not even the manifest.
        assert_eq!(fs::read(&md_path).unwrap(), md_before);
        assert_eq!(fs::read(&sidecar_path).unwrap(), sidecar_before);
        assert_eq!(mtime(&md_path), md_mtime);
        assert_eq!(mtime(&sidecar_path), sidecar_mtime);
        assert!(!root.join(CARDS_V2_MIGRATION_DIRNAME).exists());
    }

    #[test]
    fn apply_upgrades_header_and_sidecar_with_body_bytes_invariant() {
        let root = cards_v2_test_root("apply");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let (md_path, sidecar_path) = write_card(
            &dir,
            "chunk_001",
            &v1_card_text(),
            Some(&v1_sidecar_json(true, None)),
        );
        let old_text = fs::read_to_string(&md_path).unwrap();

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        assert_eq!(manifest.totals.upgraded_cards, 1);

        let new_text = fs::read_to_string(&md_path).unwrap();
        assert!(new_text.starts_with("---\nproject: demo/repo\nagent: claude\n"));
        assert!(new_text.contains("schema: card.v2\n---\n"));
        // Body bytes are identical through the shared reader, and the raw
        // tail after the header line is byte-for-byte the original.
        assert_eq!(card_body(&new_text), card_body(&old_text));
        assert_eq!(
            content_sha256(card_body(&new_text)),
            content_sha256(V1_BODY.trim_start_matches('\n'))
        );

        let sidecar: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&sidecar_path).unwrap()).unwrap();
        assert_eq!(sidecar["schema_version"], serde_json::json!(2));
        assert_eq!(sidecar["migrated_from_schema"], serde_json::json!(1));
        assert_eq!(
            sidecar["claim_scope"],
            serde_json::json!(CARD_CLAIM_SCOPE_SESSION_CLOSE)
        );
        assert_eq!(
            sidecar["freshness_contract"],
            serde_json::json!(CARD_FRESHNESS_CONTRACT_HISTORICAL)
        );
        assert_eq!(
            sidecar["verification_state"],
            serde_json::json!(CARD_VERIFICATION_STATE_NOT_VERIFIED_BY_AICX)
        );
        // Unknown fields and untouched v1 fields survive the rewrite.
        assert_eq!(sidecar["future_field"], serde_json::json!("survives"));
        let old_hash = content_sha256(&old_text);
        let new_hash = content_sha256(&new_text);
        assert_ne!(old_hash, new_hash);
        assert_eq!(sidecar["content_sha256"], serde_json::json!(new_hash));
        let item = manifest.items.first().expect("upgrade item");
        assert_eq!(item.old_content_sha256.as_deref(), Some(old_hash.as_str()));
        assert_eq!(item.new_content_sha256.as_deref(), Some(new_hash.as_str()));
        assert!(
            item.new_sidecar_fields
                .iter()
                .any(|field| field == "migrated_from_schema=1")
        );
        // No source provenance existed, so none may be invented.
        assert!(sidecar.get("source").is_none());

        // Manifest + report landed under the dot-dir.
        assert!(cards_v2_manifest_path(&root).exists());
        assert!(cards_v2_report_path(&root).exists());
    }

    #[test]
    fn apply_refreshes_stale_content_sha256_after_header_rewrite() {
        let root = cards_v2_test_root("stale-hash");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let (md_path, sidecar_path) = write_card(
            &dir,
            "chunk_001",
            &v1_card_text(),
            Some(&v1_sidecar_json_with_hash(false, None, "feedbeef")),
        );

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        let new_text = fs::read_to_string(&md_path).unwrap();
        let refreshed_hash = content_sha256(&new_text);
        let sidecar: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&sidecar_path).unwrap()).unwrap();

        assert_ne!(sidecar["content_sha256"], serde_json::json!("feedbeef"));
        assert_eq!(sidecar["content_sha256"], serde_json::json!(refreshed_hash));
        let item = manifest.items.first().expect("upgrade item");
        assert_eq!(item.old_content_sha256.as_deref(), Some("feedbeef"));
        assert_eq!(
            item.new_content_sha256.as_deref(),
            Some(refreshed_hash.as_str())
        );
    }

    #[test]
    fn second_apply_is_idempotent_with_zero_actions() {
        let root = cards_v2_test_root("idempotent");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let (md_path, sidecar_path) = write_card(
            &dir,
            "chunk_001",
            &v1_card_text(),
            Some(&v1_sidecar_json(false, None)),
        );

        run_cards_v2_migration_at(&root, false).expect("first apply");
        let md_after_first = fs::read(&md_path).unwrap();
        let sidecar_after_first = fs::read(&sidecar_path).unwrap();

        let second = run_cards_v2_migration_at(&root, false).expect("second apply");
        assert_eq!(second.totals.upgraded_cards, 0);
        assert_eq!(second.totals.rewritten_headers, 0);
        assert_eq!(second.totals.already_v2, 1);
        assert!(second.items.is_empty());
        assert_eq!(fs::read(&md_path).unwrap(), md_after_first);
        assert_eq!(fs::read(&sidecar_path).unwrap(), sidecar_after_first);
    }

    #[test]
    fn degenerate_bracket_header_aborts_on_body_hash_check_and_leaves_card_untouched() {
        let root = cards_v2_test_root("degenerate");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        // Empty project value: every identity field parses to None, so the
        // "equivalent" frontmatter carries no card field and would not parse
        // as a header — the reader would then see the whole file as body.
        // The hard body-hash check must catch exactly this and abort.
        let degenerate = format!("[project: ]\n{V1_BODY}");
        let (md_path, sidecar_path) = write_card(
            &dir,
            "chunk_001",
            &degenerate,
            Some(&v1_sidecar_json(false, None)),
        );

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        assert_eq!(manifest.totals.aborted_body_hash, 1);
        assert_eq!(manifest.totals.upgraded_cards, 0);
        let item = manifest.items.first().expect("abort item");
        assert_eq!(item.action, CardsV2Action::AbortBodyHashMismatch);
        assert_eq!(item.old_header.as_deref(), Some("[project: ]"));

        // Aborted card is fully untouched — md AND sidecar stay v1.
        assert_eq!(fs::read_to_string(&md_path).unwrap(), degenerate);
        let sidecar: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&sidecar_path).unwrap()).unwrap();
        assert!(sidecar.get("schema_version").is_none());
    }

    #[test]
    fn orphan_md_without_sidecar_is_warned_skipped_and_never_deleted() {
        let root = cards_v2_test_root("orphan");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let (md_path, _) = write_card(&dir, "chunk_001", &v1_card_text(), None);

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        assert_eq!(manifest.totals.orphan_md, 1);
        assert_eq!(manifest.totals.upgraded_cards, 0);
        let item = manifest.items.first().expect("orphan item");
        assert_eq!(item.action, CardsV2Action::SkipOrphanMd);
        assert!(item.note.is_some());
        assert!(md_path.exists());
        assert_eq!(fs::read_to_string(&md_path).unwrap(), v1_card_text());
    }

    #[test]
    fn corrupted_sidecar_is_skipped_with_manifest_note_and_untouched() {
        let root = cards_v2_test_root("corrupted");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let (md_path, sidecar_path) = write_card(
            &dir,
            "chunk_001",
            &v1_card_text(),
            Some("{ this is not json"),
        );

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        assert_eq!(manifest.totals.corrupted_sidecars, 1);
        assert_eq!(manifest.totals.upgraded_cards, 0);
        let item = manifest.items.first().expect("corrupted item");
        assert_eq!(item.action, CardsV2Action::SkipCorruptedSidecar);
        assert!(
            item.note
                .as_deref()
                .is_some_and(|note| note.contains("parse"))
        );
        assert_eq!(fs::read_to_string(&md_path).unwrap(), v1_card_text());
        assert_eq!(
            fs::read_to_string(&sidecar_path).unwrap(),
            "{ this is not json"
        );
    }

    #[test]
    fn already_v2_sidecar_is_skipped_without_an_item() {
        let root = cards_v2_test_root("already-v2");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let v2_sidecar = serde_json::to_string_pretty(&serde_json::json!({
            "id": "demo/repo_claude_2026-03-15_001",
            "schema_version": 2,
            "project": "demo/repo",
            "agent": "claude",
            "date": "2026-03-15",
            "session_id": "sess-1",
            "kind": "conversations"
        }))
        .unwrap();
        let v2_md = "---\nproject: demo/repo\nagent: claude\ndate: 2026-03-15\nschema: card.v2\n---\n\nbody\n";
        let (md_path, _) = write_card(&dir, "chunk_001", v2_md, Some(&v2_sidecar));

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        assert_eq!(manifest.totals.scanned_cards, 1);
        assert_eq!(manifest.totals.already_v2, 1);
        assert!(manifest.items.is_empty());
        assert_eq!(fs::read_to_string(&md_path).unwrap(), v2_md);
    }

    #[test]
    fn headerless_md_gets_sidecar_only_upgrade_with_md_untouched() {
        let root = cards_v2_test_root("headerless");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let headerless = "plain body without any card header\nsecond line\n";
        let (md_path, sidecar_path) = write_card(
            &dir,
            "chunk_001",
            headerless,
            Some(&v1_sidecar_json(false, None)),
        );

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        assert_eq!(manifest.totals.upgraded_cards, 1);
        assert_eq!(manifest.totals.rewritten_headers, 0);
        let item = manifest.items.first().expect("upgrade item");
        assert_eq!(item.old_header, None);

        assert_eq!(fs::read_to_string(&md_path).unwrap(), headerless);
        let sidecar: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&sidecar_path).unwrap()).unwrap();
        assert_eq!(sidecar["schema_version"], serde_json::json!(2));
    }

    #[test]
    fn source_pointer_derived_only_from_source_file_provenance() {
        let root = cards_v2_test_root("source");
        let with_dir = root.join("demo/repo/2026_0315/conversations/claude");
        let (_, with_sidecar_path) = write_card(
            &with_dir,
            "chunk_001",
            &v1_card_text(),
            Some(&v1_sidecar_json(
                false,
                Some("/home/user/.claude/session.jsonl"),
            )),
        );
        let (_, without_sidecar_path) = write_card(
            &with_dir,
            "chunk_002",
            &v1_card_text(),
            Some(&v1_sidecar_json(false, None)),
        );

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        assert_eq!(manifest.totals.upgraded_cards, 2);

        let with_source: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&with_sidecar_path).unwrap()).unwrap();
        assert_eq!(
            with_source["source"],
            serde_json::json!({ "path": "/home/user/.claude/session.jsonl" })
        );
        // sha256/span are not derivable from existing fields — never invented.
        assert!(with_source["source"].get("sha256").is_none());
        assert!(with_source["source"].get("span").is_none());

        let without_source: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&without_sidecar_path).unwrap()).unwrap();
        assert!(without_source.get("source").is_none());
    }

    // The composition half of this suite calls the corpus validator, which is
    // app-surface — the slim `loctree-consumer` profile compiles this crate
    // without `crate::corpus`, so the test must be gated with it.
    #[cfg(feature = "app")]
    #[test]
    fn apply_then_validate_composes_for_migrated_legacy_cards() {
        let root = cards_v2_test_root("compose-validate");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        let (with_source_md, with_source_sidecar) = write_card(
            &dir,
            "chunk_001",
            &v1_card_text_without_signals(),
            Some(&v1_sidecar_json(
                false,
                Some("/home/user/.claude/session.jsonl"),
            )),
        );
        let (legacy_gap_md, legacy_gap_sidecar) = write_card(
            &dir,
            "chunk_002",
            &v1_card_text(),
            Some(&v1_sidecar_json(false, None)),
        );

        let manifest = run_cards_v2_migration_at(&root, false).expect("apply");
        assert_eq!(manifest.totals.upgraded_cards, 2);
        assert_eq!(manifest.totals.rewritten_headers, 2);

        let findings: Vec<_> = [&with_source_md, &legacy_gap_md]
            .into_iter()
            .flat_map(|path| crate::corpus::validate_card(path))
            .collect();
        let errors: Vec<_> = findings
            .iter()
            .filter(|finding| finding.severity == "error")
            .collect();
        assert!(errors.is_empty(), "unexpected hard violations: {errors:#?}");
        let warning_classes: Vec<_> = findings
            .iter()
            .filter(|finding| finding.severity == "warn")
            .map(|finding| finding.class.as_str())
            .collect();
        assert_eq!(
            warning_classes,
            vec!["migrated_missing_source", "migrated_signals_unbackfilled"]
        );

        for (md_path, sidecar_path) in [
            (&with_source_md, &with_source_sidecar),
            (&legacy_gap_md, &legacy_gap_sidecar),
        ] {
            let markdown = fs::read_to_string(md_path).unwrap();
            let sidecar: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(sidecar_path).unwrap()).unwrap();
            assert_eq!(sidecar["migrated_from_schema"], serde_json::json!(1));
            assert_eq!(
                sidecar["content_sha256"],
                serde_json::json!(content_sha256(&markdown))
            );
        }
    }

    #[test]
    fn walk_skips_dot_dirs_and_quarantine() {
        let root = cards_v2_test_root("walk-skips");
        let dir = root.join("demo/repo/2026_0315/conversations/claude");
        write_card(
            &dir,
            "chunk_001",
            &v1_card_text(),
            Some(&v1_sidecar_json(false, None)),
        );
        // Quarantined orphans and dot-dir artifacts must never be visited.
        write_card(
            &dir.join("quarantine"),
            "chunk_001-orphan-1",
            "orphan body",
            None,
        );
        write_card(
            &root.join(".migration").join("cards-v2"),
            "report",
            "not a card",
            None,
        );

        let manifest = run_cards_v2_migration_at(&root, true).expect("dry run");
        assert_eq!(manifest.totals.scanned_cards, 1);
        assert_eq!(manifest.totals.orphan_md, 0);
    }
}
