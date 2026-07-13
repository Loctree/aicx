//! Deterministic projections from the validated parser model.

mod canonical_chunks;
mod timeline;

pub use canonical_chunks::{
    CANONICAL_CARD_SCHEMA, CanonicalCard, CanonicalProjection, ProjectionConfig, ProjectionError,
    UsageReference, project_validated_session,
};
pub use timeline::{ProjectAttribution, ProjectBucket, SourceSpan, TimelineFrame};
