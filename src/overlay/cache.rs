//! Derived full-history feed cache. This is deliberately not the lexical
//! extract cache: overlay must retain the census reader's complete conversation
//! and run the existing global classifier/caps independently for both lanes.

use super::{OverlayBuildStats, OverlayFeedItem, atomic_write_json, deduplicated_catalog_feed};
use crate::catalog::CatalogEntry;
use crate::source_index::ConversationCoverage;
use crate::source_path::SourceAllowlist;
use crate::timeline::TimelineEntry;
use anyhow::{Context, Result, bail};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Component, Path, PathBuf};
use std::time::UNIX_EPOCH;

/// Bump for feed classification, census shaping, or frame serialization changes.
const CACHE_SCHEMA: &str = "aicx.overlay.catalog-feed.v1";
const CACHE_OWNER_SCHEMA: &str = "aicx.overlay.catalog-cache-owner.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SourceFingerprint {
    len: u64,
    modified_ns: u128,
    created_ns: Option<u128>,
    // Inode + ctime detect atomic replacement and same-size, restored-mtime
    // edits. Metadata-only warm checks must not mistake these for unchanged.
    platform_identity: Vec<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
enum SourceState {
    Ready {
        path: PathBuf,
        fingerprint: SourceFingerprint,
    },
    Missing,
    Unavailable {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SourceRow {
    entry: CatalogEntry,
    state: SourceState,
}

struct Snapshot {
    policy: String,
    key: String,
    rows: Vec<SourceRow>,
}

#[derive(Serialize, Deserialize)]
struct FeedCache {
    schema: String,
    key: String,
    checksum: String,
    items: Vec<OverlayFeedItem>,
    coverage_notes: Vec<String>,
}

#[derive(Serialize, Deserialize)]
struct ConversationCache {
    schema: String,
    key: String,
    checksum: String,
    frames: Vec<TimelineEntry>,
    coverage: ConversationCoverage,
}

#[derive(Serialize, Deserialize)]
struct OutputReceipt {
    schema: String,
    revision: String,
    checksum: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Eq)]
struct CacheOwner {
    schema: String,
    repo_id: String,
    canonical_home_digest: String,
}

pub(super) fn output_has_receipt(root: &Path, output: &super::OverlayDocument) -> Result<bool> {
    Ok(
        read_json::<OutputReceipt>(root, &root.join("materialized-output-v1.json"))?.is_some_and(
            |receipt| {
                receipt.schema == CACHE_SCHEMA
                    && receipt.revision == output.overlay_revision
                    && digest(output).is_ok_and(|checksum| receipt.checksum == checksum)
            },
        ),
    )
}

pub(super) fn write_output_receipt(root: &Path, output: &super::OverlayDocument) -> Result<()> {
    atomic_write_json(
        &root.join("materialized-output-v1.json"),
        &OutputReceipt {
            schema: CACHE_SCHEMA.to_owned(),
            revision: output.overlay_revision.clone(),
            checksum: digest(output)?,
        },
    )
}

pub(super) fn load_catalog_feed(
    home: &Path,
    repo_id: &str,
    root: &Path,
    rebuild: bool,
) -> Result<(Vec<OverlayFeedItem>, OverlayBuildStats)> {
    let before = snapshot(home, repo_id)?;
    ensure_cache_owner(root, home, repo_id)?;
    let mut stats = OverlayBuildStats::default();
    let feed_path = root.join("catalog-feed-v1.json");
    let mut cacheable = true;
    for row in &before.rows {
        match &row.state {
            SourceState::Missing => report_skip(&row.entry, "source file is missing"),
            SourceState::Unavailable { reason } => {
                cacheable = false;
                report_skip(&row.entry, reason);
            }
            SourceState::Ready { .. } => {}
        }
    }
    if !rebuild
        && cacheable
        && let Some(saved) = read_json::<FeedCache>(root, &feed_path)?
        && saved.schema == CACHE_SCHEMA
        && saved.key == before.key
        && saved.checksum == digest(&(&saved.items, &saved.coverage_notes))?
    {
        ensure_unchanged(&before, &snapshot(home, repo_id)?)?;
        prune_obsolete_source_slots(root, &before)?;
        stats.feed_cache_hit = true;
        stats.source_sessions_reused = before
            .rows
            .iter()
            .filter(|row| matches!(row.state, SourceState::Ready { .. }))
            .count();
        for note in saved.coverage_notes {
            eprintln!("overlay: {note}");
        }
        return Ok((saved.items, stats));
    }

    let mut conversations = Vec::new();
    let mut coverage_notes = Vec::new();
    for row in &before.rows {
        let SourceState::Ready { path, .. } = &row.state else {
            continue;
        };
        let key = digest(&(&before.policy, row))?;
        // One replaceable slot per catalog source identity, not one permanent
        // file for each append. A changed row cannot reuse the previous key.
        let slot = digest(&(
            &row.entry.agent,
            &row.entry.session_id,
            &row.entry.source_path,
        ))?;
        let source_cache = root.join(format!("catalog-source-v1-{slot}.json"));
        let saved = if rebuild {
            None
        } else {
            read_json::<ConversationCache>(root, &source_cache)?
        };
        let (frames, coverage) = if let Some(saved) = saved
            && saved.schema == CACHE_SCHEMA
            && saved.key == key
            && saved.coverage.cacheable()
            && saved.checksum == digest(&(&saved.frames, &saved.coverage))?
        {
            stats.source_sessions_reused += 1;
            (saved.frames, saved.coverage)
        } else {
            stats.source_sessions_parsed += 1;
            let read = crate::source_index::read_catalog_conversation_checked_at(home, &row.entry);
            // Even a parser failure must not hide a source changing underneath
            // us. Re-stat after the read before using or persisting its frames.
            let allow = source_allowlist(home);
            if source_state(&allow, &row.entry.source_path) != row.state {
                bail!(
                    "overlay source changed during read: {}; retry after the writer settles",
                    row.entry.source_path
                );
            }
            match read {
                Ok((read_path, frames, coverage)) => {
                    if read_path != *path {
                        bail!("overlay source changed resolved path during read");
                    }
                    if coverage.cacheable() {
                        atomic_write_json(
                            &source_cache,
                            &ConversationCache {
                                schema: CACHE_SCHEMA.to_owned(),
                                key,
                                checksum: digest(&(&frames, &coverage))?,
                                frames: frames.clone(),
                                coverage: coverage.clone(),
                            },
                        )?;
                    } else {
                        cacheable = false;
                        report_skip(
                            &row.entry,
                            "partial parser coverage; current claims are not cached",
                        );
                    }
                    (frames, coverage)
                }
                Err(error) => {
                    cacheable = false;
                    report_skip(&row.entry, &format!("{error:#}"));
                    continue;
                }
            }
        };
        if let ConversationCoverage::BoundedProjection { skipped_records } = coverage {
            let note = format!(
                "bounded_projection agent={} session_id={} skipped_oversized_records={skipped_records}; cached coverage is not CompleteVisible",
                row.entry.agent, row.entry.session_id
            );
            eprintln!("overlay: {note}");
            coverage_notes.push(note);
        }
        conversations.push((row.entry.clone(), path.clone(), frames));
    }
    let records =
        crate::intents::extract_overlay_intents_from_conversations(repo_id, &conversations)?;
    let items = deduplicated_catalog_feed(records, repo_id);
    ensure_unchanged(&before, &snapshot(home, repo_id)?)?;
    if cacheable {
        atomic_write_json(
            &feed_path,
            &FeedCache {
                schema: CACHE_SCHEMA.to_owned(),
                key: before.key.clone(),
                checksum: digest(&(&items, &coverage_notes))?,
                items: items.clone(),
                coverage_notes,
            },
        )?;
        // Only an atomically published, complete/cacheable feed authorizes
        // retirement of no-longer-selected private conversation slots.
        prune_obsolete_source_slots(root, &before)?;
    }
    Ok((items, stats))
}

fn snapshot(home: &Path, repo_id: &str) -> Result<Snapshot> {
    let user_home = crate::os_user_home().unwrap_or_else(|| home.to_path_buf());
    let allow = SourceAllowlist::for_operator(&user_home, home);
    let ignore = crate::legacy_archive::load_repo_path_ignore(home, &user_home)?;
    let roots = allow
        .roots()
        .iter()
        .map(|root| (root, root.canonicalize().ok()))
        .collect::<Vec<_>>();
    let policy = digest(&(
        CACHE_SCHEMA,
        env!("CARGO_PKG_VERSION"),
        option_env!("AICX_GIT_COMMIT"),
        crate::source_index::SIGNAL_FILTER_VERSION,
        home,
        repo_id,
        roots,
        ignore.fingerprint(),
    ))?;
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid Unix epoch");
    let mut rows = Vec::new();
    for entry in crate::catalog::read_entries_at(home)? {
        let Some(project) = &entry.project else {
            continue;
        };
        let (org, repo) = project.split_once('/').unwrap_or(("", project));
        if !crate::legacy_archive::project_filter_matches(org, repo, repo_id) {
            continue;
        }
        if entry
            .date
            .as_deref()
            .and_then(|date| NaiveDate::parse_from_str(date, "%Y-%m-%d").ok())
            .is_some_and(|date| date < epoch)
        {
            continue;
        }
        let state = source_state(&allow, &entry.source_path);
        rows.push(SourceRow { entry, state });
    }
    // Preserve catalog order: global cap/dedup tie behavior belongs to intents.
    let key = digest(&(&policy, &rows))?;
    Ok(Snapshot { policy, key, rows })
}

fn source_allowlist(home: &Path) -> SourceAllowlist {
    SourceAllowlist::for_operator(
        &crate::os_user_home().unwrap_or_else(|| home.to_path_buf()),
        home,
    )
}

fn source_state(allow: &SourceAllowlist, source: &str) -> SourceState {
    match ready_source(allow, Path::new(source)) {
        Ok((path, fingerprint)) => SourceState::Ready { path, fingerprint },
        Err(_) if missing_allowed_source(allow, Path::new(source)) => SourceState::Missing,
        Err(error) => SourceState::Unavailable {
            reason: format!("{error:#}"),
        },
    }
}

fn ready_source(allow: &SourceAllowlist, source: &Path) -> Result<(PathBuf, SourceFingerprint)> {
    let path = allow.resolve_file(source)?;
    // Opening without reading proves current readability; stat alone cannot
    // detect ACL/permission failures and must not authorize stale claims.
    let file = allow.open_file(source)?;
    let metadata = file.metadata()?;
    let fingerprint = fingerprint(&metadata)?;
    // Non-Unix metadata APIs do not expose the same cheap inode/ctime
    // guarantee. Be conservative: hash bytes with bounded memory rather than
    // trusting a same-size edit whose mtime was restored. Still no reparse.
    #[cfg(not(unix))]
    let content_digest = {
        use std::io::Read;
        let mut file = file;
        let mut hasher = Sha256::new();
        let mut buffer = [0u8; 64 * 1024];
        loop {
            let count = file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        hasher.finalize()
    };
    let live_path = allow.resolve_file(source)?;
    if path != live_path || fingerprint != self::fingerprint(&fs::metadata(&live_path)?)? {
        bail!("overlay source changed while checking its identity");
    }
    #[cfg(not(unix))]
    let fingerprint = {
        let mut fingerprint = fingerprint;
        for bytes in content_digest.chunks_exact(8) {
            fingerprint
                .platform_identity
                .push(u64::from_le_bytes(bytes.try_into()?));
        }
        fingerprint
    };
    Ok((path, fingerprint))
}

fn fingerprint(metadata: &fs::Metadata) -> Result<SourceFingerprint> {
    if !metadata.is_file() {
        bail!("overlay source is not a regular file");
    }
    #[cfg(unix)]
    let platform_identity = {
        use std::os::unix::fs::MetadataExt;
        vec![
            metadata.dev(),
            metadata.ino(),
            metadata.ctime() as u64,
            metadata.ctime_nsec() as u64,
            metadata.mode() as u64,
            metadata.uid() as u64,
            metadata.gid() as u64,
        ]
    };
    #[cfg(windows)]
    let platform_identity = {
        use std::os::windows::fs::MetadataExt;
        vec![
            metadata.creation_time(),
            metadata.last_write_time(),
            metadata.file_attributes() as u64,
        ]
    };
    #[cfg(not(any(unix, windows)))]
    let platform_identity = Vec::new();
    Ok(SourceFingerprint {
        len: metadata.len(),
        modified_ns: metadata.modified()?.duration_since(UNIX_EPOCH)?.as_nanos(),
        created_ns: metadata
            .created()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|time| time.as_nanos()),
        platform_identity,
    })
}

/// Only stable absence under a proven approved parent is reusable. A dangling
/// symlink, permission error, or unknown outside-root path is NOT cached absence.
fn missing_allowed_source(allow: &SourceAllowlist, path: &Path) -> bool {
    if path
        .components()
        .any(|part| matches!(part, Component::ParentDir))
        || path
            .to_string_lossy()
            .split(['/', '\\'])
            .any(|part| part == "..")
    {
        return false;
    }
    if !matches!(fs::symlink_metadata(path), Err(ref error) if error.kind() == std::io::ErrorKind::NotFound)
    {
        return false;
    }
    let mut parent = path.parent();
    while let Some(candidate) = parent {
        match candidate.canonicalize() {
            Ok(canonical) => {
                return allow.roots().iter().any(|root| {
                    root.canonicalize()
                        .is_ok_and(|root| canonical.starts_with(root))
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                parent = candidate.parent()
            }
            Err(_) => return false,
        }
    }
    false
}

fn ensure_unchanged(before: &Snapshot, after: &Snapshot) -> Result<()> {
    if before.key != after.key {
        bail!(
            "overlay catalog, source, or policy changed during build; retry after the writer settles"
        );
    }
    Ok(())
}

fn ensure_cache_owner(root: &Path, home: &Path, repo_id: &str) -> Result<()> {
    let canonical_home = home
        .canonicalize()
        .with_context(|| format!("canonicalize AICX home {}", home.display()))?;
    let expected = CacheOwner {
        schema: CACHE_OWNER_SCHEMA.to_owned(),
        repo_id: repo_id.to_owned(),
        canonical_home_digest: digest(&(
            CACHE_OWNER_SCHEMA,
            canonical_home.to_string_lossy().as_ref(),
        ))?,
    };
    let marker = root.join("catalog-cache-owner-v1.json");
    if let Some(actual) = read_json::<CacheOwner>(root, &marker)? {
        if actual != expected {
            bail!(
                "overlay catalog cache owner mismatch under {}; refusing source-cache reads, writes, or cleanup",
                root.display()
            );
        }
        return Ok(());
    }
    if directory_has_source_slot_name(root)? {
        bail!(
            "overlay source cache slots under {} have no valid owner marker; refusing adoption or cleanup",
            root.display()
        );
    }
    atomic_write_json(&marker, &expected)
}

fn directory_has_source_slot_name(root: &Path) -> Result<bool> {
    for entry in super::read_dir_rebuilt_under_base(root, root)
        .with_context(|| format!("scan overlay cache ownership under {}", root.display()))?
    {
        let entry = entry?;
        if entry.file_name().to_str().is_some_and(is_source_slot_name) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Remove only obsolete private conversation slots. Public overlay revisions,
/// identity indexes, locks, unknown files, directories and symlinks are outside
/// this cleanup contract.
fn prune_obsolete_source_slots(root: &Path, snapshot: &Snapshot) -> Result<()> {
    let active = snapshot
        .rows
        .iter()
        .filter(|row| matches!(row.state, SourceState::Ready { .. }))
        .map(source_slot_name)
        .collect::<Result<std::collections::BTreeSet<_>>>()?;
    let mut errors = Vec::new();
    for entry in super::read_dir_rebuilt_under_base(root, root)
        .with_context(|| format!("scan overlay source slots under {}", root.display()))?
    {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!("read directory entry: {error}"));
                continue;
            }
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !is_source_slot_name(&name) || active.contains(&name) {
            continue;
        }
        let path = entry.path();
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
                if let Err(error) = fs::remove_file(&path) {
                    errors.push(format!("{}: {error}", path.display()));
                }
            }
            Ok(_) => {}
            Err(error) => errors.push(format!("{}: {error}", path.display())),
        }
    }
    if !errors.is_empty() {
        bail!(
            "failed to prune {} obsolete overlay source cache slot(s): {}",
            errors.len(),
            errors.join("; ")
        );
    }
    Ok(())
}

fn source_slot_name(row: &SourceRow) -> Result<String> {
    Ok(format!(
        "catalog-source-v1-{}.json",
        digest(&(
            &row.entry.agent,
            &row.entry.session_id,
            &row.entry.source_path
        ))?
    ))
}

fn is_source_slot_name(name: &str) -> bool {
    let Some(digest) = name
        .strip_prefix("catalog-source-v1-")
        .and_then(|name| name.strip_suffix(".json"))
    else {
        return false;
    };
    digest.len() == 64
        && digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn report_skip(entry: &CatalogEntry, reason: &str) {
    let message = format!(
        "intents_source_skip agent={} session_id={} path={} error={reason}",
        entry.agent, entry.session_id, entry.source_path
    );
    crate::diagnostics::log_describe(&message);
    eprintln!("overlay: {message}");
}

fn digest(value: &impl Serialize) -> Result<String> {
    Ok(format!("{:x}", Sha256::digest(serde_json::to_vec(value)?)))
}

/// New cache files are flat children of a canonical, user-selected index root.
/// Refuse symlinks (even in-root) rather than following attacker-selected data.
pub(super) fn validate_cache_path(root: &Path, path: &Path) -> Result<()> {
    let canonical_root = root.canonicalize()?;
    if path
        .parent()
        .context("cache has no parent")?
        .canonicalize()?
        != canonical_root
    {
        bail!("overlay cache path escapes index root: {}", path.display());
    }
    match fs::symlink_metadata(path) {
        Ok(metadata) if !metadata.is_file() || metadata.file_type().is_symlink() => {
            bail!(
                "overlay cache path must be a regular non-symlink file: {}",
                path.display()
            );
        }
        Ok(_) => {
            super::rebuild_under_root(&canonical_root, path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    Ok(())
}

pub(super) fn read_json<T: DeserializeOwned>(root: &Path, path: &Path) -> Result<Option<T>> {
    validate_cache_path(root, path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("read overlay cache {}", path.display()));
        }
    };
    validate_cache_path(root, path)?;
    let mut bytes = Vec::new();
    use std::io::Read;
    file.read_to_end(&mut bytes)?;
    // Invalid derived JSON is a cache miss, never a stale result or lost source.
    Ok(serde_json::from_slice(&bytes).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::intents::{IntentsConfig, extract_intents_from_root_at_with_stats};
    use crate::timeline::FrameKind;

    struct Fixture {
        temp_root: PathBuf,
        home: PathBuf,
        root: PathBuf,
        rows: Vec<CatalogEntry>,
    }

    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicUsize, Ordering};
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let temp_root = std::env::temp_dir().join(format!(
                "aicx-overlay-cache-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            let home = temp_root.join("home");
            let root = home.join("overlay-index-v1/test");
            fs::create_dir_all(&root).unwrap();
            let mut rows = Vec::new();
            for n in 0..2 {
                let source = home.join(format!("source-{n}.log"));
                fs::write(&source, format!("DECISION: preserve source {n} in src/source-{n}.rs\nWHY: keep every original evidence reference.\n")).unwrap();
                rows.push(CatalogEntry {
                    schema: crate::catalog::CATALOG_SCHEMA.to_owned(),
                    session_id: format!("session-{n}"),
                    agent: "vibecrafted".to_owned(),
                    project: Some("Loctree/aicx".to_owned()),
                    date: Some("2026-09-03".to_owned()),
                    cwd: Some(format!("/repo/aicx-{n}")),
                    source_path: source.to_string_lossy().into_owned(),
                    source_len: None,
                    source_mtime_ns: None,
                    title: None,
                    machine: None,
                    logical_session_id: None,
                });
            }
            let fixture = Self {
                temp_root,
                home,
                root,
                rows,
            };
            fixture.write_catalog();
            fixture
        }

        fn write_catalog(&self) {
            let path = crate::catalog::sessions_path_for(&self.home);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let text = self
                .rows
                .iter()
                .map(|entry| serde_json::to_string(entry).unwrap())
                .collect::<Vec<_>>()
                .join("\n");
            fs::write(path, text).unwrap();
        }

        fn load(&self, rebuild: bool) -> (Vec<OverlayFeedItem>, OverlayBuildStats) {
            load_catalog_feed(&self.home, "Loctree/aicx", &self.root, rebuild).unwrap()
        }

        fn source_slot(&self, row: &CatalogEntry) -> PathBuf {
            self.root.join(
                source_slot_name(&SourceRow {
                    entry: row.clone(),
                    state: SourceState::Missing,
                })
                .unwrap(),
            )
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.temp_root);
        }
    }

    #[test]
    fn warm_feed_skips_parsing_and_changed_source_reuses_other_session() {
        let fixture = Fixture::new();
        let (cold, stats) = fixture.load(false);
        assert!(!cold.is_empty());
        assert_eq!(stats.source_sessions_parsed, 2);
        let (warm, stats) = fixture.load(false);
        assert!(stats.feed_cache_hit);
        assert_eq!(stats.source_sessions_parsed, 0);
        assert_eq!(stats.source_sessions_reused, 2);
        assert_eq!(digest(&cold).unwrap(), digest(&warm).unwrap());
        fs::write(&fixture.rows[0].source_path, "DECISION: preserve a changed source in src/new.rs\nWHY: changed intent must be visible.\n").unwrap();
        let (changed, stats) = fixture.load(false);
        assert!(!stats.feed_cache_hit);
        assert_eq!(stats.source_sessions_parsed, 1);
        assert_eq!(stats.source_sessions_reused, 1);
        assert_ne!(digest(&cold).unwrap(), digest(&changed).unwrap());
        let (rebuilt, stats) = fixture.load(true);
        assert_eq!(stats.source_sessions_parsed, 2);
        assert!(!stats.feed_cache_hit);
        assert_eq!(digest(&changed).unwrap(), digest(&rebuilt).unwrap());
    }

    #[test]
    fn catalog_removal_prunes_only_exact_obsolete_regular_source_slots() {
        let mut fixture = Fixture::new();
        fixture.load(false);
        let removed_slot = fixture.source_slot(&fixture.rows[0]);
        let retained_slot = fixture.source_slot(&fixture.rows[1]);
        assert!(removed_slot.is_file());
        assert!(retained_slot.is_file());

        let stale_exact = fixture
            .root
            .join(format!("catalog-source-v1-{}.json", "a".repeat(64)));
        let malformed_name = fixture.root.join("catalog-source-v1-not-a-hash.json");
        let unrelated = fixture.root.join("operator-note.txt");
        let public_overlay = fixture.root.join("ov1-public-retained.json");
        for path in [&stale_exact, &malformed_name, &unrelated, &public_overlay] {
            fs::write(path, "sentinel").unwrap();
        }
        #[cfg(unix)]
        let stale_symlink = {
            let path = fixture
                .root
                .join(format!("catalog-source-v1-{}.json", "b".repeat(64)));
            std::os::unix::fs::symlink(&unrelated, &path).unwrap();
            path
        };

        fixture.rows.remove(0);
        fixture.write_catalog();
        fixture.load(false);
        assert!(!removed_slot.exists());
        assert!(retained_slot.is_file());
        assert!(!stale_exact.exists());
        assert_eq!(fs::read_to_string(malformed_name).unwrap(), "sentinel");
        assert_eq!(fs::read_to_string(unrelated).unwrap(), "sentinel");
        assert_eq!(fs::read_to_string(public_overlay).unwrap(), "sentinel");
        #[cfg(unix)]
        assert!(
            fs::symlink_metadata(stale_symlink)
                .unwrap()
                .file_type()
                .is_symlink()
        );
    }

    #[test]
    fn cache_owner_mismatch_fails_closed_before_source_slot_use() {
        let fixture = Fixture::new();
        fixture.load(false);
        let marker = fixture.root.join("catalog-cache-owner-v1.json");
        let wrong = CacheOwner {
            schema: CACHE_OWNER_SCHEMA.to_owned(),
            repo_id: "other/repo".to_owned(),
            canonical_home_digest: "wrong-home".to_owned(),
        };
        atomic_write_json(&marker, &wrong).unwrap();
        let error =
            load_catalog_feed(&fixture.home, "Loctree/aicx", &fixture.root, false).unwrap_err();
        assert!(error.to_string().contains("owner mismatch"));
        assert!(fixture.source_slot(&fixture.rows[0]).is_file());
    }

    #[test]
    fn unowned_matching_source_slot_is_not_adopted_or_deleted() {
        let fixture = Fixture::new();
        let slot = fixture
            .root
            .join(format!("catalog-source-v1-{}.json", "c".repeat(64)));
        fs::write(&slot, "unowned private cache").unwrap();
        let error =
            load_catalog_feed(&fixture.home, "Loctree/aicx", &fixture.root, false).unwrap_err();
        assert!(error.to_string().contains("no valid owner marker"));
        assert_eq!(fs::read_to_string(slot).unwrap(), "unowned private cache");
        assert!(!fixture.root.join("catalog-cache-owner-v1.json").exists());
    }

    #[test]
    fn legacy_root_without_source_slots_can_establish_owner_and_keep_outputs() {
        let fixture = Fixture::new();
        let public_overlay = fixture.root.join("ov1-existing.json");
        let identity_index = fixture.root.join("side-index.json");
        fs::write(&public_overlay, "public output").unwrap();
        fs::write(&identity_index, "identity registry").unwrap();
        let before = snapshot(&fixture.home, "Loctree/aicx").unwrap();
        ensure_cache_owner(&fixture.root, &fixture.home, "Loctree/aicx").unwrap();
        prune_obsolete_source_slots(&fixture.root, &before).unwrap();
        assert_eq!(fs::read_to_string(public_overlay).unwrap(), "public output");
        assert_eq!(
            fs::read_to_string(identity_index).unwrap(),
            "identity registry"
        );
        assert!(fixture.root.join("catalog-cache-owner-v1.json").is_file());
    }

    #[test]
    fn missing_source_prunes_its_cleaned_conversation_slot() {
        let fixture = Fixture::new();
        fixture.load(false);
        let source = PathBuf::from(&fixture.rows[0].source_path);
        let obsolete_slot = fixture.source_slot(&fixture.rows[0]);
        assert!(obsolete_slot.is_file());
        fs::remove_file(&source).unwrap();
        fixture.load(false);
        assert!(!obsolete_slot.exists());
        assert!(
            !source.exists(),
            "cleanup must not recreate or touch the source"
        );
    }

    #[test]
    fn source_path_replacement_prunes_old_slot_and_keeps_new_slot() {
        let mut fixture = Fixture::new();
        fixture.load(false);
        let old_slot = fixture.source_slot(&fixture.rows[0]);
        let old_source = PathBuf::from(&fixture.rows[0].source_path);
        let new_source = fixture.home.join("replacement-source.log");
        fs::copy(&old_source, &new_source).unwrap();
        fixture.rows[0].source_path = new_source.to_string_lossy().into_owned();
        let new_slot = fixture.source_slot(&fixture.rows[0]);
        fixture.write_catalog();
        fixture.load(false);
        assert!(!old_slot.exists());
        assert!(new_slot.is_file());
        assert!(
            old_source.is_file(),
            "cleanup must never delete user sources"
        );
        assert!(new_source.is_file());
    }

    #[test]
    fn cached_feed_matches_original_full_history_lane_extraction() {
        let mut fixture = Fixture::new();
        for (number, row) in fixture.rows.iter_mut().enumerate() {
            row.agent = "codex".to_owned();
            let messages = [
                serde_json::json!({"timestamp":"2026-09-03T10:00:00Z", "type":"session_meta",
                    "payload":{"id":row.session_id,"cwd":row.cwd,"source":"cli"}}),
                serde_json::json!({"timestamp":"2026-09-03T10:00:01Z", "type":"response_item",
                    "payload":{"type":"message","role":"user","content":[{"type":"input_text",
                    "text":format!("DECISION: preserve original evidence in src/source-{number}.rs. WHY: full history must remain recoverable.")}]}}),
                serde_json::json!({"timestamp":"2026-09-03T10:00:02Z", "type":"response_item",
                    "payload":{"type":"message","role":"assistant","content":[{"type":"output_text",
                    "text":format!("DECISION: retain durable source identity in src/agent-source-{number}.rs. WHY: unchanged evidence must keep stable references.")}]}}),
            ];
            fs::write(
                &row.source_path,
                messages
                    .iter()
                    .map(|value| serde_json::to_string(value).unwrap())
                    .collect::<Vec<_>>()
                    .join("\n")
                    + "\n",
            )
            .unwrap();
        }
        fixture.write_catalog();
        let mut records = Vec::new();
        for frame_kind in [FrameKind::UserMsg, FrameKind::AgentReply] {
            let config = IntentsConfig {
                project: "Loctree/aicx".to_owned(),
                hours: 0,
                strict: false,
                min_confidence: None,
                kind_filter: None,
                frame_kind: Some(frame_kind),
                live: false,
            };
            let lane =
                extract_intents_from_root_at_with_stats(&config, &fixture.home, chrono::Utc::now())
                    .unwrap()
                    .records;
            assert!(
                !lane.is_empty(),
                "equivalence requires nonempty {frame_kind:?} lane"
            );
            records.extend(lane);
        }
        let original = deduplicated_catalog_feed(records, "Loctree/aicx");
        let (actual, _) = fixture.load(false);
        assert_eq!(digest(&original).unwrap(), digest(&actual).unwrap());
    }

    #[test]
    fn missing_source_drops_claims_and_reappearance_invalidates_warm_feed() {
        let fixture = Fixture::new();
        fixture.load(false);
        let source = &fixture.rows[0].source_path;
        let body = fs::read(source).unwrap();
        fs::remove_file(source).unwrap();
        let (missing, stats) = fixture.load(false);
        assert!(!stats.feed_cache_hit);
        assert_eq!(stats.source_sessions_parsed, 0);
        assert!(missing.iter().all(|item| item.session_id != "session-0"));
        assert!(fixture.load(false).1.feed_cache_hit);
        fs::write(source, body).unwrap();
        let (restored, stats) = fixture.load(false);
        assert!(!stats.feed_cache_hit);
        assert_eq!(stats.source_sessions_parsed, 1);
        assert!(restored.iter().any(|item| item.session_id == "session-0"));
    }

    #[test]
    fn policy_and_catalog_metadata_invalidate_derived_claims() {
        let mut fixture = Fixture::new();
        fixture.load(false);
        fixture.rows[0].logical_session_id = Some("new-conversation".to_owned());
        fixture.write_catalog();
        let (_, stats) = fixture.load(false);
        assert_eq!(stats.source_sessions_parsed, 1);
        assert_eq!(stats.source_sessions_reused, 1);
        fs::write(fixture.home.join(".aicxignore"), "/repo/aicx-0\n").unwrap();
        let (filtered, stats) = fixture.load(false);
        assert!(!stats.feed_cache_hit);
        assert_eq!(stats.source_sessions_parsed, 2);
        assert!(filtered.iter().all(|item| item.session_id != "session-0"));
        assert!(fixture.load(false).1.feed_cache_hit);
    }

    #[test]
    fn malformed_or_checksum_mismatched_caches_are_rebuilt() {
        let fixture = Fixture::new();
        let (expected, _) = fixture.load(false);
        fs::write(fixture.root.join("catalog-feed-v1.json"), "{broken").unwrap();
        let row = &fixture.rows[0];
        let slot = digest(&(&row.agent, &row.session_id, &row.source_path)).unwrap();
        let source_cache = fixture.root.join(format!("catalog-source-v1-{slot}.json"));
        let mut saved: ConversationCache =
            read_json(&fixture.root, &source_cache).unwrap().unwrap();
        saved.frames[0].message = "DECISION: inject an untrusted cached claim".to_owned();
        atomic_write_json(&source_cache, &saved).unwrap();
        let (rebuilt, stats) = fixture.load(false);
        assert_eq!(stats.source_sessions_parsed, 1);
        assert_eq!(stats.source_sessions_reused, 1);
        assert_eq!(digest(&expected).unwrap(), digest(&rebuilt).unwrap());
    }

    #[test]
    fn mutation_between_snapshots_is_not_publishable() {
        let fixture = Fixture::new();
        let before = snapshot(&fixture.home, "Loctree/aicx").unwrap();
        fs::write(&fixture.rows[0].source_path, "new source body").unwrap();
        let after = snapshot(&fixture.home, "Loctree/aicx").unwrap();
        assert!(ensure_unchanged(&before, &after).is_err());
    }

    #[test]
    fn catalog_refresh_hides_absent_claims_and_retains_reappearing_identity() {
        let fixture = Fixture::new();
        let (mut feed, _) = fixture.load(false);
        let (initial, _, _) =
            super::super::update_side_index(None, &feed, "Loctree/aicx", "rev1", "test", true)
                .unwrap();
        feed.retain(|item| item.session_id == "session-1");
        assert!(!feed.is_empty());
        for item in &mut feed {
            item.valid_from = "2026-09-04T00:00:00Z".to_owned();
        }
        let (updated, added, _) = super::super::update_side_index(
            Some(initial.clone()),
            &feed,
            "Loctree/aicx",
            "rev2",
            "test",
            true,
        )
        .unwrap();
        assert_eq!(added, 0);
        assert!(!updated.entries.is_empty());
        let active = super::super::active_side_index_entries(
            &updated,
            &feed,
            super::super::OverlayFeedSource::Catalog,
            false,
        );
        for entry in active {
            assert_eq!(entry.session_id, "session-1");
            assert_eq!(entry.valid_from, "2026-09-04T00:00:00Z");
            assert!(
                initial
                    .entries
                    .iter()
                    .any(|old| old.intent_id == entry.intent_id)
            );
        }
        let (restored_feed, _) = fixture.load(false);
        let (restored, added, _) = super::super::update_side_index(
            Some(updated),
            &restored_feed,
            "Loctree/aicx",
            "rev1",
            "test",
            true,
        )
        .unwrap();
        assert_eq!(added, 0, "reappearing claims must reactivate existing IDs");
        let original_ids: Vec<_> = initial
            .entries
            .iter()
            .map(|entry| entry.intent_id.clone())
            .collect();
        let restored_ids: Vec<_> = super::super::active_side_index_entries(
            &restored,
            &restored_feed,
            super::super::OverlayFeedSource::Catalog,
            false,
        )
        .iter()
        .map(|entry| entry.intent_id.clone())
        .collect();
        assert_eq!(original_ids, restored_ids);
    }

    #[test]
    fn output_revision_changes_for_temporal_authority_and_turn_metadata() {
        let fixture = Fixture::new();
        let (feed, _) = fixture.load(false);
        let baseline = super::super::catalog_feed_revision(&feed);
        for field in ["valid_from", "authority", "turn_idx"] {
            let mut changed = feed.clone();
            match field {
                "valid_from" => changed[0].valid_from = "2026-09-04T00:00:00Z".to_owned(),
                "authority" => changed[0].authority = "different_authority".to_owned(),
                _ => changed[0].turn_idx += 1,
            }
            assert_eq!(changed[0].evidence_event_id, feed[0].evidence_event_id);
            assert_ne!(
                baseline,
                super::super::catalog_feed_revision(&changed),
                "field={field}"
            );
        }
    }

    #[test]
    fn residual_fallback_retires_catalog_claims_without_erasing_residual_history() {
        let fixture = Fixture::new();
        let (mut feed, _) = fixture.load(false);
        let mut old_c6 = feed[0].clone();
        old_c6.evidence_event_id = "ev1:old-c6".to_owned();
        feed.push(old_c6.clone());
        let (initial, _, _) =
            super::super::update_side_index(None, &feed, "Loctree/aicx", "before", "test", true)
                .unwrap();
        let side_path = fixture.root.join("side-index.json");
        atomic_write_json(&side_path, &initial).unwrap();
        let previous = super::super::read_side_index(&side_path, "Loctree/aicx").unwrap();
        let mut new_c6 = old_c6;
        new_c6.evidence_event_id = "ev1:new-c6".to_owned();
        let (updated, _, _) = super::super::update_side_index(
            previous,
            std::slice::from_ref(&new_c6),
            "Loctree/aicx",
            "after",
            "test",
            false,
        )
        .unwrap();
        let active = super::super::active_side_index_entries(
            &updated,
            std::slice::from_ref(&new_c6),
            super::super::OverlayFeedSource::ResidualC6,
            false,
        );
        assert!(
            active
                .iter()
                .all(|entry| !entry.evidence_event_id.starts_with("intent1:"))
        );
        assert!(
            active
                .iter()
                .any(|entry| entry.evidence_event_id == "ev1:old-c6")
        );
        assert!(
            active
                .iter()
                .any(|entry| entry.evidence_event_id == "ev1:new-c6")
        );
        let mut rebuilt = updated;
        super::super::retire_absent_residual_claims(&mut rebuilt, std::slice::from_ref(&new_c6));
        assert!(
            rebuilt
                .entries
                .iter()
                .any(|entry| entry.evidence_event_id.starts_with("intent1:")),
            "catalog identity registry must survive residual rebuild"
        );
        let subsequent = super::super::active_side_index_entries(
            &rebuilt,
            std::slice::from_ref(&new_c6),
            super::super::OverlayFeedSource::ResidualC6,
            false,
        );
        assert!(
            subsequent
                .iter()
                .all(|entry| entry.evidence_event_id == "ev1:new-c6"),
            "retired C6 claims must not reappear on a later normal build"
        );
    }

    #[test]
    fn parser_errors_never_publish_a_trusted_feed() {
        let mut fixture = Fixture::new();
        fixture.rows[0].agent = "unsupported-agent".to_owned();
        fixture.write_catalog();
        let (partial, stats) = fixture.load(false);
        assert!(!stats.feed_cache_hit);
        assert!(partial.iter().all(|item| item.session_id != "session-0"));
        assert!(!fixture.root.join("catalog-feed-v1.json").exists());
        let (_, stats) = fixture.load(false);
        assert_eq!(stats.source_sessions_parsed, 1);
        assert_eq!(stats.source_sessions_reused, 1);
        assert!(!stats.feed_cache_hit);
    }

    #[test]
    fn large_settled_projection_is_reused_but_unfinished_tail_is_not() {
        use std::io::{Seek, SeekFrom, Write};
        let mut fixture = Fixture::new();
        fixture.rows.truncate(1);
        fixture.rows[0].agent = "codex".to_owned();
        let path = Path::new(&fixture.rows[0].source_path);
        let mut source = fs::File::create(path).unwrap();
        // A sparse oversized record exercises the real >64MiB dispatch without
        // allocating a giant payload. The bounded reader reports it explicitly.
        source.set_len(65 * 1024 * 1024).unwrap();
        source.seek(SeekFrom::End(0)).unwrap();
        source.write_all(b"\n").unwrap();
        writeln!(source, "{}", serde_json::json!({
            "timestamp": "2026-09-03T12:00:00Z", "type": "response_item",
            "payload": {"type":"message", "role":"user", "content":[
                {"type":"input_text", "text":"DECISION: retain the large-session intent in src/main.rs"}
            ]}
        })).unwrap();
        source.flush().unwrap();
        drop(source);
        fixture.write_catalog();
        let (cold, stats) = fixture.load(false);
        assert!(!cold.is_empty());
        assert_eq!(stats.source_sessions_parsed, 1);
        let (_, warm) = fixture.load(false);
        assert!(warm.feed_cache_hit);
        assert_eq!(warm.source_sessions_parsed, 0);
        let saved: FeedCache = read_json(&fixture.root, &fixture.root.join("catalog-feed-v1.json"))
            .unwrap()
            .unwrap();
        assert_eq!(saved.coverage_notes.len(), 1);
        assert!(saved.coverage_notes[0].contains("skipped_oversized_records=1"));
        assert!(saved.coverage_notes[0].contains("not CompleteVisible"));

        let mut source = fs::OpenOptions::new().append(true).open(path).unwrap();
        source.write_all(b"{\"timestamp\":").unwrap();
        source.flush().unwrap();
        assert!(!fixture.load(false).1.feed_cache_hit);
        let (_, repeated_partial) = fixture.load(false);
        assert!(!repeated_partial.feed_cache_hit);
        assert_eq!(repeated_partial.source_sessions_parsed, 1);

        source
            .write_all(b"\"2026-09-03T12:00:01Z\",\"type\":\"session_meta\"}\n")
            .unwrap();
        source.flush().unwrap();
        let (repaired, stats) = fixture.load(false);
        assert_eq!(stats.source_sessions_parsed, 1);
        assert_eq!(digest(&cold).unwrap(), digest(&repaired).unwrap());
        assert!(fixture.load(false).1.feed_cache_hit);
    }

    #[test]
    fn inference_from_current_time_cannot_become_trusted_bounded_cache() {
        use std::io::{Seek, SeekFrom, Write};
        let mut fixture = Fixture::new();
        fixture.rows.truncate(1);
        fixture.rows[0].agent = "codex".to_owned();
        let mut source = fs::File::create(&fixture.rows[0].source_path).unwrap();
        source.set_len(65 * 1024 * 1024).unwrap();
        source.seek(SeekFrom::End(0)).unwrap();
        writeln!(source, "\n{}", serde_json::json!({
            "type":"response_item", "payload":{"type":"message", "role":"user",
            "content":[{"type":"input_text", "text":"DECISION: retain evidence in src/main.rs"}]}
        })).unwrap();
        source.flush().unwrap();
        fixture.write_catalog();
        assert!(!fixture.load(false).0.is_empty());
        assert!(!fixture.root.join("catalog-feed-v1.json").exists());
        assert!(!fixture.load(false).1.feed_cache_hit);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_cache_is_refused_without_reading_or_overwriting_target() {
        let fixture = Fixture::new();
        let outside = fixture.temp_root.join("outside.json");
        fs::write(&outside, "private").unwrap();
        std::os::unix::fs::symlink(&outside, fixture.root.join("catalog-feed-v1.json")).unwrap();
        assert!(load_catalog_feed(&fixture.home, "Loctree/aicx", &fixture.root, false).is_err());
        assert_eq!(fs::read_to_string(outside).unwrap(), "private");
    }
}
