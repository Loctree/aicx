use super::timeline::{ProjectBucket, TimelineFrame, frame, project_bucket};
use crate::engine::{AgentKind, Known, ParseStatus, UsageEvent, ValidatedSession};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

pub const CANONICAL_CARD_SCHEMA: &str = "aicx.store.canonical_card.v3";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionConfig {
    pub extraction_schema: String,
    pub producer_version: String,
    pub attribution_version: String,
    pub project_override: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReference {
    pub evidence_event_id: String,
    pub provider: String,
    pub model: Known<String>,
    pub counter_semantics: crate::engine::CounterSemantics,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalCard {
    pub schema: String,
    pub id: String,
    pub session_id: String,
    pub project: ProjectBucket,
    pub agent: AgentKind,
    pub model: Known<String>,
    pub source_hash: String,
    pub source_bytes: u64,
    pub frame: TimelineFrame,
    pub evidence_event_ids: Vec<String>,
    pub parse_status: ParseStatus,
    pub usage_references: Vec<UsageReference>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CanonicalProjection {
    pub schema: String,
    pub extraction_schema: String,
    pub producer_version: String,
    pub store_revision: String,
    pub cards: Vec<CanonicalCard>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectionError(String);

impl fmt::Display for ProjectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for ProjectionError {}

pub fn project_validated_session(
    session: &ValidatedSession,
    config: &ProjectionConfig,
) -> Result<CanonicalProjection, ProjectionError> {
    let model = session.model();
    let usage = usage_refs(&model.usage_events);
    let mut cards = Vec::with_capacity(model.turns.len());
    let mut ids = BTreeSet::new();
    for turn in &model.turns {
        let frame = frame(model, turn);
        let project = project_bucket(
            model,
            turn.segment_id,
            config.project_override.as_deref(),
            &config.attribution_version,
        );
        let evidence_event_ids = frame.evidence_event_ids.clone();
        let id = card_id(
            &model.session_id,
            turn.turn_idx,
            &evidence_event_ids,
            &turn.text_hash,
        );
        if !ids.insert(id.clone()) {
            return Err(ProjectionError(format!(
                "duplicate canonical card id: {id}"
            )));
        }
        cards.push(CanonicalCard {
            schema: CANONICAL_CARD_SCHEMA.to_owned(),
            id,
            session_id: model.session_id.clone(),
            project,
            agent: model.provenance.agent,
            model: model.provenance.model.clone(),
            source_hash: model.provenance.original_source_hash.clone(),
            source_bytes: model.provenance.original_source_bytes,
            frame,
            evidence_event_ids,
            parse_status: model.coverage.status,
            usage_references: usage.clone(),
        });
    }
    projection_from_cards(cards, config)
}

/// Build one deterministic store projection from already validated cards.
///
/// Runtime ingest uses this to replace the cards for sessions retried in the
/// current batch while preserving cards produced by earlier healthy batches.
pub fn projection_from_cards(
    mut cards: Vec<CanonicalCard>,
    config: &ProjectionConfig,
) -> Result<CanonicalProjection, ProjectionError> {
    if config.extraction_schema.trim().is_empty() || config.producer_version.trim().is_empty() {
        return Err(ProjectionError(
            "extraction schema and producer version are required".to_owned(),
        ));
    }
    cards.sort_by(|left, right| left.id.cmp(&right.id));
    for pair in cards.windows(2) {
        if pair[0].id == pair[1].id {
            return Err(ProjectionError(format!(
                "duplicate canonical card id: {}",
                pair[0].id
            )));
        }
    }
    if let Some(card) = cards
        .iter()
        .find(|card| card.schema != CANONICAL_CARD_SCHEMA)
    {
        return Err(ProjectionError(format!(
            "unsupported canonical card schema for {}: {}",
            card.id, card.schema
        )));
    }
    let store_revision = store_revision(&cards, config);
    Ok(CanonicalProjection {
        schema: "aicx.store.canonical_projection.v1".to_owned(),
        extraction_schema: config.extraction_schema.clone(),
        producer_version: config.producer_version.clone(),
        store_revision,
        cards,
    })
}

fn usage_refs(events: &[UsageEvent]) -> Vec<UsageReference> {
    let mut refs: Vec<_> = events
        .iter()
        .map(|event| UsageReference {
            evidence_event_id: event.evidence.evidence_event_id.clone(),
            provider: event.provider.clone(),
            model: event.model.clone(),
            counter_semantics: event.counter_semantics,
        })
        .collect();
    refs.sort_by(|left, right| left.evidence_event_id.cmp(&right.evidence_event_id));
    refs.dedup_by(|left, right| left.evidence_event_id == right.evidence_event_id);
    refs
}

fn card_id(session_id: &str, turn_idx: u64, evidence: &[String], text_hash: &str) -> String {
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, session_id);
    hasher.update(turn_idx.to_be_bytes());
    for id in evidence {
        hash_field(&mut hasher, id);
    }
    hash_field(&mut hasher, text_hash);
    format!("card3:{}", &format!("{:x}", hasher.finalize())[..24])
}

fn store_revision(cards: &[CanonicalCard], config: &ProjectionConfig) -> String {
    let mut scoped: BTreeMap<&str, Vec<(&str, &str, &str)>> = BTreeMap::new();
    for card in cards {
        scoped.entry(&card.project.slug).or_default().push((
            &card.id,
            &card.source_hash,
            &card.frame.text_hash,
        ));
    }
    let mut hasher = Sha256::new();
    hash_field(&mut hasher, &config.extraction_schema);
    hash_field(&mut hasher, &config.producer_version);
    for (scope, members) in scoped {
        hash_field(&mut hasher, scope);
        for (id, source_hash, content_hash) in members {
            hash_field(&mut hasher, id);
            hash_field(&mut hasher, source_hash);
            hash_field(&mut hasher, content_hash);
        }
    }
    format!("sr1:{:x}", hasher.finalize())
}

fn hash_field(hasher: &mut Sha256, value: &str) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value.as_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{
        BoundaryFlags, CounterSemantics, CoverageReport, Known, ParseStatus, Provenance,
        RawUnitRef, TokenComponents, Turn, TurnKind, TurnRole, UnvalidatedParse, UsageEvent,
        VisibleCompleteness, validate_parse,
    };

    fn validated(turns: usize, cwd: &str) -> ValidatedSession {
        let status = ParseStatus {
            visible_completeness: VisibleCompleteness::CompleteVisible,
            boundary_flags: BoundaryFlags::default(),
            malformed_tail_present: false,
            visible_event_lost: false,
        };
        let provenance = Provenance {
            agent: AgentKind::Codex,
            model: Known::value("gpt-test".to_owned()),
            cli_version: Known::unknown(),
            cwd: Known::value(cwd.to_owned()),
            branch: Known::unknown(),
            started_at: Known::unknown(),
            ended_at: Known::unknown(),
            original_source_hash: crate::engine::sha256_hex(b"source"),
            original_source_bytes: 42,
        };
        let mut model = crate::engine::SessionModel::new(
            "session-1",
            provenance,
            CoverageReport::new(0, Vec::new(), Vec::new(), Vec::new(), status),
        );
        if turns > 0 {
            model.segments.push(crate::engine::Segment {
                segment_id: 0,
                cwd: Known::value(cwd.to_owned()),
                branch: Known::unknown(),
                started_at: Known::unknown(),
                ended_at: Known::unknown(),
                turn_range: crate::engine::TurnRange {
                    start: 0,
                    end: turns as u64 - 1,
                },
            });
        }
        for idx in 0..turns as u64 {
            let content_hash = crate::engine::sha256_hex(format!("raw-{idx}").as_bytes());
            let locator = format!("{:06}", idx + 1);
            let evidence = RawUnitRef {
                evidence_event_id: crate::engine::evidence_event_id_from_hash(
                    AgentKind::Codex,
                    "session-1",
                    &locator,
                    "message",
                    &content_hash,
                )
                .unwrap(),
                coverage_ordinal: idx + 1,
                physical_ordinal: idx + 1,
                locator,
                unit_kind: "message".to_owned(),
                artifact: "session.jsonl".to_owned(),
                content_hash,
                original_bytes: 4,
            };
            model.turns.push(Turn {
                turn_idx: idx,
                role: TurnRole::User,
                timestamp: Known::unknown(),
                kind: TurnKind::UserMsg,
                text: format!("turn {idx}"),
                text_hash: crate::engine::sha256_hex(format!("turn {idx}").as_bytes()),
                text_chars: format!("turn {idx}").chars().count() as u64,
                tool_name: Known::unknown(),
                segment_id: 0,
                raw_unit_refs: vec![evidence.clone()],
            });
            model.usage_events.push(UsageEvent {
                provider: "openai".to_owned(),
                model: Known::value("gpt-test".to_owned()),
                tokens: TokenComponents {
                    input: Known::value(1),
                    output: Known::unknown(),
                    reasoning: Known::unknown(),
                    cache_read: Known::unknown(),
                    cache_creation: Known::unknown(),
                },
                cost: Known::unknown(),
                timestamp: Known::unknown(),
                span: Known::unknown(),
                counter_semantics: CounterSemantics::Cumulative,
                evidence,
            });
        }
        model.coverage.raw_unit_count = turns as u64;
        model.coverage.raw_line_count = turns as u64;
        model.coverage.consumed_count = turns as u64;
        model.coverage.consumed = model
            .turns
            .iter()
            .enumerate()
            .map(|(idx, turn)| crate::engine::ConsumedUnit {
                ordinal: idx as u64 + 1,
                kind: "message".to_owned(),
                evidence: turn.raw_unit_refs[0].clone(),
            })
            .collect();
        model.coverage.consumed_ranges = if turns == 0 {
            Vec::new()
        } else {
            vec![crate::engine::OrdinalRange {
                start: 1,
                end: turns as u64,
            }]
        };
        match validate_parse(UnvalidatedParse::from_model(model)).unwrap() {
            crate::engine::ValidatedParse::Session(session) => *session,
            crate::engine::ValidatedParse::Fatal(_) => panic!("fixture unexpectedly fatal"),
        }
    }

    fn config() -> ProjectionConfig {
        ProjectionConfig {
            extraction_schema: "extract-v1".to_owned(),
            producer_version: "parser-v1".to_owned(),
            attribution_version: "attrib-v1".to_owned(),
            project_override: None,
        }
    }

    #[test]
    fn projects_evidence_complete_unique_cards_for_regression_volume() {
        let projection =
            project_validated_session(&validated(127, "/src/Loctree/aicx"), &config()).unwrap();
        assert_eq!(projection.cards.len(), 127);
        let ids: BTreeSet<_> = projection.cards.iter().map(|card| &card.id).collect();
        assert_eq!(ids.len(), 127);
        assert!(
            projection.cards.iter().all(
                |card| !card.evidence_event_ids.is_empty() && !card.usage_references.is_empty()
            )
        );
    }

    #[test]
    fn bucket_case_collisions_and_attribution_version_are_deterministic() {
        let model = validated(1, "/src/Loctree/aicx");
        let first = project_validated_session(&model, &config()).unwrap();
        let mut changed = config();
        changed.project_override = Some("LOCTREE/AICX".to_owned());
        changed.attribution_version = "attrib-v99".to_owned();
        let second = project_validated_session(&model, &changed).unwrap();
        assert_eq!(first.cards[0].project.slug, "loctree/aicx");
        assert_eq!(second.cards[0].project.slug, "loctree/aicx");
        assert_eq!(first.store_revision, second.store_revision);
        assert!(matches!(
            second.cards[0].project.attribution,
            super::super::ProjectAttribution::OperatorOverride { .. }
        ));
    }

    #[test]
    fn revision_changes_for_membership_content_schema_and_producer() {
        let one = project_validated_session(&validated(1, "/src/Loctree/aicx"), &config()).unwrap();
        let two = project_validated_session(&validated(2, "/src/Loctree/aicx"), &config()).unwrap();
        assert_ne!(one.store_revision, two.store_revision);
        let mut schema = config();
        schema.extraction_schema = "extract-v2".to_owned();
        let schema =
            project_validated_session(&validated(1, "/src/Loctree/aicx"), &schema).unwrap();
        assert_ne!(one.store_revision, schema.store_revision);
        let mut producer = config();
        producer.producer_version = "parser-v2".to_owned();
        let producer =
            project_validated_session(&validated(1, "/src/Loctree/aicx"), &producer).unwrap();
        assert_ne!(one.store_revision, producer.store_revision);
    }

    #[test]
    fn merged_cards_are_order_independent_and_reject_duplicates() {
        let mut cards = project_validated_session(&validated(2, "/src/Loctree/aicx"), &config())
            .unwrap()
            .cards;
        let forward = projection_from_cards(cards.clone(), &config()).unwrap();
        cards.reverse();
        let reverse = projection_from_cards(cards, &config()).unwrap();
        assert_eq!(forward, reverse);

        let duplicate = vec![forward.cards[0].clone(), forward.cards[0].clone()];
        assert!(projection_from_cards(duplicate, &config()).is_err());
    }
}
