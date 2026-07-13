use crate::engine::{Known, SessionModel, Turn};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ProjectAttribution {
    Inferred { version: String },
    OperatorOverride { supplied: String },
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectBucket {
    /// Logical identity is deliberately case-folded on every platform.
    pub slug: String,
    pub attribution: ProjectAttribution,
}

impl ProjectBucket {
    pub fn normalized(slug: &str, attribution: ProjectAttribution) -> Self {
        Self {
            slug: normalize_slug(slug),
            attribution,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpan {
    pub artifact: String,
    pub locator: String,
    pub coverage_ordinal: u64,
    pub physical_ordinal: u64,
    pub content_hash: String,
    pub original_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineFrame {
    pub turn_idx: u64,
    pub segment_id: u32,
    pub role: crate::engine::TurnRole,
    pub kind: crate::engine::TurnKind,
    pub timestamp: Known<String>,
    /// Source-derived UTC calendar bucket. Unknown timestamps remain unbucketed.
    pub date: Option<String>,
    pub cwd: Known<String>,
    pub branch: Known<String>,
    pub text: String,
    pub text_hash: String,
    pub source_spans: Vec<SourceSpan>,
    pub evidence_event_ids: Vec<String>,
}

pub(crate) fn frame(model: &SessionModel, turn: &Turn) -> TimelineFrame {
    let segment = model
        .segments
        .iter()
        .find(|segment| segment.segment_id == turn.segment_id);
    TimelineFrame {
        turn_idx: turn.turn_idx,
        segment_id: turn.segment_id,
        role: turn.role,
        kind: turn.kind,
        timestamp: turn.timestamp.clone(),
        date: known_string(&turn.timestamp)
            .and_then(|timestamp| timestamp.get(..10))
            .filter(|date| {
                date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-')
            })
            .map(str::to_owned),
        cwd: segment
            .map(|segment| segment.cwd.clone())
            .unwrap_or_else(|| model.provenance.cwd.clone()),
        branch: segment
            .map(|segment| segment.branch.clone())
            .unwrap_or_else(|| model.provenance.branch.clone()),
        text: turn.text.clone(),
        text_hash: turn.text_hash.clone(),
        source_spans: turn
            .raw_unit_refs
            .iter()
            .map(|reference| SourceSpan {
                artifact: reference.artifact.clone(),
                locator: reference.locator.clone(),
                coverage_ordinal: reference.coverage_ordinal,
                physical_ordinal: reference.physical_ordinal,
                content_hash: reference.content_hash.clone(),
                original_bytes: reference.original_bytes,
            })
            .collect(),
        evidence_event_ids: turn
            .raw_unit_refs
            .iter()
            .map(|reference| reference.evidence_event_id.clone())
            .collect(),
    }
}

pub(crate) fn project_bucket(
    model: &SessionModel,
    segment_id: u32,
    override_slug: Option<&str>,
    version: &str,
) -> ProjectBucket {
    if let Some(slug) = override_slug {
        return ProjectBucket::normalized(
            slug,
            ProjectAttribution::OperatorOverride {
                supplied: slug.to_owned(),
            },
        );
    }
    let cwd = model
        .segments
        .iter()
        .filter(|segment| segment.segment_id == segment_id)
        .find_map(|segment| known_string(&segment.cwd))
        .or_else(|| known_string(&model.provenance.cwd));
    match cwd.and_then(slug_from_cwd) {
        Some(slug) => ProjectBucket::normalized(
            &slug,
            ProjectAttribution::Inferred {
                version: version.to_owned(),
            },
        ),
        None => ProjectBucket::normalized("non-repository-contexts", ProjectAttribution::Unknown),
    }
}

fn known_string(value: &Known<String>) -> Option<&str> {
    match value {
        Known::Value(value) if !value.trim().is_empty() => Some(value),
        _ => None,
    }
}

fn slug_from_cwd(cwd: &str) -> Option<String> {
    let parts: Vec<_> = cwd
        .replace('\\', "/")
        .split('/')
        .filter(|part| !part.is_empty())
        .map(str::to_owned)
        .collect();
    let repository = parts.last()?;
    let owner = parts.get(parts.len().saturating_sub(2));
    Some(match owner {
        Some(owner) => format!("{owner}/{repository}"),
        None => repository.clone(),
    })
}

fn normalize_slug(value: &str) -> String {
    value
        .replace('\\', "/")
        .split('/')
        .filter_map(|part| {
            let normalized: String = part
                .trim()
                .chars()
                .flat_map(char::to_lowercase)
                .map(|ch| {
                    if ch.is_alphanumeric() || ch == '-' || ch == '_' {
                        ch
                    } else {
                        '-'
                    }
                })
                .collect();
            (!normalized.is_empty()).then_some(normalized)
        })
        .collect::<Vec<_>>()
        .join("/")
}
