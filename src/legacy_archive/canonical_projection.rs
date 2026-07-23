use aicx_parser::projections::{CANONICAL_CARD_SCHEMA, CanonicalCard, CanonicalProjection};
#[cfg(test)]
use anyhow::Context;
use anyhow::{Result, anyhow};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

pub const CANONICAL_PROJECTION_DIRNAME: &str = "canonical-projection-v1";

/// Lease schema for in-flight projection stages. The lease is written into
/// the stage directory BEFORE any payload so an interrupted stage is always
/// classifiable: a stage without a readable lease is `Corrupt` by definition,
/// never silently ambiguous.
pub const PROJECTION_STAGE_SCHEMA: &str = "aicx.store.projection_stage.v1";
pub const PROJECTION_STAGE_META_FILENAME: &str = "stage.json";
/// A verified-alive owner whose heartbeat is older than this is `Stale`
/// (wedged, but alive — doctor never force-kills, it only reports).
pub const PROJECTION_STAGE_STALE_AFTER_SECS: i64 = 900;
/// Sentinel recorded when the writer cannot compute its own process start
/// identity. Probes treat it as unprovable ownership (fail closed to
/// `UnknownOwner`), never as a match.
pub const PROJECTION_STAGE_IDENTITY_UNVERIFIABLE: &str = "unverifiable";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectionStageLease {
    pub schema: String,
    pub pid: u32,
    /// Platform-specific process start identity (Linux: /proc starttime
    /// ticks; macOS: `ps -o lstart=` string). PID alone is never trusted:
    /// a recycled PID with a different start identity is proof the original
    /// owner is gone, not evidence of ownership.
    pub process_start_identity: String,
    pub created_at: String,
    pub heartbeat_at: String,
    /// Content identity of the staged projection (schema-independent hash
    /// over revision, extraction schema, producer, and card ids). This is
    /// the recovery reference for a quarantined stage.
    pub source_hash: String,
    /// `store_revision` of the committed target at stage creation, or
    /// "none". If the committed target moves past this, the stage was built
    /// against an outdated world and fails closed as `SourceDrift`.
    pub target_generation: String,
    /// "staging" until the payload + manifest are fully written, then
    /// "complete" immediately before promotion.
    pub state: String,
}

pub const PROJECTION_STAGE_STATE_STAGING: &str = "staging";
pub const PROJECTION_STAGE_STATE_COMPLETE: &str = "complete";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageOwner {
    /// PID alive and start identity matches the lease.
    VerifiedAlive,
    /// PID no longer exists.
    Dead,
    /// PID alive but start identity differs — the PID was recycled; the
    /// original owner is gone.
    Reused,
    /// Liveness or identity cannot be determined on this platform/state.
    Unverifiable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageClass {
    Live,
    Stale,
    DeadOwner,
    SourceDrift,
    CompleteUnpromoted,
    UnknownOwner,
    Corrupt,
}

impl StageClass {
    pub fn as_str(self) -> &'static str {
        match self {
            StageClass::Live => "live",
            StageClass::Stale => "stale",
            StageClass::DeadOwner => "dead-owner",
            StageClass::SourceDrift => "source-drift",
            StageClass::CompleteUnpromoted => "complete-unpromoted",
            StageClass::UnknownOwner => "unknown-owner",
            StageClass::Corrupt => "corrupt",
        }
    }

    /// Default remediation is a recoverable quarantine move — never delete.
    /// Live/stale owners keep their stage; unproven ownership stays in
    /// place and is only reported (the operator owns the final decision).
    pub fn quarantine_eligible(self) -> bool {
        matches!(
            self,
            StageClass::DeadOwner
                | StageClass::SourceDrift
                | StageClass::CompleteUnpromoted
                | StageClass::Corrupt
        )
    }
}

#[derive(Debug)]
pub struct StageInventoryEntry {
    pub path: PathBuf,
    pub class: StageClass,
    pub lease: Option<ProjectionStageLease>,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalStoreManifest {
    pub schema: String,
    pub card_schema: String,
    pub store_revision: String,
    pub extraction_schema: String,
    pub producer_version: String,
    pub card_ids: Vec<String>,
}

/// Retired (extracts-store cut). Catalog + extract replace the card mill.
/// Callers that still need a fixture projection for overlay tests write
/// `manifest.json` + `cards/` directly under `canonical-projection-v1`.
pub fn write_canonical_projection_at(
    _root: &Path,
    _projection: &CanonicalProjection,
) -> Result<PathBuf> {
    Err(anyhow!(
        "canonical projection write is retired; use catalog rebuild + extract, not store cards"
    ))
}

pub fn read_canonical_projection_at(
    root: &Path,
) -> Result<Option<(CanonicalStoreManifest, Vec<CanonicalCard>)>> {
    let target = root.join(CANONICAL_PROJECTION_DIRNAME);
    if !target.exists() {
        return Ok(None);
    }
    let manifest_path = crate::sanitize::validate_read_path(&target.join("manifest.json"))?;
    let manifest: CanonicalStoreManifest =
        serde_json::from_str(&crate::sanitize::read_to_string_validated(&manifest_path)?)?;
    if manifest.schema != "aicx.store.manifest.v1" || manifest.card_schema != CANONICAL_CARD_SCHEMA
    {
        return Err(anyhow!("unsupported canonical store schema"));
    }
    let mut cards = Vec::with_capacity(manifest.card_ids.len());
    for id in &manifest.card_ids {
        let card_path =
            crate::sanitize::validate_read_path(&target.join("cards").join(card_filename(id)?))?;
        cards.push(serde_json::from_str(
            &crate::sanitize::read_to_string_validated(&card_path)?,
        )?);
    }
    Ok(Some((manifest, cards)))
}

#[cfg(test)]
fn write_stage_lease(stage: &Path, lease: &ProjectionStageLease) -> Result<()> {
    super::atomic_write::atomic_write(
        &stage.join(PROJECTION_STAGE_META_FILENAME),
        &serde_json::to_vec_pretty(lease)?,
    )
    .context("write projection stage lease")
}

pub fn read_stage_lease(stage: &Path) -> Result<ProjectionStageLease> {
    let path = crate::sanitize::validate_read_path(&stage.join(PROJECTION_STAGE_META_FILENAME))?;
    let lease: ProjectionStageLease =
        serde_json::from_str(&crate::sanitize::read_to_string_validated(&path)?)?;
    Ok(lease)
}

/// Content identity of a projection: what a stage was going to commit.
/// Recorded in the lease so a quarantined stage carries a verifiable
/// recovery reference independent of filesystem layout.
pub fn projection_source_hash(projection: &CanonicalProjection) -> String {
    let mut hasher = Sha256::new();
    hasher.update(projection.store_revision.as_bytes());
    hasher.update([0]);
    hasher.update(projection.extraction_schema.as_bytes());
    hasher.update([0]);
    hasher.update(projection.producer_version.as_bytes());
    for card in &projection.cards {
        hasher.update([0]);
        hasher.update(card.id.as_bytes());
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// `store_revision` of the committed canonical projection, without loading
/// card payloads (fast-inventory requirement).
fn read_target_generation(root: &Path) -> Option<String> {
    let manifest_path = root
        .join(CANONICAL_PROJECTION_DIRNAME)
        .join("manifest.json");
    if !manifest_path.exists() {
        return None;
    }
    let manifest_path = crate::sanitize::validate_read_path(&manifest_path).ok()?;
    let raw = crate::sanitize::read_to_string_validated(&manifest_path).ok()?;
    serde_json::from_str::<CanonicalStoreManifest>(&raw)
        .ok()
        .map(|manifest| manifest.store_revision)
}

/// Enumerate and classify every in-flight `.canonical-projection-v1.stage-*`
/// directory under `root`. Reads only lease metadata (never payload), so the
/// inventory stays fast regardless of stage size.
pub fn inspect_projection_stages_at(root: &Path) -> Vec<StageInventoryEntry> {
    let Ok(entries) = fs::read_dir(root) else {
        return Vec::new();
    };
    let prefix = format!(".{CANONICAL_PROJECTION_DIRNAME}.stage-");
    let current_generation = read_target_generation(root);
    let now = Utc::now();
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with(&prefix) || !path.is_dir() {
            continue;
        }
        let lease = read_stage_lease(&path).ok();
        let owner = lease
            .as_ref()
            .map(probe_stage_owner)
            .unwrap_or(StageOwner::Unverifiable);
        let (class, reason) =
            classify_stage(lease.as_ref(), owner, now, current_generation.as_deref());
        out.push(StageInventoryEntry {
            path,
            class,
            lease,
            reason,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Pure classification: lease + owner verdict + clock + current target
/// generation → class. Fail-closed by construction: only a verified-alive
/// owner ever yields `Live`/`Stale`; everything unprovable lands in
/// `UnknownOwner` (left in place) or a quarantine-eligible class.
pub fn classify_stage(
    lease: Option<&ProjectionStageLease>,
    owner: StageOwner,
    now: DateTime<Utc>,
    current_target_generation: Option<&str>,
) -> (StageClass, String) {
    let Some(lease) = lease else {
        return (
            StageClass::Corrupt,
            "stage lease missing or unreadable".to_owned(),
        );
    };
    if lease.schema != PROJECTION_STAGE_SCHEMA {
        return (
            StageClass::Corrupt,
            format!("unsupported stage lease schema {:?}", lease.schema),
        );
    }
    match owner {
        StageOwner::VerifiedAlive => {
            let Ok(heartbeat) = DateTime::parse_from_rfc3339(&lease.heartbeat_at) else {
                return (
                    StageClass::Corrupt,
                    format!("unparseable heartbeat timestamp {:?}", lease.heartbeat_at),
                );
            };
            let age = now
                .signed_duration_since(heartbeat.with_timezone(&Utc))
                .num_seconds();
            if age <= PROJECTION_STAGE_STALE_AFTER_SECS {
                (
                    StageClass::Live,
                    format!("owner pid {} verified alive; heartbeat fresh", lease.pid),
                )
            } else {
                (
                    StageClass::Stale,
                    format!(
                        "owner pid {} verified alive but heartbeat is {age}s old",
                        lease.pid
                    ),
                )
            }
        }
        StageOwner::Unverifiable => (
            StageClass::UnknownOwner,
            format!(
                "ownership of pid {} cannot be proven; leaving stage in place",
                lease.pid
            ),
        ),
        StageOwner::Dead | StageOwner::Reused => {
            let owner_reason = if owner == StageOwner::Dead {
                format!("owner pid {} is gone", lease.pid)
            } else {
                format!(
                    "pid {} is alive but its start identity differs (PID reuse); original owner is gone",
                    lease.pid
                )
            };
            if let Some(current) = current_target_generation
                && lease.target_generation != current
            {
                return (
                    StageClass::SourceDrift,
                    format!(
                        "{owner_reason}; staged against target generation {:?} but current is {:?}",
                        lease.target_generation, current
                    ),
                );
            }
            if lease.state == PROJECTION_STAGE_STATE_COMPLETE {
                (
                    StageClass::CompleteUnpromoted,
                    format!("{owner_reason}; staging completed but promotion never happened"),
                )
            } else {
                (StageClass::DeadOwner, owner_reason)
            }
        }
    }
}

/// Probe the owner recorded in a lease against the live system. PID alone
/// is never sufficient: without a matching process start identity the stage
/// is not considered owned.
pub fn probe_stage_owner(lease: &ProjectionStageLease) -> StageOwner {
    let Ok(pid) = i32::try_from(lease.pid) else {
        return StageOwner::Unverifiable;
    };
    if pid <= 0 {
        return StageOwner::Unverifiable;
    }
    match process_alive(pid) {
        Some(false) => StageOwner::Dead,
        None => StageOwner::Unverifiable,
        Some(true) => {
            if lease.process_start_identity.is_empty()
                || lease.process_start_identity == PROJECTION_STAGE_IDENTITY_UNVERIFIABLE
            {
                return StageOwner::Unverifiable;
            }
            match process_start_identity(lease.pid) {
                None => StageOwner::Unverifiable,
                Some(current) if current == lease.process_start_identity => {
                    StageOwner::VerifiedAlive
                }
                Some(_) => StageOwner::Reused,
            }
        }
    }
}

#[cfg(unix)]
fn process_alive(pid: i32) -> Option<bool> {
    // Signal 0: existence probe only, nothing is delivered.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return Some(true);
    }
    match std::io::Error::last_os_error().raw_os_error() {
        Some(code) if code == libc::ESRCH => Some(false),
        Some(code) if code == libc::EPERM => Some(true),
        _ => None,
    }
}

#[cfg(not(unix))]
fn process_alive(_pid: i32) -> Option<bool> {
    None
}

#[cfg(test)]
fn current_process_start_identity() -> Option<String> {
    process_start_identity(std::process::id())
}

/// Platform process start identity: stable for the lifetime of a process,
/// different for any later process that recycles the same PID.
#[cfg(target_os = "linux")]
pub fn process_start_identity(pid: u32) -> Option<String> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // Field 22 (starttime) counted from field 3, which begins after the
    // parenthesized comm — comm itself may contain spaces.
    let rest = stat.rsplit_once(')')?.1;
    let starttime = rest.split_whitespace().nth(19)?;
    Some(format!("linux-starttime:{starttime}"))
}

#[cfg(target_os = "macos")]
pub fn process_start_identity(pid: u32) -> Option<String> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let lstart = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if lstart.is_empty() {
        return None;
    }
    Some(format!("ps-lstart:{lstart}"))
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub fn process_start_identity(_pid: u32) -> Option<String> {
    None
}

fn valid_card_id(id: &str) -> bool {
    id.strip_prefix("card3:")
        .is_some_and(|hash| hash.len() == 24 && hash.chars().all(|ch| ch.is_ascii_hexdigit()))
}

fn card_filename(id: &str) -> Result<String> {
    if !valid_card_id(id) {
        return Err(anyhow!("invalid canonical card id {id:?}"));
    }
    Ok(format!("{}.json", id.replace(':', "_")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn write_canonical_projection_is_permanently_retired() {
        let root = std::env::temp_dir().join(format!("aicx-c6-{}", std::process::id()));
        let target = root.join(CANONICAL_PROJECTION_DIRNAME);
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("sentinel"), b"old").unwrap();
        let projection = CanonicalProjection {
            schema: "aicx.store.canonical_projection.v1".to_owned(),
            extraction_schema: "x".to_owned(),
            producer_version: "y".to_owned(),
            store_revision: "z".to_owned(),
            cards: Vec::new(),
        };
        let err = write_canonical_projection_at(&root, &projection).unwrap_err();
        assert!(
            err.to_string().contains("retired"),
            "write must fail closed: {err}"
        );
        assert_eq!(fs::read(target.join("sentinel")).unwrap(), b"old");
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn absent_projection_preserves_legacy_readability() {
        let root = std::env::temp_dir().join(format!("aicx-c6-legacy-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("legacy.md"), b"legacy card").unwrap();
        assert!(read_canonical_projection_at(&root).unwrap().is_none());
        assert_eq!(fs::read(root.join("legacy.md")).unwrap(), b"legacy card");
        let _ = fs::remove_dir_all(root);
    }

    /// Manual fixture write — the only way residual readers are fed after
    /// the projection mill write path was deleted.
    fn materialize_projection_fixture(root: &Path, projection: &CanonicalProjection) {
        let target = root.join(CANONICAL_PROJECTION_DIRNAME);
        let cards_dir = target.join("cards");
        fs::create_dir_all(&cards_dir).unwrap();
        let mut card_ids = Vec::new();
        for card in &projection.cards {
            let name = card_filename(&card.id).unwrap();
            fs::write(
                cards_dir.join(&name),
                serde_json::to_vec_pretty(card).unwrap(),
            )
            .unwrap();
            card_ids.push(card.id.clone());
        }
        let manifest = CanonicalStoreManifest {
            schema: "aicx.store.manifest.v1".to_owned(),
            card_schema: CANONICAL_CARD_SCHEMA.to_owned(),
            store_revision: projection.store_revision.clone(),
            extraction_schema: projection.extraction_schema.clone(),
            producer_version: projection.producer_version.clone(),
            card_ids,
        };
        fs::write(
            target.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
    }

    fn lease_fixture(
        pid: u32,
        identity: &str,
        state: &str,
        target_generation: &str,
    ) -> ProjectionStageLease {
        let now = Utc::now().to_rfc3339();
        ProjectionStageLease {
            schema: PROJECTION_STAGE_SCHEMA.to_owned(),
            pid,
            process_start_identity: identity.to_owned(),
            created_at: now.clone(),
            heartbeat_at: now,
            source_hash: "sha256:test".to_owned(),
            target_generation: target_generation.to_owned(),
            state: state.to_owned(),
        }
    }

    /// A PID far above any real pid space on Linux (4194304) and macOS
    /// (99998), still within i32 for the kill(2) probe.
    const DEAD_PID: u32 = 500_000_000;

    #[cfg(unix)]
    #[test]
    fn forged_live_pid_with_wrong_start_identity_is_not_owned() {
        // Falsification: a stage lease carrying OUR live pid but a forged
        // start identity must never be treated as owned. PID reuse fails
        // closed.
        let lease = lease_fixture(
            std::process::id(),
            "forged:not-the-real-identity",
            PROJECTION_STAGE_STATE_STAGING,
            "none",
        );
        let owner = probe_stage_owner(&lease);
        assert_ne!(
            owner,
            StageOwner::VerifiedAlive,
            "forged identity must not verify ownership"
        );
        let (class, reason) = classify_stage(Some(&lease), owner, Utc::now(), None);
        assert!(
            !matches!(class, StageClass::Live | StageClass::Stale),
            "forged identity classified as owned: {class:?} ({reason})"
        );
    }

    #[cfg(unix)]
    #[test]
    fn matching_start_identity_verifies_live_owner() {
        let Some(identity) = current_process_start_identity() else {
            // Identity probe unavailable on this host — ownership must then
            // be unprovable, never verified.
            let lease = lease_fixture(
                std::process::id(),
                PROJECTION_STAGE_IDENTITY_UNVERIFIABLE,
                PROJECTION_STAGE_STATE_STAGING,
                "none",
            );
            assert_eq!(probe_stage_owner(&lease), StageOwner::Unverifiable);
            return;
        };
        let lease = lease_fixture(
            std::process::id(),
            &identity,
            PROJECTION_STAGE_STATE_STAGING,
            "none",
        );
        assert_eq!(probe_stage_owner(&lease), StageOwner::VerifiedAlive);
        let (class, _) = classify_stage(Some(&lease), StageOwner::VerifiedAlive, Utc::now(), None);
        assert_eq!(class, StageClass::Live);
    }

    #[cfg(unix)]
    #[test]
    fn dead_pid_probes_as_dead_owner() {
        let lease = lease_fixture(DEAD_PID, "whatever", PROJECTION_STAGE_STATE_STAGING, "none");
        assert_eq!(probe_stage_owner(&lease), StageOwner::Dead);
    }

    #[test]
    fn source_drift_fails_closed_for_dead_owner() {
        let lease = lease_fixture(
            DEAD_PID,
            "x",
            PROJECTION_STAGE_STATE_COMPLETE,
            "sr1:old-generation",
        );
        let (class, reason) = classify_stage(
            Some(&lease),
            StageOwner::Dead,
            Utc::now(),
            Some("sr1:new-generation"),
        );
        assert_eq!(class, StageClass::SourceDrift, "{reason}");
        assert!(class.quarantine_eligible());
    }

    #[test]
    fn complete_unpromoted_detected_when_generations_match() {
        let lease = lease_fixture(DEAD_PID, "x", PROJECTION_STAGE_STATE_COMPLETE, "sr1:gen");
        let (class, _) =
            classify_stage(Some(&lease), StageOwner::Dead, Utc::now(), Some("sr1:gen"));
        assert_eq!(class, StageClass::CompleteUnpromoted);
        assert!(class.quarantine_eligible());
    }

    #[test]
    fn stale_heartbeat_with_verified_owner_is_stale_not_quarantined() {
        let mut lease = lease_fixture(
            std::process::id(),
            "x",
            PROJECTION_STAGE_STATE_STAGING,
            "none",
        );
        lease.heartbeat_at = (Utc::now()
            - chrono::Duration::seconds(PROJECTION_STAGE_STALE_AFTER_SECS + 60))
        .to_rfc3339();
        let (class, _) = classify_stage(Some(&lease), StageOwner::VerifiedAlive, Utc::now(), None);
        assert_eq!(class, StageClass::Stale);
        assert!(
            !class.quarantine_eligible(),
            "no force-kill, no quarantine of a live owner"
        );
    }

    #[test]
    fn unverifiable_owner_is_unknown_and_left_in_place() {
        let lease = lease_fixture(
            std::process::id(),
            PROJECTION_STAGE_IDENTITY_UNVERIFIABLE,
            PROJECTION_STAGE_STATE_STAGING,
            "none",
        );
        let (class, reason) =
            classify_stage(Some(&lease), StageOwner::Unverifiable, Utc::now(), None);
        assert_eq!(class, StageClass::UnknownOwner);
        assert!(!class.quarantine_eligible());
        assert!(reason.contains("cannot be proven"), "{reason}");
    }

    #[test]
    fn missing_lease_classifies_corrupt() {
        let (class, _) = classify_stage(None, StageOwner::Unverifiable, Utc::now(), None);
        assert_eq!(class, StageClass::Corrupt);
        assert!(class.quarantine_eligible());
    }

    #[cfg(unix)]
    #[test]
    fn interrupted_stage_is_classifiable_at_every_transition_without_payload_loss() {
        let root = std::env::temp_dir().join(format!(
            "aicx-stage-interrupt-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();

        // T1: interrupted right after metadata, before any payload.
        let staging = root.join(format!(
            ".{CANONICAL_PROJECTION_DIRNAME}.stage-{DEAD_PID}-1"
        ));
        fs::create_dir(&staging).unwrap();
        write_stage_lease(
            &staging,
            &lease_fixture(DEAD_PID, "gone", PROJECTION_STAGE_STATE_STAGING, "none"),
        )
        .unwrap();

        // T2: interrupted after payload complete, before promotion.
        let complete = root.join(format!(
            ".{CANONICAL_PROJECTION_DIRNAME}.stage-{DEAD_PID}-2"
        ));
        fs::create_dir(&complete).unwrap();
        fs::create_dir(complete.join("cards")).unwrap();
        fs::write(complete.join("cards").join("payload.json"), b"{}").unwrap();
        write_stage_lease(
            &complete,
            &lease_fixture(DEAD_PID, "gone", PROJECTION_STAGE_STATE_COMPLETE, "none"),
        )
        .unwrap();

        // T0 (legacy / metadata lost): a stage without any lease.
        let corrupt = root.join(format!(
            ".{CANONICAL_PROJECTION_DIRNAME}.stage-{DEAD_PID}-3"
        ));
        fs::create_dir(&corrupt).unwrap();
        fs::write(corrupt.join("orphan.bin"), b"payload").unwrap();

        let inventory = inspect_projection_stages_at(&root);
        assert_eq!(inventory.len(), 3, "{inventory:?}");
        let class_of = |suffix: &str| {
            inventory
                .iter()
                .find(|entry| entry.path.to_string_lossy().ends_with(suffix))
                .map(|entry| entry.class)
                .unwrap()
        };
        assert_eq!(class_of("-1"), StageClass::DeadOwner);
        assert_eq!(class_of("-2"), StageClass::CompleteUnpromoted);
        assert_eq!(class_of("-3"), StageClass::Corrupt);

        // Inventory never deletes payload.
        assert!(complete.join("cards").join("payload.json").exists());
        assert!(corrupt.join("orphan.bin").exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn committed_fixture_has_no_stage_lease_and_no_active_stages() {
        let root = std::env::temp_dir().join(format!(
            "aicx-stage-lease-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let projection = CanonicalProjection {
            schema: "aicx.store.canonical_projection.v1".to_owned(),
            extraction_schema: "extract-v1".to_owned(),
            producer_version: "parser-v1".to_owned(),
            store_revision: "sr1:lease".to_owned(),
            cards: Vec::new(),
        };
        materialize_projection_fixture(&root, &projection);
        let target = root.join(CANONICAL_PROJECTION_DIRNAME);

        // Committed residual has no ambiguous active stage...
        assert!(inspect_projection_stages_at(&root).is_empty());
        // ...and the committed target carries no lease.
        assert!(!target.join(PROJECTION_STAGE_META_FILENAME).exists());

        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn projection_source_hash_is_stable_and_card_sensitive() {
        let base = CanonicalProjection {
            schema: "aicx.store.canonical_projection.v1".to_owned(),
            extraction_schema: "extract-v1".to_owned(),
            producer_version: "parser-v1".to_owned(),
            store_revision: "sr1:hash".to_owned(),
            cards: Vec::new(),
        };
        let mut drifted = base.clone();
        drifted.store_revision = "sr1:other".to_owned();
        assert_eq!(projection_source_hash(&base), projection_source_hash(&base));
        assert_ne!(
            projection_source_hash(&base),
            projection_source_hash(&drifted)
        );
    }

    #[test]
    fn residual_projection_read_roundtrips_fixture_manifest() {
        let root = std::env::temp_dir().join(format!(
            "aicx-c6-roundtrip-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        let projection = CanonicalProjection {
            schema: "aicx.store.canonical_projection.v1".to_owned(),
            extraction_schema: "extract-v1".to_owned(),
            producer_version: "parser-v1".to_owned(),
            store_revision: "sr1:empty".to_owned(),
            cards: Vec::new(),
        };
        materialize_projection_fixture(&root, &projection);
        let (manifest, cards) = read_canonical_projection_at(&root).unwrap().unwrap();
        assert_eq!(manifest.schema, "aicx.store.manifest.v1");
        assert_eq!(manifest.card_schema, CANONICAL_CARD_SCHEMA);
        assert_eq!(manifest.store_revision, "sr1:empty");
        assert!(cards.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
