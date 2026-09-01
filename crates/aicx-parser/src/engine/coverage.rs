//! Exhaustive raw-unit accounting and orthogonal parse status.
//!
//! Totality contract (W1-T5): every raw unit is either consumed or knowingly
//! skipped, and the ledger says *by kind* and *by reason*:
//!
//! ```text
//! sum(consumed_by_kind) == consumed_count
//! sum(known_skipped)    == skipped_count
//! consumed_count + skipped_count == raw_unit_count
//! ```
//!
//! A2 (codex compaction): frame identity is the content hash of the body, not
//! the rotating `msg_*` id. `compaction_replay` and `duplicate_body` are
//! counted by hash, never by id.

use super::model::RawUnitRef;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisibleCompleteness {
    CompleteVisible,
    PartialVisible,
    Fatal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoundaryFlags {
    pub opaque_reasoning_present: bool,
    pub unsupported_visible_event: bool,
    /// At least one Codex compaction marker was consumed (context_compacted /
    /// top-level compacted / response_item.compacted). Default false for
    /// backward-compatible serde of older coverage snapshots.
    #[serde(default)]
    pub compaction_boundary_present: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParseStatus {
    pub visible_completeness: VisibleCompleteness,
    pub boundary_flags: BoundaryFlags,
    pub malformed_tail_present: bool,
    pub visible_event_lost: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct OrdinalRange {
    pub start: u64,
    pub end: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConsumedUnit {
    pub ordinal: u64,
    pub kind: String,
    pub evidence: RawUnitRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkippedReason {
    UnknownPayloadType,
    Malformed,
    Oversized,
    EncryptedOpaque,
    Unsupported,
    /// Body replayed by a compaction marker (`compacted.replacement_history`).
    /// Known, accounted, never projected as new speech (A2 / Decision 9).
    CompactionReplay,
    /// Same body hash already consumed under a different transport id
    /// (A2: `msg_*` ids rotate after compact while the body is identical).
    DuplicateBody,
}

impl SkippedReason {
    /// Stable snake_case name, identical to the serde form.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::UnknownPayloadType => "unknown_payload_type",
            Self::Malformed => "malformed",
            Self::Oversized => "oversized",
            Self::EncryptedOpaque => "encrypted_opaque",
            Self::Unsupported => "unsupported",
            Self::CompactionReplay => "compaction_replay",
            Self::DuplicateBody => "duplicate_body",
        }
    }

    /// Skips that are an accounting decision of the engine (the bytes were
    /// understood) rather than a limitation (the bytes were not).
    pub const fn is_known_content(self) -> bool {
        matches!(self, Self::CompactionReplay | Self::DuplicateBody)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkippedUnit {
    pub ordinal: u64,
    pub reason: SkippedReason,
    pub bytes: u64,
    pub visible: bool,
    pub evidence: RawUnitRef,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    UnknownPayloadType,
    MalformedUnit,
    OversizedUnit,
    OpaqueReasoning,
    UnsupportedVisibleEvent,
    UnterminatedTail,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageWarning {
    pub kind: WarningKind,
    pub count: u64,
    pub first_ordinal: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub raw_line_count: u64,
    pub raw_unit_count: u64,
    pub consumed_count: u64,
    pub skipped_count: u64,
    pub consumed_ranges: Vec<OrdinalRange>,
    pub consumed: Vec<ConsumedUnit>,
    pub skipped: Vec<SkippedUnit>,
    pub warnings: Vec<CoverageWarning>,
    pub status: ParseStatus,
    /// Consumed units per unit kind. `sum == consumed_count`.
    /// `serde(default)` only so older snapshots deserialize; validation
    /// rejects a ledger that does not match the records (no silent hole).
    #[serde(default)]
    pub consumed_by_kind: BTreeMap<String, u64>,
    /// Knowingly skipped units per reason. `sum == skipped_count`.
    #[serde(default)]
    pub known_skipped: BTreeMap<SkippedReason, u64>,
}

/// The ledger disagrees with the records it claims to summarize.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TotalityError {
    /// `sum(consumed_by_kind) != consumed_count`.
    ConsumedByKind { ledger: u64, records: u64 },
    /// `sum(known_skipped) != skipped_count`.
    KnownSkipped { ledger: u64, records: u64 },
    /// `consumed_count + skipped_count != raw_unit_count`.
    Partition {
        consumed: u64,
        skipped: u64,
        raw_unit_count: u64,
    },
    /// The per-kind / per-reason breakdown differs from a recount of the
    /// records (e.g. a stale ledger deserialized from an older snapshot).
    LedgerDrift { field: &'static str },
}

impl fmt::Display for TotalityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ConsumedByKind { ledger, records } => write!(
                formatter,
                "consumed_by_kind sums to {ledger} but consumed_count is {records}"
            ),
            Self::KnownSkipped { ledger, records } => write!(
                formatter,
                "known_skipped sums to {ledger} but skipped_count is {records}"
            ),
            Self::Partition {
                consumed,
                skipped,
                raw_unit_count,
            } => write!(
                formatter,
                "consumed {consumed} + skipped {skipped} != raw_unit_count {raw_unit_count}"
            ),
            Self::LedgerDrift { field } => {
                write!(formatter, "{field} differs from a recount of the records")
            }
        }
    }
}

impl std::error::Error for TotalityError {}

impl CoverageReport {
    pub fn new(
        raw_unit_count: u64,
        consumed: Vec<ConsumedUnit>,
        skipped: Vec<SkippedUnit>,
        warnings: Vec<CoverageWarning>,
        status: ParseStatus,
    ) -> Self {
        Self::with_raw_line_count(
            raw_unit_count,
            raw_unit_count,
            consumed,
            skipped,
            warnings,
            status,
        )
    }

    pub fn with_raw_line_count(
        raw_line_count: u64,
        raw_unit_count: u64,
        consumed: Vec<ConsumedUnit>,
        skipped: Vec<SkippedUnit>,
        warnings: Vec<CoverageWarning>,
        status: ParseStatus,
    ) -> Self {
        let consumed_ranges = ranges_for(consumed.iter().map(|unit| unit.ordinal));
        let consumed_by_kind = consumed_by_kind(&consumed);
        let known_skipped = known_skipped(&skipped);
        Self {
            raw_line_count,
            raw_unit_count,
            consumed_count: consumed.len() as u64,
            skipped_count: skipped.len() as u64,
            consumed_ranges,
            consumed,
            skipped,
            warnings,
            status,
            consumed_by_kind,
            known_skipped,
        }
    }

    /// Recompute the ledger from the records. Use after deserializing a
    /// snapshot that predates the ledger; never to paper over drift.
    pub fn rebuild_ledger(&mut self) {
        self.consumed_by_kind = consumed_by_kind(&self.consumed);
        self.known_skipped = known_skipped(&self.skipped);
    }

    /// Consumed units of one kind.
    pub fn consumed_of_kind(&self, kind: &str) -> u64 {
        self.consumed_by_kind.get(kind).copied().unwrap_or(0)
    }

    /// Knowingly skipped units for one reason.
    pub fn skipped_for(&self, reason: SkippedReason) -> u64 {
        self.known_skipped.get(&reason).copied().unwrap_or(0)
    }

    /// Units that are content the engine understood but refused to project
    /// as new speech (compaction replay + duplicate body).
    pub fn replayed_total(&self) -> u64 {
        self.skipped_for(SkippedReason::CompactionReplay)
            + self.skipped_for(SkippedReason::DuplicateBody)
    }

    /// The totality invariant. `Ok(())` means: every raw unit is accounted
    /// for exactly once and the ledger matches the records.
    pub fn check_totality(&self) -> Result<(), TotalityError> {
        let consumed_ledger: u64 = self.consumed_by_kind.values().sum();
        if consumed_ledger != self.consumed_count {
            return Err(TotalityError::ConsumedByKind {
                ledger: consumed_ledger,
                records: self.consumed_count,
            });
        }
        let skipped_ledger: u64 = self.known_skipped.values().sum();
        if skipped_ledger != self.skipped_count {
            return Err(TotalityError::KnownSkipped {
                ledger: skipped_ledger,
                records: self.skipped_count,
            });
        }
        if self.consumed_count.checked_add(self.skipped_count) != Some(self.raw_unit_count) {
            return Err(TotalityError::Partition {
                consumed: self.consumed_count,
                skipped: self.skipped_count,
                raw_unit_count: self.raw_unit_count,
            });
        }
        if self.consumed_by_kind != consumed_by_kind(&self.consumed) {
            return Err(TotalityError::LedgerDrift {
                field: "consumed_by_kind",
            });
        }
        if self.known_skipped != known_skipped(&self.skipped) {
            return Err(TotalityError::LedgerDrift {
                field: "known_skipped",
            });
        }
        Ok(())
    }
}

/// Per-kind recount of consumed records.
pub fn consumed_by_kind(consumed: &[ConsumedUnit]) -> BTreeMap<String, u64> {
    let mut ledger = BTreeMap::new();
    for unit in consumed {
        *ledger.entry(unit.kind.clone()).or_insert(0) += 1;
    }
    ledger
}

/// Per-reason recount of skipped records.
pub fn known_skipped(skipped: &[SkippedUnit]) -> BTreeMap<SkippedReason, u64> {
    let mut ledger = BTreeMap::new();
    for unit in skipped {
        *ledger.entry(unit.reason).or_insert(0) += 1;
    }
    ledger
}

/// Count bodies whose content hash was already seen, in evidence order.
/// This is the A2 measure: identity by hash, not by transport id. Adapters
/// use it to decide `SkippedReason::DuplicateBody` for the repeats.
pub fn duplicate_bodies<'a>(evidence: impl IntoIterator<Item = &'a RawUnitRef>) -> u64 {
    let mut seen = BTreeSet::new();
    let mut duplicates = 0;
    for unit in evidence {
        if !seen.insert(unit.content_hash.as_str()) {
            duplicates += 1;
        }
    }
    duplicates
}

pub(crate) fn ranges_for(ordinals: impl IntoIterator<Item = u64>) -> Vec<OrdinalRange> {
    let mut ordinals: Vec<_> = ordinals.into_iter().collect();
    ordinals.sort_unstable();
    let mut ranges = Vec::new();
    for ordinal in ordinals {
        match ranges.last_mut() {
            Some(OrdinalRange { end, .. }) if end.saturating_add(1) == ordinal => *end = ordinal,
            _ => ranges.push(OrdinalRange {
                start: ordinal,
                end: ordinal,
            }),
        }
    }
    ranges
}
