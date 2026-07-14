use aicx_parser::projections::{CANONICAL_CARD_SCHEMA, CanonicalCard, CanonicalProjection};
use anyhow::{Context, Result, anyhow};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

pub const CANONICAL_PROJECTION_DIRNAME: &str = "canonical-projection-v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CanonicalStoreManifest {
    pub schema: String,
    pub card_schema: String,
    pub store_revision: String,
    pub extraction_schema: String,
    pub producer_version: String,
    pub card_ids: Vec<String>,
}

pub fn write_canonical_projection_at(
    root: &Path,
    projection: &CanonicalProjection,
) -> Result<PathBuf> {
    validate_projection(projection)?;
    fs::create_dir_all(root)?;
    let target = root.join(CANONICAL_PROJECTION_DIRNAME);
    let stage = root.join(format!(
        ".{CANONICAL_PROJECTION_DIRNAME}.stage-{}",
        std::process::id()
    ));
    if stage.exists() {
        fs::remove_dir_all(&stage)?;
    }
    fs::create_dir(&stage)?;
    let result = stage_projection(&stage, projection);
    if let Err(error) = result {
        let _ = fs::remove_dir_all(&stage);
        return Err(error);
    }
    let backup = root.join(format!(".{CANONICAL_PROJECTION_DIRNAME}.previous"));
    if backup.exists() {
        fs::remove_dir_all(&backup)?;
    }
    if target.exists() {
        fs::rename(&target, &backup).context("move previous canonical projection aside")?;
    }
    if let Err(error) = fs::rename(&stage, &target) {
        if backup.exists() {
            let _ = fs::rename(&backup, &target);
        }
        return Err(error).context("commit canonical projection atomically");
    }
    if backup.exists() {
        fs::remove_dir_all(backup)?;
    }
    Ok(target)
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

fn validate_projection(projection: &CanonicalProjection) -> Result<()> {
    if projection.schema != "aicx.store.canonical_projection.v1" {
        return Err(anyhow!("unsupported canonical projection schema"));
    }
    let mut ids = BTreeSet::new();
    for card in &projection.cards {
        if card.schema != CANONICAL_CARD_SCHEMA || !valid_card_id(&card.id) || !ids.insert(&card.id)
        {
            return Err(anyhow!(
                "invalid or duplicate canonical card id {}",
                card.id
            ));
        }
    }
    Ok(())
}

fn stage_projection(stage: &Path, projection: &CanonicalProjection) -> Result<()> {
    let cards_dir = stage.join("cards");
    fs::create_dir(&cards_dir)?;
    for card in &projection.cards {
        let bytes = serde_json::to_vec_pretty(card)?;
        super::atomic_write::atomic_write(&cards_dir.join(card_filename(&card.id)?), &bytes)?;
    }
    let manifest = CanonicalStoreManifest {
        schema: "aicx.store.manifest.v1".to_owned(),
        card_schema: CANONICAL_CARD_SCHEMA.to_owned(),
        store_revision: projection.store_revision.clone(),
        extraction_schema: projection.extraction_schema.clone(),
        producer_version: projection.producer_version.clone(),
        card_ids: projection
            .cards
            .iter()
            .map(|card| card.id.clone())
            .collect(),
    };
    super::atomic_write::atomic_write(
        &stage.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest)?,
    )?;
    Ok(())
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
    fn invalid_projection_does_not_mutate_existing_store() {
        let root = std::env::temp_dir().join(format!("aicx-c6-{}", std::process::id()));
        let target = root.join(CANONICAL_PROJECTION_DIRNAME);
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("sentinel"), b"old").unwrap();
        let invalid = CanonicalProjection {
            schema: "invalid".to_owned(),
            extraction_schema: "x".to_owned(),
            producer_version: "y".to_owned(),
            store_revision: "z".to_owned(),
            cards: Vec::new(),
        };
        assert!(write_canonical_projection_at(&root, &invalid).is_err());
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

    #[test]
    fn projection_commit_is_versioned_and_roundtrips() {
        let root = std::env::temp_dir().join(format!(
            "aicx-c6-roundtrip-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let projection = CanonicalProjection {
            schema: "aicx.store.canonical_projection.v1".to_owned(),
            extraction_schema: "extract-v1".to_owned(),
            producer_version: "parser-v1".to_owned(),
            store_revision: "sr1:empty".to_owned(),
            cards: Vec::new(),
        };
        let target = write_canonical_projection_at(&root, &projection).unwrap();
        assert_eq!(target.file_name().unwrap(), CANONICAL_PROJECTION_DIRNAME);
        let (manifest, cards) = read_canonical_projection_at(&root).unwrap().unwrap();
        assert_eq!(manifest.schema, "aicx.store.manifest.v1");
        assert_eq!(manifest.card_schema, CANONICAL_CARD_SCHEMA);
        assert_eq!(manifest.store_revision, "sr1:empty");
        assert!(cards.is_empty());
        let _ = fs::remove_dir_all(root);
    }
}
