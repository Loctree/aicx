//! Shared public steer-index contract for Lance and non-Lance builds.

use crate::timeline::FrameKind;

/// Steer index filter — all 8 optional filters in one bag so helpers don't
/// need long argument lists and callers can build the filter once.
#[derive(Debug, Default, Clone)]
pub struct SteerFilter<'a> {
    pub run_id: Option<&'a str>,
    pub prompt_id: Option<&'a str>,
    pub agent: Option<&'a str>,
    pub kind: Option<&'a str>,
    pub frame_kind: Option<FrameKind>,
    pub project: Option<&'a str>,
    pub date_lo: Option<&'a str>,
    pub date_hi: Option<&'a str>,
}
