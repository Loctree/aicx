//! Exhaustive raw-unit accounting and orthogonal parse status.

use super::model::RawUnitRef;
use serde::{Deserialize, Serialize};

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkippedReason {
    UnknownPayloadType,
    Malformed,
    Oversized,
    EncryptedOpaque,
    Unsupported,
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
}

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
        }
    }
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
