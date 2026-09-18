//! `aicx extract all` — one bulk projection pass over every compatible source.
//!
//! Deliberately *not* a second extractor. Discovery is the session catalog,
//! parsing is `parser_dispatch`, and the view is the same [`ProjectionSpec`]
//! that `aicx extract <agent>` builds. This module owns only what bulk adds:
//! per-source accounting, incrementality, and a manifest that says what
//! happened to every source it saw.
//!
//! Three invariants shape the code:
//!
//! * **Nothing is silently equal.** "No sessions" (an empty catalog) and "the
//!   sources failed to parse" are different rows in the manifest and different
//!   exit statuses. A partial run never reports overall success.
//! * **A rerun is free and safe.** The incremental key is source fingerprint +
//!   parser version + projection fingerprint, so an unchanged source with an
//!   unchanged view is skipped, and a changed parser invalidates the cache
//!   without the operator having to know that it should.
//! * **Variants never collide.** Two different filter sets over the same
//!   session write to two different files; neither overwrites the other.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::extraction::projection::ProjectionSpec;
use crate::session_catalog::AgentKind;

pub const BULK_MANIFEST_SCHEMA: &str = "aicx.extract.all.manifest.v1";
pub const BULK_STATE_SCHEMA: &str = "aicx.extract.all.state.v1";

/// Directory holding bulk bookkeeping under the extracts root.
pub const BULK_DIRNAME: &str = "_bulk";

/// Every agent the parser registry can actually claim.
///
/// The catalog's own list ([`AgentKind::ALL`]), not a copy of it: a provider
/// added to the registry is picked up here without a second edit, and a
/// provider that only exists in help text is not. The previous hand-written
/// five-entry array claimed to be derived and was not.
pub const ALL_AGENTS: [AgentKind; AgentKind::ALL.len()] = AgentKind::ALL;

/// What happened to one discovered source.
///
/// `Unchanged` is a success (the projection on disk is already correct);
/// `EmptyAfterFilter` is a success with nothing to write; `Failed` is the only
/// state that degrades the run's exit status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum SourceOutcome {
    Extracted {
        output_path: String,
        entries: usize,
    },
    Unchanged {
        output_path: String,
    },
    /// The requested filters admit none of this source's events. Not a
    /// failure: `-H 1 -p some/repo` legitimately empties most sessions.
    ///
    /// `reason` says how that was established, because "parsed and found
    /// nothing" and "proved it could hold nothing" are different facts.
    EmptyAfterFilter {
        reason: String,
    },
    /// Discovered, but this run cannot claim it (no adapter, unreadable
    /// artifact shape). Distinct from `Failed`: nothing went wrong, the
    /// support simply is not there.
    Unsupported {
        reason: String,
    },
    /// Filtered out before parsing by the session-level project filter.
    FilteredOut,
    Failed {
        reason: String,
        recover: String,
    },
}

impl SourceOutcome {
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::Extracted { .. } => "extracted",
            Self::Unchanged { .. } => "unchanged",
            Self::EmptyAfterFilter { .. } => "empty_after_filter",
            Self::Unsupported { .. } => "unsupported",
            Self::FilteredOut => "filtered_out",
            Self::Failed { .. } => "failed",
        }
    }

    pub const fn is_failure(&self) -> bool {
        matches!(self, Self::Failed { .. })
    }
}

/// One manifest row: what was seen, who could read it, and what came of it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestEntry {
    pub provider: String,
    pub source_id: String,
    pub logical_session_id: Option<String>,
    pub source_path: String,
    /// Content-independent source identity: size + mtime as the catalog saw
    /// them. Cheap enough to compute for every source in a large archive.
    pub source_fingerprint: String,
    pub parser_version: String,
    #[serde(flatten)]
    pub outcome: SourceOutcome,
}

/// Run-level counters. Every discovered source lands in exactly one bucket, so
/// the totals reconcile against `discovered` — a manifest that does not add up
/// is a bug, not a rounding artifact.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestTotals {
    pub discovered: usize,
    pub selected: usize,
    pub extracted: usize,
    pub unchanged: usize,
    pub empty_after_filter: usize,
    pub unsupported: usize,
    pub filtered_out: usize,
    pub failed: usize,
}

impl ManifestTotals {
    fn record(&mut self, outcome: &SourceOutcome) {
        self.discovered += 1;
        match outcome {
            SourceOutcome::Extracted { .. } => {
                self.selected += 1;
                self.extracted += 1;
            }
            SourceOutcome::Unchanged { .. } => {
                self.selected += 1;
                self.unchanged += 1;
            }
            SourceOutcome::EmptyAfterFilter { .. } => {
                self.selected += 1;
                self.empty_after_filter += 1;
            }
            SourceOutcome::Unsupported { .. } => self.unsupported += 1,
            SourceOutcome::FilteredOut => self.filtered_out += 1,
            SourceOutcome::Failed { .. } => {
                self.selected += 1;
                self.failed += 1;
            }
        }
    }

    /// Sanity invariant used by tests and by the CLI summary.
    pub fn reconciles(&self) -> bool {
        self.extracted
            + self.unchanged
            + self.empty_after_filter
            + self.unsupported
            + self.filtered_out
            + self.failed
            == self.discovered
    }
}

/// The filter set a run applied, recorded so a later reader can tell why a
/// session is absent without re-deriving the flags.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManifestFilters {
    pub roles: Vec<String>,
    pub kinds: Vec<String>,
    pub shell_executors: Vec<String>,
    pub projects: Vec<String>,
    pub hours: Option<u64>,
    pub result_body: String,
    pub conversation: bool,
    /// The single instant every `-H` comparison in this run used, in UTC.
    pub cutoff_utc: String,
    /// Stable digest of everything above: the incremental cache key's view
    /// half.
    pub projection_fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkManifest {
    pub schema: &'static str,
    pub aicx_version: &'static str,
    pub generated_at: String,
    pub agents_requested: Vec<String>,
    pub filters: ManifestFilters,
    pub totals: ManifestTotals,
    pub entries: Vec<ManifestEntry>,
}

impl BulkManifest {
    /// Exit status contract: clean run `0`, partial `3`, everything failed `4`.
    ///
    /// `3` and `4` are distinct so automation can tell "some sources are
    /// broken" from "this run produced nothing at all". An empty archive is
    /// still `0`: having no sessions is not an error.
    pub const fn exit_code(&self) -> i32 {
        if self.totals.failed == 0 {
            0
        } else if self.totals.failed == self.totals.selected {
            4
        } else {
            3
        }
    }

    pub const fn is_partial(&self) -> bool {
        self.totals.failed > 0
    }
}

/// Persisted incremental state: what this machine already materialized.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct BulkState {
    #[serde(default)]
    pub schema: String,
    /// `"<provider>\u{1f}<source_id>\u{1f}<projection_fingerprint>"` ->
    /// what was written for it.
    #[serde(default)]
    pub entries: BTreeMap<String, BulkStateEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BulkStateEntry {
    pub source_fingerprint: String,
    pub parser_version: String,
    pub output_path: String,
    pub extracted_at: String,
}

/// Unit separator: cannot appear in a provider label, a source id, or a hex
/// fingerprint, so the composite key is unambiguous.
const KEY_SEP: char = '\u{1f}';

pub fn state_key(provider: &str, source_id: &str, projection_fingerprint: &str) -> String {
    format!("{provider}{KEY_SEP}{source_id}{KEY_SEP}{projection_fingerprint}")
}

pub fn bulk_dir(extracts_root: &Path) -> PathBuf {
    extracts_root.join(BULK_DIRNAME)
}

pub fn state_path(extracts_root: &Path) -> PathBuf {
    bulk_dir(extracts_root).join("state.json")
}

pub fn manifest_path(extracts_root: &Path, generated_at: DateTime<Utc>) -> PathBuf {
    bulk_dir(extracts_root).join(format!(
        "manifest-{}.json",
        generated_at.format("%Y%m%dT%H%M%SZ")
    ))
}

pub fn latest_manifest_path(extracts_root: &Path) -> PathBuf {
    bulk_dir(extracts_root).join("manifest-latest.json")
}

/// Load prior state; a missing or unreadable file is an empty state, never a
/// hard failure. The state is a cache — losing it costs work, not truth.
pub fn load_state(extracts_root: &Path) -> BulkState {
    let path = state_path(extracts_root);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return BulkState::default();
    };
    match serde_json::from_str::<BulkState>(&raw) {
        Ok(state) if state.schema == BULK_STATE_SCHEMA => state,
        Ok(_) => {
            // A state written by another schema version cannot be trusted to
            // mean the same thing; re-materializing is cheap next to a wrong
            // "unchanged".
            crate::diagnostics::log_describe(
                "bulk_state_schema_mismatch: ignoring prior extract-all state",
            );
            BulkState::default()
        }
        Err(error) => {
            crate::diagnostics::log_describe(&format!("bulk_state_unreadable: {error}"));
            BulkState::default()
        }
    }
}

pub fn save_state(extracts_root: &Path, state: &BulkState) -> Result<()> {
    let dir = bulk_dir(extracts_root);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let payload = serde_json::to_vec_pretty(state).context("failed to encode extract-all state")?;
    crate::legacy_archive::atomic_write::atomic_write(&state_path(extracts_root), &payload)
        .with_context(|| format!("failed to write {}", state_path(extracts_root).display()))?;
    Ok(())
}

pub fn save_manifest(extracts_root: &Path, manifest: &BulkManifest) -> Result<PathBuf> {
    let dir = bulk_dir(extracts_root);
    std::fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    let payload =
        serde_json::to_vec_pretty(manifest).context("failed to encode extract-all manifest")?;
    let generated_at = manifest
        .generated_at
        .parse::<DateTime<Utc>>()
        .unwrap_or_else(|_| Utc::now());
    let path = manifest_path(extracts_root, generated_at);
    crate::legacy_archive::atomic_write::atomic_write(&path, &payload)
        .with_context(|| format!("failed to write {}", path.display()))?;
    crate::legacy_archive::atomic_write::atomic_write(
        &latest_manifest_path(extracts_root),
        &payload,
    )
    .with_context(|| "failed to write manifest-latest.json")?;
    Ok(path)
}

/// Stable digest of the axes that change *what a projection contains*.
///
/// Deliberately excludes cosmetic axes (`max_message_chars` truncation is a
/// rendering choice, `dialog` only reveals seal metadata) — no, it includes
/// both, because both change the bytes on disk and therefore whether a cached
/// extract is still correct. What it excludes is the cutoff *instant*: two
/// runs an hour apart with the same `-H 24` share a fingerprint, and the
/// source fingerprint is what tells them apart.
pub fn projection_fingerprint(spec: &ProjectionSpec, conversation: bool) -> String {
    let mut roles: Vec<&str> = spec.roles.iter().map(|role| role.as_cli_token()).collect();
    roles.sort_unstable();
    let mut kinds: Vec<&str> = spec.kinds.iter().map(|kind| kind.as_cli_token()).collect();
    kinds.sort_unstable();
    let mut executors: Vec<&str> = spec
        .shell_executors
        .iter()
        .map(|executor| executor.as_str())
        .collect();
    executors.sort_unstable();
    let mut projects = spec.project.clone();
    projects.sort();
    let canonical = format!(
        "v1|roles={}|kinds={}|exec={}|projects={}|hours={}|since={}|until={}|result={:?}|chars={}|dialog={}|lineage={:?}|conversation={}",
        roles.join(","),
        kinds.join(","),
        executors.join(","),
        projects.join(","),
        spec.window.hours.unwrap_or(0),
        spec.window.since.as_deref().unwrap_or(""),
        spec.window.until.as_deref().unwrap_or(""),
        spec.result,
        spec.max_message_chars,
        spec.dialog,
        spec.lineage_depth,
        conversation,
    );
    let digest = aicx_parser::engine::sha256_hex(canonical.as_bytes());
    digest[..16].to_owned()
}

/// Reason strings for [`SourceOutcome::EmptyAfterFilter`].
pub const EMPTY_BY_WINDOW_PROOF: &str =
    "source was last written before the -H cutoff, so it cannot hold an in-window event";
pub const EMPTY_AFTER_PROJECTION: &str = "parsed; the requested filters admitted no entry";

/// Can this source be skipped without opening it, given an `-H` window?
///
/// A file's mtime is an upper bound on the newest event it can contain: no
/// record inside was written after the file itself last was. So
/// `mtime < cutoff` *proves* the source holds nothing in the window, and the
/// skip is a deduction rather than the "pick files by mtime" shortcut that
/// would wrongly drop a freshly-copied archive of old events (that case has
/// `mtime >= cutoff` and is still parsed).
///
/// Returns `false` whenever the window is unbounded or the mtime is unusable,
/// so the fallback is always "open it and look".
pub fn window_proves_empty(
    modified_unix_nanos: u128,
    hours: Option<u64>,
    cutoff: DateTime<Utc>,
) -> bool {
    let Some(hours) = hours.filter(|hours| *hours > 0) else {
        return false;
    };
    let Some(lower_bound) = cutoff.checked_sub_signed(chrono::Duration::hours(hours as i64)) else {
        return false;
    };
    let Some(lower_nanos) = lower_bound.timestamp_nanos_opt() else {
        return false;
    };
    if lower_nanos <= 0 {
        return false;
    }
    modified_unix_nanos < lower_nanos as u128
}

/// Source identity for the incremental key: size and mtime as recorded by the
/// catalog scan.
///
/// Not a content hash: hashing every byte of a multi-gigabyte archive on every
/// run would defeat the purpose of an incremental pass. A source rewritten in
/// place with an identical size *and* an identical mtime is the one case this
/// misses, which `--rebuild` exists for.
pub fn source_fingerprint(size_bytes: u64, modified_unix_nanos: u128) -> String {
    format!("s{size_bytes:x}m{modified_unix_nanos:x}")
}

/// Filename stem for one session under one projection variant.
///
/// The legacy axes keep their readable suffixes so existing paths are stable;
/// any further narrowing appends the projection fingerprint, which is what
/// keeps `--agent-only` from overwriting `--user-only` for the same session.
pub fn variant_stem(
    session_stem: &str,
    conversation: bool,
    user_only: bool,
    extra_variant: Option<&str>,
) -> String {
    let mut stem = session_stem.to_owned();
    if conversation {
        stem.push_str("_conversation");
    }
    if user_only {
        stem.push_str("_user");
    }
    if let Some(variant) = extra_variant {
        stem.push('_');
        stem.push_str(variant);
    }
    stem
}

/// Build the run manifest from the rows a run produced.
pub fn build_manifest(
    generated_at: DateTime<Utc>,
    agents: &[AgentKind],
    filters: ManifestFilters,
    entries: Vec<ManifestEntry>,
) -> BulkManifest {
    let mut totals = ManifestTotals::default();
    for entry in &entries {
        totals.record(&entry.outcome);
    }
    BulkManifest {
        schema: BULK_MANIFEST_SCHEMA,
        aicx_version: env!("CARGO_PKG_VERSION"),
        generated_at: generated_at.to_rfc3339(),
        agents_requested: agents.iter().map(ToString::to_string).collect(),
        filters,
        totals,
        entries,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extraction::projection::{ProjectionKind, ProjectionRole, ShellExecutor};

    fn entry(provider: &str, outcome: SourceOutcome) -> ManifestEntry {
        ManifestEntry {
            provider: provider.to_owned(),
            source_id: format!("{provider}-1"),
            logical_session_id: None,
            source_path: "/tmp/x".to_owned(),
            source_fingerprint: "s1m1".to_owned(),
            parser_version: "test".to_owned(),
            outcome,
        }
    }

    #[test]
    fn totals_reconcile_across_every_bucket() {
        let manifest = build_manifest(
            Utc::now(),
            &ALL_AGENTS,
            filters_fixture(),
            vec![
                entry(
                    "claude",
                    SourceOutcome::Extracted {
                        output_path: "/tmp/a.md".to_owned(),
                        entries: 3,
                    },
                ),
                entry(
                    "codex",
                    SourceOutcome::Unchanged {
                        output_path: "/tmp/b.md".to_owned(),
                    },
                ),
                entry(
                    "grok",
                    SourceOutcome::EmptyAfterFilter {
                        reason: EMPTY_AFTER_PROJECTION.to_owned(),
                    },
                ),
                entry(
                    "gemini",
                    SourceOutcome::Unsupported {
                        reason: "no adapter".to_owned(),
                    },
                ),
                entry("junie", SourceOutcome::FilteredOut),
                entry(
                    "claude",
                    SourceOutcome::Failed {
                        reason: "bad json".to_owned(),
                        recover: "aicx extract claude --file ...".to_owned(),
                    },
                ),
            ],
        );
        assert!(manifest.totals.reconciles());
        assert_eq!(manifest.totals.discovered, 6);
        assert_eq!(manifest.totals.extracted, 1);
        assert_eq!(manifest.totals.unchanged, 1);
        assert_eq!(manifest.totals.empty_after_filter, 1);
        assert_eq!(manifest.totals.unsupported, 1);
        assert_eq!(manifest.totals.filtered_out, 1);
        assert_eq!(manifest.totals.failed, 1);
        // Unsupported and filtered-out sources were never selected for work.
        assert_eq!(manifest.totals.selected, 4);
    }

    fn filters_fixture() -> ManifestFilters {
        ManifestFilters {
            roles: vec!["human".to_owned()],
            kinds: vec!["human".to_owned()],
            shell_executors: vec!["human".to_owned(), "agent".to_owned()],
            projects: Vec::new(),
            hours: None,
            result_body: "none".to_owned(),
            conversation: false,
            cutoff_utc: Utc::now().to_rfc3339(),
            projection_fingerprint: "deadbeefdeadbeef".to_owned(),
        }
    }

    #[test]
    fn empty_archive_is_success_not_failure() {
        let manifest = build_manifest(Utc::now(), &ALL_AGENTS, filters_fixture(), Vec::new());
        assert_eq!(manifest.exit_code(), 0);
        assert!(!manifest.is_partial());
        assert!(manifest.totals.reconciles());
    }

    #[test]
    fn partial_and_total_failure_have_distinct_exit_codes() {
        let partial = build_manifest(
            Utc::now(),
            &ALL_AGENTS,
            filters_fixture(),
            vec![
                entry(
                    "claude",
                    SourceOutcome::Extracted {
                        output_path: "/tmp/a.md".to_owned(),
                        entries: 1,
                    },
                ),
                entry(
                    "codex",
                    SourceOutcome::Failed {
                        reason: "boom".to_owned(),
                        recover: "retry".to_owned(),
                    },
                ),
            ],
        );
        assert_eq!(partial.exit_code(), 3);
        assert!(partial.is_partial());

        let total = build_manifest(
            Utc::now(),
            &ALL_AGENTS,
            filters_fixture(),
            vec![entry(
                "codex",
                SourceOutcome::Failed {
                    reason: "boom".to_owned(),
                    recover: "retry".to_owned(),
                },
            )],
        );
        assert_eq!(total.exit_code(), 4);
    }

    #[test]
    fn projection_fingerprint_separates_variants_and_is_stable() {
        let base = ProjectionSpec::default();
        let base_fp = projection_fingerprint(&base, false);
        assert_eq!(
            base_fp,
            projection_fingerprint(&ProjectionSpec::default(), false)
        );
        assert_eq!(base_fp.len(), 16);

        let user_only = ProjectionSpec {
            roles: vec![ProjectionRole::Human],
            ..ProjectionSpec::default()
        };
        assert_ne!(base_fp, projection_fingerprint(&user_only, false));

        let agent_only = ProjectionSpec {
            roles: vec![ProjectionRole::Assistant],
            kinds: vec![ProjectionKind::AssistantFinal],
            ..ProjectionSpec::default()
        };
        assert_ne!(
            projection_fingerprint(&user_only, false),
            projection_fingerprint(&agent_only, false)
        );

        // The executor axis must move the fingerprint: `--user-commands` and
        // `--agent-commands` render different files from the same session.
        let user_cmds = ProjectionSpec {
            kinds: vec![ProjectionKind::ShellAction],
            shell_executors: vec![ShellExecutor::Human],
            ..ProjectionSpec::default()
        };
        let agent_cmds = ProjectionSpec {
            shell_executors: vec![ShellExecutor::Agent],
            ..user_cmds.clone()
        };
        assert_ne!(
            projection_fingerprint(&user_cmds, false),
            projection_fingerprint(&agent_cmds, false)
        );

        // Conversation mode is a different rendering of the same events.
        assert_ne!(base_fp, projection_fingerprint(&base, true));

        // Order of repeatable -p must not change identity.
        let a = ProjectionSpec {
            project: vec!["x/y".to_owned(), "p/q".to_owned()],
            ..ProjectionSpec::default()
        };
        let b = ProjectionSpec {
            project: vec!["p/q".to_owned(), "x/y".to_owned()],
            ..ProjectionSpec::default()
        };
        assert_eq!(
            projection_fingerprint(&a, false),
            projection_fingerprint(&b, false)
        );
    }

    #[test]
    fn variant_stem_keeps_filter_sets_apart() {
        assert_eq!(variant_stem("sess", false, false, None), "sess");
        assert_eq!(
            variant_stem("sess", true, true, None),
            "sess_conversation_user"
        );
        assert_ne!(
            variant_stem("sess", false, false, Some("aaaa")),
            variant_stem("sess", false, false, Some("bbbb"))
        );
    }

    #[test]
    fn state_key_is_unambiguous_across_components() {
        // Without a separator that cannot occur inside a component, `a` + `bc`
        // and `ab` + `c` would collide.
        assert_ne!(state_key("a", "bc", "fp"), state_key("ab", "c", "fp"));
        assert_ne!(state_key("a", "b", "fp1"), state_key("a", "b", "fp2"));
    }

    #[test]
    fn the_window_skip_is_a_deduction_not_an_mtime_shortcut() {
        let cutoff = DateTime::parse_from_rfc3339("2026-09-10T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let nanos = |rfc: &str| {
            DateTime::parse_from_rfc3339(rfc)
                .unwrap()
                .timestamp_nanos_opt()
                .unwrap() as u128
        };

        // Written long before the window: it cannot hold an in-window event.
        assert!(window_proves_empty(
            nanos("2026-09-01T00:00:00Z"),
            Some(24),
            cutoff
        ));
        // Written inside the window: must still be opened and looked at, even
        // though its *events* may all be old (a freshly copied archive).
        assert!(!window_proves_empty(
            nanos("2026-09-10T11:00:00Z"),
            Some(24),
            cutoff
        ));
        // No window at all: never skip.
        assert!(!window_proves_empty(
            nanos("2020-01-01T00:00:00Z"),
            None,
            cutoff
        ));
        assert!(!window_proves_empty(
            nanos("2020-01-01T00:00:00Z"),
            Some(0),
            cutoff
        ));
        // An mtime at the epoch is older than any sane cutoff, so the proof
        // holds — but the deduction still only ever *skips* reading; it never
        // claims the source is broken.
        assert!(window_proves_empty(0, Some(24), cutoff));
        // Exactly at the boundary the source is kept: `<` not `<=`, so an
        // event written in the same nanosecond as the cutoff is not lost.
        let boundary = cutoff
            .checked_sub_signed(chrono::Duration::hours(24))
            .unwrap()
            .timestamp_nanos_opt()
            .unwrap() as u128;
        assert!(!window_proves_empty(boundary, Some(24), cutoff));
        assert!(window_proves_empty(boundary - 1, Some(24), cutoff));
    }

    #[test]
    fn source_fingerprint_tracks_size_and_mtime() {
        assert_ne!(source_fingerprint(10, 1), source_fingerprint(11, 1));
        assert_ne!(source_fingerprint(10, 1), source_fingerprint(10, 2));
        assert_eq!(source_fingerprint(10, 1), source_fingerprint(10, 1));
    }
}
