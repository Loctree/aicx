//! Typed intent-to-structure overlay (`loctree.overlay.intent.v1`).
//!
//! This layer consumes only C6 canonical cards and their frozen
//! `evidence_event_id` references. It never opens agent sessions or rendered
//! conversation Markdown. Loctree owns structural identity; this module joins
//! distilled card claims to the catalog emitted by `loct anchors`.

use crate::rank::{SEMANTIC_INTENT_CANDIDATE_THRESHOLD, intent_candidate_similarity};
use crate::store::{canonical_store_dir, read_canonical_projection_at, resolve_aicx_home};
use aicx_parser::engine::{Known, TurnRole};
use aicx_parser::projections::CanonicalCard;
use anyhow::{Context, Result, anyhow, bail};
use regex::Regex;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

pub const OVERLAY_SCHEMA: &str = "loctree.overlay.intent.v1";
pub const OVERLAY_INDEX_SCHEMA: &str = "aicx.overlay.side_index.v1";
pub const ATTRIBUTION_VERSION: &str = "path-symbol-resolver.v2";
pub const DEDUP_VERSION: &str = "semantic-negation-veto.v1";
pub const EMBEDDING_MODEL: &str = "aicx-embeddings.configured.v1";
pub const ATTRIBUTION_THRESHOLD: f64 = 0.90;

#[derive(Debug, Clone)]
pub struct OverlayOptions {
    pub repo: PathBuf,
    pub rebuild: bool,
    pub loct_bin: Option<PathBuf>,
    pub store_root: Option<PathBuf>,
    pub index_root: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OverlayDocument {
    pub schema: String,
    pub repo_id: String,
    pub snapshot_commit: String,
    pub anchor_catalog_revision: String,
    pub store_revision: String,
    pub overlay_revision: String,
    pub producer_version: String,
    pub entries: Vec<OverlayEntry>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_attributions: Vec<UnresolvedAttribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OverlayEntry {
    pub intent_id: String,
    pub content_hash: String,
    pub target: OverlayTarget,
    pub thesis: String,
    pub status: String,
    pub authority: String,
    pub verification_status: String,
    pub valid_from: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub relations: Vec<OverlayRelation>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attributions: Vec<Attribution>,
    pub refs: Vec<OverlayRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OverlayTarget {
    Repo,
    Path {
        path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        language: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor_id: Option<String>,
    },
    Symbol {
        #[serde(skip_serializing_if = "Option::is_none")]
        path: Option<String>,
        language: String,
        qualified_symbol: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        signature_hash: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        anchor_id: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Attribution {
    pub target_anchor: String,
    pub relation: String,
    pub match_kind: String,
    pub confidence: f64,
    pub evidence_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UnresolvedAttribution {
    pub intent_id: String,
    pub target_anchor: String,
    pub relation: String,
    pub match_kind: String,
    pub confidence: f64,
    pub evidence_ref: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OverlayRef {
    pub evidence_event_id: String,
    #[serde(rename = "ref")]
    pub opaque_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OverlayRelation {
    pub kind: String,
    pub intent_id: String,
    pub evidence_ref: String,
    pub confidence: f64,
    pub observed_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct AnchorCatalog {
    repo_id: String,
    snapshot_commit: String,
    anchor_catalog_revision: String,
    producer_version: String,
    anchors: Vec<Anchor>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Anchor {
    anchor_id: String,
    normalized_path: String,
    language: String,
    #[serde(default)]
    qualified_symbol: Option<String>,
    #[serde(default)]
    signature_hash: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SideIndex {
    schema: String,
    repo_id: String,
    store_revision: String,
    #[serde(default)]
    embedding_model: String,
    entries: Vec<IndexedIntent>,
    #[serde(default)]
    groups: Vec<OverlayEntry>,
    #[serde(default)]
    unresolved_attributions: Vec<UnresolvedAttribution>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct IndexedIntent {
    intent_id: String,
    #[serde(default)]
    group_intent_id: String,
    evidence_event_id: String,
    #[serde(default)]
    claim_key: String,
    session_id: String,
    turn_idx: u64,
    thesis: String,
    valid_from: String,
    authority: String,
    #[serde(default)]
    embedding: Vec<f32>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayBuildStats {
    pub canonical_cards_seen: usize,
    pub new_intents: usize,
    pub retained_intents: usize,
    pub emitted_attributions: usize,
    pub unresolved_attributions: usize,
    pub files_opened: usize,
    pub raw_session_files_opened: usize,
}

pub fn build_overlay(options: &OverlayOptions) -> Result<(OverlayDocument, OverlayBuildStats)> {
    let started = Instant::now();
    let repo = options
        .repo
        .canonicalize()
        .with_context(|| format!("invalid overlay repository {}", options.repo.display()))?;
    if !repo.is_dir() {
        bail!("overlay repository is not a directory: {}", repo.display());
    }
    let catalog = load_anchor_catalog(&repo, options.loct_bin.as_deref())?;
    let store_root = options
        .store_root
        .clone()
        .unwrap_or(canonical_store_dir()?)
        .canonicalize()
        .context("canonical C6 store root is missing or unreadable")?;
    let projections = discover_canonical_projections(&store_root)?;
    if projections.is_empty() {
        bail!(
            "typed C6 canonical projection is unavailable under {}; run canonical ingest before overlay emission",
            store_root.display()
        );
    }
    let mut cards = Vec::new();
    let mut revisions = BTreeSet::new();
    let mut files_opened = 0usize;
    for root in projections {
        if let Some((manifest, projection_cards)) = read_canonical_projection_at(&root)? {
            files_opened += 1 + manifest.card_ids.len();
            let mut matching: Vec<_> = projection_cards
                .into_iter()
                .filter(|card| card_matches_repo(card, &catalog.repo_id, &repo))
                .collect();
            if !matching.is_empty() {
                revisions.insert(manifest.store_revision);
                cards.append(&mut matching);
            }
        }
    }
    if cards.is_empty() {
        bail!(
            "typed C6 store has no canonical cards for {} (raw-session fallback is forbidden)",
            catalog.repo_id
        );
    }
    cards.sort_by(|left, right| left.id.cmp(&right.id));
    cards.dedup_by(|left, right| left.id == right.id);
    let store_revision = combined_store_revision(&revisions)?;
    let embedding_model = configured_embedding_model_key();
    let overlay_revision = overlay_revision(&catalog, &store_revision, &embedding_model);
    let index_root = options.index_root.clone().unwrap_or(
        resolve_aicx_home()?
            .join("overlay-index-v1")
            .join(short_hash(&catalog.repo_id)),
    );
    fs::create_dir_all(&index_root)?;
    let index_root = index_root
        .canonicalize()
        .context("overlay index root is unreadable after creation")?;
    let output_path = index_root.join(format!("{overlay_revision}.json"));
    if !options.rebuild && output_path.exists() {
        // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path -- output_path is a canonicalized index root plus a SHA-256-derived filename controlled by this module.
        let output: OverlayDocument = serde_json::from_slice(&fs::read(&output_path)?)?;
        return Ok((
            output,
            OverlayBuildStats {
                canonical_cards_seen: cards.len(),
                files_opened: files_opened + 2,
                ..OverlayBuildStats::default()
            },
        ));
    }
    let side_index_path = index_root.join("side-index.json");
    let previous = read_side_index(&side_index_path, &catalog.repo_id)?;
    let (mut index, new_intents, retained_intents) = update_side_index(
        previous,
        &cards,
        &catalog.repo_id,
        &store_revision,
        &embedding_model,
        options.rebuild,
    )?;
    materialize_side_index(&mut index, &catalog, &repo)?;
    atomic_write_json(&side_index_path, &index)?;
    let entries = index.groups.clone();
    let unresolved = index.unresolved_attributions.clone();
    let emitted_attributions = entries.iter().map(|entry| entry.attributions.len()).sum();
    let stats = OverlayBuildStats {
        canonical_cards_seen: cards.len(),
        new_intents,
        retained_intents,
        emitted_attributions,
        unresolved_attributions: unresolved.len(),
        files_opened: files_opened + usize::from(side_index_path.exists()) + 1,
        raw_session_files_opened: 0,
    };
    let output = OverlayDocument {
        schema: OVERLAY_SCHEMA.to_owned(),
        repo_id: catalog.repo_id.clone(),
        snapshot_commit: catalog.snapshot_commit.clone(),
        anchor_catalog_revision: catalog.anchor_catalog_revision.clone(),
        store_revision,
        overlay_revision,
        producer_version: env!("CARGO_PKG_VERSION").to_owned(),
        entries,
        unresolved_attributions: unresolved,
    };
    atomic_write_json(&output_path, &output)?;
    tracing::debug!(
        elapsed_ms = started.elapsed().as_millis(),
        ?stats,
        "overlay built"
    );
    Ok((output, stats))
}

fn load_anchor_catalog(repo: &Path, configured: Option<&Path>) -> Result<AnchorCatalog> {
    let repo_local = repo.join("target/release/loct");
    let binary = configured
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("LOCT_BIN").map(PathBuf::from))
        .or_else(|| repo_local.is_file().then_some(repo_local))
        .unwrap_or_else(|| PathBuf::from("loct"));
    let binary = if binary == Path::new("loct") {
        binary
    } else {
        let binary = binary.canonicalize().with_context(|| {
            format!(
                "configured loct binary does not exist: {}",
                binary.display()
            )
        })?;
        if !binary.is_file() {
            bail!("configured loct binary is not a file: {}", binary.display());
        }
        binary
    };
    // nosemgrep: rust.actix.command-injection.rust-actix-command-injection.rust-actix-command-injection -- std::process::Command does not invoke a shell; the only bare program accepted is `loct`, explicit paths are canonicalized files, and all arguments are constants.
    let output = Command::new(&binary)
        .args(["anchors", "--format", "json"])
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed to execute {} anchors", binary.display()))?;
    if !output.status.success() {
        bail!(
            "loct anchors failed for {}: {}",
            repo.display(),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let catalog: AnchorCatalog = serde_json::from_slice(&output.stdout)
        .context("loct anchors returned an invalid catalog")?;
    if catalog.anchors.is_empty() || !catalog.anchor_catalog_revision.starts_with("acr1:") {
        bail!("loct anchors returned an empty or unversioned catalog");
    }
    Ok(catalog)
}

fn discover_canonical_projections(root: &Path) -> Result<Vec<PathBuf>> {
    let mut found = Vec::new();
    discover_projection_roots(root, 0, &mut found)?;
    found.sort();
    found.dedup();
    Ok(found)
}

fn discover_projection_roots(root: &Path, depth: usize, found: &mut Vec<PathBuf>) -> Result<()> {
    if depth > 6 || !root.is_dir() {
        return Ok(());
    }
    if root.join("canonical-projection-v1/manifest.json").is_file() {
        found.push(root.to_path_buf());
        return Ok(());
    }
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path -- root begins at a canonicalized operator-owned C6 store; recursion follows only real directories (not symlinks), is depth-bounded, and appends no user-provided path component.
    let mut children = fs::read_dir(root)
        .with_context(|| format!("read canonical store directory {}", root.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(|entry| entry.file_name());
    if children.len() > 10_000 {
        bail!(
            "canonical store directory exceeds bounded scan: {}",
            root.display()
        );
    }
    for entry in children {
        if entry.file_type()?.is_dir() {
            discover_projection_roots(&entry.path(), depth + 1, found)?;
        }
    }
    Ok(())
}

fn card_matches_repo(card: &CanonicalCard, repo_id: &str, repo: &Path) -> bool {
    let repo_id = repo_id.to_ascii_lowercase();
    let slug = card.project.slug.to_ascii_lowercase();
    let repo_name = repo
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    slug == repo_id || slug == repo_name || repo_id.ends_with(&format!("/{slug}"))
}

fn combined_store_revision(revisions: &BTreeSet<String>) -> Result<String> {
    if revisions.is_empty() {
        bail!("canonical store manifests did not expose store_revision");
    }
    if revisions.len() != 1 {
        bail!(
            "target repo spans {} distinct C6 store revisions; rebuild the canonical projection instead of synthesizing a downstream revision",
            revisions.len()
        );
    }
    Ok(revisions.iter().next().cloned().unwrap_or_default())
}

fn overlay_revision(
    catalog: &AnchorCatalog,
    store_revision: &str,
    embedding_model: &str,
) -> String {
    overlay_revision_with_attribution(
        catalog,
        store_revision,
        ATTRIBUTION_VERSION,
        embedding_model,
    )
}

fn overlay_revision_with_attribution(
    catalog: &AnchorCatalog,
    store_revision: &str,
    attribution_version: &str,
    embedding_model: &str,
) -> String {
    let material = [
        catalog.repo_id.as_str(),
        store_revision,
        catalog.snapshot_commit.as_str(),
        catalog.anchor_catalog_revision.as_str(),
        OVERLAY_SCHEMA,
        attribution_version,
        DEDUP_VERSION,
        EMBEDDING_MODEL,
        embedding_model,
        &format!("{SEMANTIC_INTENT_CANDIDATE_THRESHOLD:.2}"),
        "0.90",
    ]
    .join("\0");
    format!("ov1:{}", hex::encode(Sha256::digest(material.as_bytes())))
}

#[cfg(test)]
fn configured_embedding_model_key() -> String {
    // The test embedder is a deterministic offline seam. Do not let unrelated
    // parallel tests which mutate embedding env/config change cache identity.
    "test:frozen-semantic-fixture.v1".to_owned()
}

#[cfg(all(
    not(test),
    any(feature = "native-embedder", feature = "cloud-embedder")
))]
fn configured_embedding_model_key() -> String {
    let config = crate::embedder::EmbeddingConfig::from_env();
    if config.backend.as_str() == "cloud" {
        let cloud_model = config
            .cloud
            .as_ref()
            .map(|cloud| cloud.model.as_str())
            .unwrap_or("unconfigured");
        return format!("cloud:{cloud_model}");
    }
    let resolved = config.resolved_model();
    format!(
        "{}:{}:{}",
        config.backend.as_str(),
        resolved.repo,
        resolved.filename
    )
}

#[cfg(all(
    not(test),
    not(any(feature = "native-embedder", feature = "cloud-embedder"))
))]
fn configured_embedding_model_key() -> String {
    "unavailable:no-embedder-feature".to_owned()
}

fn read_side_index(path: &Path, repo_id: &str) -> Result<Option<SideIndex>> {
    if !path.exists() {
        return Ok(None);
    }
    // nosemgrep: rust.actix.path-traversal.tainted-path.tainted-path -- caller supplies a fixed `side-index.json` child of the canonicalized overlay index root.
    let index: SideIndex = serde_json::from_slice(&fs::read(path)?)?;
    if index.schema != OVERLAY_INDEX_SCHEMA || index.repo_id != repo_id {
        return Ok(None);
    }
    Ok(Some(index))
}

fn update_side_index(
    previous: Option<SideIndex>,
    cards: &[CanonicalCard],
    repo_id: &str,
    store_revision: &str,
    embedding_model: &str,
    rebuild: bool,
) -> Result<(SideIndex, usize, usize)> {
    let mut previous = previous;
    if previous
        .as_ref()
        .is_some_and(|index| index.embedding_model != embedding_model)
        && let Some(index) = &mut previous
    {
        for entry in &mut index.entries {
            entry.embedding.clear();
            entry.group_intent_id.clear();
        }
        index.groups.clear();
        index.unresolved_attributions.clear();
    }
    let mut existing_by_claim: BTreeMap<String, IndexedIntent> = previous
        .map(|index| {
            index
                .entries
                .into_iter()
                .map(|entry| {
                    let key = claim_key(&entry.evidence_event_id, &entry.thesis);
                    (key, entry)
                })
                .collect()
        })
        .unwrap_or_default();
    if rebuild {
        let current: BTreeSet<_> = cards
            .iter()
            .filter_map(|card| {
                card.evidence_event_ids
                    .first()
                    .map(|evidence| (evidence, distill_theses(&card.frame.text)))
            })
            .flat_map(|(evidence, theses)| {
                theses
                    .into_iter()
                    .map(|thesis| claim_key(evidence, &thesis))
            })
            .collect();
        existing_by_claim.retain(|key, _| current.contains(key));
    }
    let retained = existing_by_claim.len();
    let mut new_intents = 0usize;
    for card in cards {
        let Some(evidence_event_id) = card.evidence_event_ids.first() else {
            continue;
        };
        for thesis in distill_theses(&card.frame.text) {
            let key = claim_key(evidence_event_id, &thesis);
            if existing_by_claim.contains_key(&key) {
                continue;
            }
            let intent_id = random_intent_id()?;
            existing_by_claim.insert(
                key.clone(),
                IndexedIntent {
                    intent_id,
                    group_intent_id: String::new(),
                    evidence_event_id: evidence_event_id.clone(),
                    claim_key: key,
                    session_id: safe_token(&card.session_id),
                    turn_idx: card.frame.turn_idx,
                    thesis,
                    valid_from: card_timestamp(card),
                    // User/operator cards are authority; other typed cards remain derived.
                    authority: if card.frame.role == TurnRole::User {
                        "operator_confirmed"
                    } else {
                        "agent_derived"
                    }
                    .to_owned(),
                    embedding: Vec::new(),
                },
            );
            new_intents += 1;
        }
    }
    let mut entries: Vec<_> = existing_by_claim.into_values().collect();
    entries.sort_by(|left, right| left.intent_id.cmp(&right.intent_id));
    Ok((
        SideIndex {
            schema: OVERLAY_INDEX_SCHEMA.to_owned(),
            repo_id: repo_id.to_owned(),
            store_revision: store_revision.to_owned(),
            embedding_model: embedding_model.to_owned(),
            entries,
            groups: Vec::new(),
            unresolved_attributions: Vec::new(),
        },
        new_intents,
        retained,
    ))
}

fn distill_theses(text: &str) -> Vec<String> {
    // Contract filter: dispatch templates and runtime plumbing are not product intent.
    const BOILERPLATE: &[&str] = &[
        "VIBECRAFTED_",
        "vibecrafted_report_path",
        "you are running under",
        "<INSTRUCTIONS>",
        "background-task completions",
    ];
    let report_line = Regex::new(r"^\d+\s+").expect("static regex");
    let path = Regex::new(r"[A-Za-z0-9._@+-]+(?:/[A-Za-z0-9._@+*?-]+)+").expect("static regex");
    let quoted_filename = Regex::new(r"`[^`/\s]+\.[A-Za-z0-9]+`").expect("static regex");
    let diffstat = Regex::new(r"\|\s*\d+\s+[+-]+$").expect("static regex");
    let mut theses = BTreeSet::new();
    for line in text.lines() {
        let numbered = report_line.replace(line.trim(), "");
        let trimmed = numbered
            .trim()
            .trim_start_matches(['-', '*', '#', '>', ' '])
            .trim();
        let lower = trimmed.to_ascii_lowercase();
        if trimmed.is_empty()
            || BOILERPLATE
                .iter()
                .any(|marker| lower.contains(&marker.to_ascii_lowercase()))
            || lower.starts_with("task:")
            || lower.starts_with("todo:")
            || trimmed.starts_with("?? ")
            || trimmed.starts_with('|')
            || trimmed.starts_with("```")
            || diffstat.is_match(trimmed)
        {
            continue;
        }
        let explicit = ["decision:", "intent:", "why:", "decyzja:", "intencja:"]
            .iter()
            .find_map(|prefix| {
                lower
                    .starts_with(prefix)
                    .then(|| trimmed[prefix.len()..].trim())
            });
        let normative = ["must ", "should ", "musimy ", "należy ", "zakaz "]
            .iter()
            .any(|marker| lower.starts_with(marker));
        // Canonical report cards often carry several independently evidenced
        // decisions. A path-bearing line is one typed claim, not boilerplate;
        // keeping each line separate lets one evidence event own stable intent
        // clusters without reparsing the rendered conversation corpus.
        let preferred = explicit.or_else(|| {
            (normative || path.is_match(trimmed) || quoted_filename.is_match(trimmed))
                .then_some(trimmed)
        });
        let Some(preferred) = preferred else {
            continue;
        };
        let thesis = preferred.split_whitespace().collect::<Vec<_>>().join(" ");
        let thesis = truncate_chars(&thesis, 200);
        if !thesis.is_empty() {
            theses.insert(thesis);
        }
    }
    theses.into_iter().collect()
}

#[cfg(test)]
fn distill_thesis(text: &str) -> Option<String> {
    distill_theses(text).into_iter().next()
}

fn claim_key(evidence_event_id: &str, thesis: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(evidence_event_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(thesis.as_bytes());
    format!("claim2:{}", hex::encode(hasher.finalize()))
}

fn materialize_side_index(
    index: &mut SideIndex,
    catalog: &AnchorCatalog,
    repo: &Path,
) -> Result<()> {
    let (entries, unresolved) = resolve_entries(&index.entries, catalog, repo);
    // Attribution is the precision gate for overlay emission. Embed only the
    // typed claims which survived it: unresolved claims cannot participate in
    // a same-target semantic group, and eagerly embedding them makes a cold
    // side-index rebuild scale with discarded payload rather than output.
    let emitted_ids: BTreeSet<_> = entries
        .iter()
        .map(|entry| entry.intent_id.clone())
        .collect();
    ensure_embeddings(&mut index.entries, &emitted_ids)?;
    let embeddings_by_id: BTreeMap<_, _> = index
        .entries
        .iter()
        .map(|entry| (entry.intent_id.clone(), entry.embedding.clone()))
        .collect();
    let established_groups: BTreeMap<_, _> = index
        .entries
        .iter()
        .filter(|entry| !entry.group_intent_id.is_empty())
        .map(|entry| (entry.intent_id.clone(), entry.group_intent_id.clone()))
        .collect();
    let embeddings = entries
        .iter()
        .map(|entry| {
            embeddings_by_id
                .get(&entry.intent_id)
                .cloned()
                .ok_or_else(|| anyhow!("missing side-index embedding for {}", entry.intent_id))
        })
        .collect::<Result<Vec<_>>>()?;
    let (groups, membership) = consolidate_entries(entries, embeddings, &established_groups)?;
    for entry in &mut index.entries {
        if let Some(group_id) = membership.get(&entry.intent_id) {
            entry.group_intent_id.clone_from(group_id);
        }
    }
    index.groups = groups;
    index.unresolved_attributions = unresolved;
    Ok(())
}

fn ensure_embeddings(entries: &mut [IndexedIntent], emitted_ids: &BTreeSet<String>) -> Result<()> {
    let missing: Vec<_> = entries
        .iter()
        .enumerate()
        .filter(|(_, entry)| {
            emitted_ids.contains(entry.intent_id.as_str()) && entry.embedding.is_empty()
        })
        .map(|(index, entry)| (index, entry.thesis.clone()))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let texts: Vec<_> = missing.iter().map(|(_, text)| text.clone()).collect();
    let vectors = embed_intent_batch(&texts)?;
    if vectors.len() != missing.len() {
        bail!(
            "semantic embedder returned {} vectors for {} intent theses",
            vectors.len(),
            missing.len()
        );
    }
    for ((entry_index, _), vector) in missing.into_iter().zip(vectors) {
        if vector.is_empty() {
            bail!("semantic embedder returned an empty intent vector");
        }
        entries[entry_index].embedding = vector;
    }
    Ok(())
}

#[cfg(test)]
fn embed_intent_batch(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    // Unit/e2e overlay tests must not depend on a live model server. This
    // identity-only seam deliberately creates no semantic candidates; focused
    // fixture tests below inject frozen vectors into `consolidate_entries`.
    Ok(texts
        .iter()
        .map(|text| {
            let digest = Sha256::digest(text.as_bytes());
            digest
                .iter()
                .map(|byte| (*byte as f32 / 127.5) - 1.0)
                .collect()
        })
        .collect())
}

#[cfg(all(
    not(test),
    any(feature = "native-embedder", feature = "cloud-embedder")
))]
fn embed_intent_batch(texts: &[String]) -> Result<Vec<Vec<f32>>> {
    let mut engine = crate::embedder::EmbeddingEngine::new()
        .context("initialize semantic embedder for overlay side-index")?;
    let mut vectors = Vec::with_capacity(texts.len());
    for chunk in texts.chunks(64) {
        vectors.extend(
            engine
                .embed_batch(chunk)
                .context("embed overlay intent candidate batch")?,
        );
    }
    Ok(vectors)
}

#[cfg(all(
    not(test),
    not(any(feature = "native-embedder", feature = "cloud-embedder"))
))]
fn embed_intent_batch(_texts: &[String]) -> Result<Vec<Vec<f32>>> {
    bail!("overlay semantic dedup requires feature `native-embedder` or `cloud-embedder`")
}

fn resolve_entries(
    intents: &[IndexedIntent],
    catalog: &AnchorCatalog,
    repo: &Path,
) -> (Vec<OverlayEntry>, Vec<UnresolvedAttribution>) {
    let mut entries = Vec::new();
    let mut unresolved = Vec::new();
    for intent in intents {
        let evidence_ref = intent.evidence_event_id.clone();
        let candidates = attribution_candidates(&intent.thesis, &catalog.anchors, repo);
        let mut accepted = Vec::new();
        let mut target = OverlayTarget::Repo;
        for candidate in candidates {
            if candidate.confidence >= ATTRIBUTION_THRESHOLD {
                if accepted.is_empty() {
                    target = target_from_anchor(candidate.anchor);
                }
                accepted.push(Attribution {
                    target_anchor: candidate.anchor.anchor_id.clone(),
                    relation: relation_for(&intent.thesis).to_owned(),
                    match_kind: candidate.match_kind.to_owned(),
                    confidence: candidate.confidence,
                    evidence_ref: evidence_ref.clone(),
                });
            } else {
                unresolved.push(UnresolvedAttribution {
                    intent_id: intent.intent_id.clone(),
                    target_anchor: candidate.anchor.anchor_id.clone(),
                    relation: relation_for(&intent.thesis).to_owned(),
                    match_kind: candidate.match_kind.to_owned(),
                    confidence: candidate.confidence,
                    evidence_ref: evidence_ref.clone(),
                    reason: "candidate below attribution threshold; payload-only abstention"
                        .to_owned(),
                });
            }
        }
        accepted.sort_by(|left, right| left.target_anchor.cmp(&right.target_anchor));
        accepted.dedup_by(|left, right| left.target_anchor == right.target_anchor);
        // Unresolved candidates are payload-only by contract. A thesis becomes
        // an overlay entry only after at least one target crosses the truth
        // threshold; otherwise consumers would receive force-fed repo truth.
        if accepted.is_empty() {
            continue;
        }
        let refs = vec![OverlayRef {
            evidence_event_id: intent.evidence_event_id.clone(),
            opaque_ref: format!("session:{}#turn-{}", intent.session_id, intent.turn_idx),
        }];
        let content_hash = content_hash(&intent.thesis, "current", &[], &refs);
        entries.push(OverlayEntry {
            intent_id: intent.intent_id.clone(),
            content_hash,
            target,
            thesis: intent.thesis.clone(),
            status: "current".to_owned(),
            authority: intent.authority.clone(),
            verification_status: "unverified".to_owned(),
            valid_from: intent.valid_from.clone(),
            valid_to: None,
            relations: Vec::new(),
            attributions: accepted,
            refs,
        });
    }
    entries.sort_by(|left, right| left.intent_id.cmp(&right.intent_id));
    unresolved.sort_by(|left, right| {
        (&left.intent_id, &left.target_anchor).cmp(&(&right.intent_id, &right.target_anchor))
    });
    (entries, unresolved)
}

fn consolidate_entries(
    entries: Vec<OverlayEntry>,
    embeddings: Vec<Vec<f32>>,
    established_groups: &BTreeMap<String, String>,
) -> Result<(Vec<OverlayEntry>, BTreeMap<String, String>)> {
    if entries.len() != embeddings.len() {
        bail!("overlay entries and semantic vectors have different lengths");
    }
    let mut by_target: BTreeMap<OverlayTarget, Vec<usize>> = BTreeMap::new();
    for (index, entry) in entries.iter().enumerate() {
        by_target
            .entry(entry.target.clone())
            .or_default()
            .push(index);
    }

    let mut merge_edges = Vec::new();
    let mut veto_edges = Vec::new();
    for indices in by_target.values() {
        for (offset, &left) in indices.iter().enumerate() {
            for &right in &indices[offset + 1..] {
                let similarity = intent_candidate_similarity(&embeddings[left], &embeddings[right]);
                if similarity < SEMANTIC_INTENT_CANDIDATE_THRESHOLD {
                    continue;
                }
                if negation_veto(&entries[left].thesis, &entries[right].thesis) {
                    veto_edges.push((left, right, similarity));
                } else {
                    merge_edges.push((left, right, similarity));
                }
            }
        }
    }
    merge_edges.sort_by(|left, right| right.2.total_cmp(&left.2));

    let mut union = UnionFind::new(entries.len());
    for (left, right, _) in merge_edges {
        let left_root = union.find(left);
        let right_root = union.find(right);
        if left_root == right_root {
            continue;
        }
        let left_members = union.members(left_root);
        let right_members = union.members(right_root);
        let crosses_veto = veto_edges.iter().any(|(a, b, _)| {
            (left_members.contains(a) && right_members.contains(b))
                || (left_members.contains(b) && right_members.contains(a))
        });
        if !crosses_veto {
            union.union(left_root, right_root);
        }
    }

    let mut components: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
    for index in 0..entries.len() {
        components.entry(union.find(index)).or_default().push(index);
    }
    let mut groups = Vec::with_capacity(components.len());
    let mut membership = BTreeMap::new();
    for members in components.into_values() {
        let mut chronological = members.clone();
        chronological.sort_by(|&left, &right| {
            entries[left]
                .valid_from
                .cmp(&entries[right].valid_from)
                .then_with(|| entries[left].intent_id.cmp(&entries[right].intent_id))
        });
        let representative = chronological[0];
        let established = chronological.iter().find_map(|index| {
            established_groups
                .get(&entries[*index].intent_id)
                .filter(|group_id| !group_id.is_empty())
                .cloned()
        });
        let group_id = established.unwrap_or_else(|| entries[representative].intent_id.clone());
        let mut refs = Vec::new();
        let mut attributions = Vec::new();
        let mut authority = "agent_derived".to_owned();
        for &member in &members {
            refs.extend(entries[member].refs.clone());
            attributions.extend(entries[member].attributions.clone());
            if entries[member].authority == "operator_confirmed" {
                authority = "operator_confirmed".to_owned();
            }
            membership.insert(entries[member].intent_id.clone(), group_id.clone());
        }
        refs.sort_by(|left, right| {
            (&left.evidence_event_id, &left.opaque_ref)
                .cmp(&(&right.evidence_event_id, &right.opaque_ref))
        });
        refs.dedup_by(|left, right| left == right);
        attributions.sort_by(|left, right| {
            (&left.target_anchor, &left.evidence_ref)
                .cmp(&(&right.target_anchor, &right.evidence_ref))
        });
        attributions.dedup_by(|left, right| left == right);
        let thesis = entries[representative].thesis.clone();
        groups.push(OverlayEntry {
            intent_id: group_id,
            content_hash: content_hash(&thesis, "current", &[], &refs),
            target: entries[representative].target.clone(),
            thesis,
            status: "current".to_owned(),
            authority,
            verification_status: entries[representative].verification_status.clone(),
            valid_from: entries[representative].valid_from.clone(),
            valid_to: None,
            relations: Vec::new(),
            attributions,
            refs,
        });
    }

    apply_veto_relations(&entries, &veto_edges, &membership, &mut groups);
    for group in &mut groups {
        group.content_hash =
            content_hash(&group.thesis, &group.status, &group.relations, &group.refs);
    }
    groups.sort_by(|left, right| {
        left.target
            .cmp(&right.target)
            .then_with(|| status_rank(&left.status).cmp(&status_rank(&right.status)))
            .then_with(|| right.valid_from.cmp(&left.valid_from))
            .then_with(|| left.intent_id.cmp(&right.intent_id))
    });
    Ok((groups, membership))
}

fn apply_veto_relations(
    entries: &[OverlayEntry],
    veto_edges: &[(usize, usize, f32)],
    membership: &BTreeMap<String, String>,
    groups: &mut [OverlayEntry],
) {
    let group_positions: BTreeMap<_, _> = groups
        .iter()
        .enumerate()
        .map(|(index, group)| (group.intent_id.clone(), index))
        .collect();
    let mut direct_successors: BTreeMap<String, (String, f32, String, String)> = BTreeMap::new();
    let mut disputes = Vec::new();
    for &(left, right, similarity) in veto_edges {
        let left_group = membership[&entries[left].intent_id].clone();
        let right_group = membership[&entries[right].intent_id].clone();
        if left_group == right_group {
            continue;
        }
        match entries[left].valid_from.cmp(&entries[right].valid_from) {
            std::cmp::Ordering::Less => {
                record_successor(
                    &mut direct_successors,
                    &left_group,
                    &right_group,
                    similarity,
                    &entries[right],
                );
            }
            std::cmp::Ordering::Greater => {
                record_successor(
                    &mut direct_successors,
                    &right_group,
                    &left_group,
                    similarity,
                    &entries[left],
                );
            }
            std::cmp::Ordering::Equal => {
                disputes.push((
                    left_group,
                    right_group,
                    similarity,
                    entries[left].valid_from.clone(),
                ));
            }
        }
    }
    for (older_id, (newer_id, similarity, evidence_ref, observed_at)) in direct_successors {
        let Some(&older) = group_positions.get(&older_id) else {
            continue;
        };
        let Some(&newer) = group_positions.get(&newer_id) else {
            continue;
        };
        groups[older].status = "superseded".to_owned();
        groups[older].valid_to = Some(observed_at.clone());
        push_relation(
            &mut groups[older].relations,
            OverlayRelation {
                kind: "superseded_by".to_owned(),
                intent_id: newer_id.clone(),
                evidence_ref: evidence_ref.clone(),
                confidence: similarity as f64,
                observed_at: observed_at.clone(),
            },
        );
        push_relation(
            &mut groups[newer].relations,
            OverlayRelation {
                kind: "supersedes".to_owned(),
                intent_id: older_id,
                evidence_ref,
                confidence: similarity as f64,
                observed_at,
            },
        );
    }
    for (left_id, right_id, similarity, observed_at) in disputes {
        let evidence_ref = group_positions
            .get(&right_id)
            .and_then(|&index| groups[index].refs.first())
            .map(|reference| reference.evidence_event_id.clone())
            .unwrap_or_default();
        for (source, target) in [(&left_id, &right_id), (&right_id, &left_id)] {
            if let Some(&position) = group_positions.get(source) {
                push_relation(
                    &mut groups[position].relations,
                    OverlayRelation {
                        kind: "disputes".to_owned(),
                        intent_id: target.clone(),
                        evidence_ref: evidence_ref.clone(),
                        confidence: similarity as f64,
                        observed_at: observed_at.clone(),
                    },
                );
            }
        }
    }
}

fn record_successor(
    successors: &mut BTreeMap<String, (String, f32, String, String)>,
    older_id: &str,
    newer_id: &str,
    similarity: f32,
    newer: &OverlayEntry,
) {
    let evidence_ref = newer
        .refs
        .first()
        .map(|reference| reference.evidence_event_id.clone())
        .unwrap_or_default();
    let candidate = (
        newer_id.to_owned(),
        similarity,
        evidence_ref,
        newer.valid_from.clone(),
    );
    successors
        .entry(older_id.to_owned())
        .and_modify(|current| {
            if candidate.3 < current.3 {
                *current = candidate.clone();
            }
        })
        .or_insert(candidate);
}

fn push_relation(relations: &mut Vec<OverlayRelation>, relation: OverlayRelation) {
    if !relations
        .iter()
        .any(|existing| existing.kind == relation.kind && existing.intent_id == relation.intent_id)
    {
        relations.push(relation);
        relations.sort_by(|left, right| {
            (&left.kind, &left.intent_id).cmp(&(&right.kind, &right.intent_id))
        });
    }
}

fn status_rank(status: &str) -> u8 {
    u8::from(status != "current")
}

fn negation_veto(left: &str, right: &str) -> bool {
    // Contract markers are deliberately strong and pair-relative. A marker in
    // both theses represents the same negative polarity and does not veto;
    // exactly one side carrying reversal/negation blocks semantic merging.
    const REVERSAL_MARKERS: &[&str] = &[
        " disabled",
        " rejected",
        " reverted",
        " withdrawn",
        " no longer",
        " do not ",
        " don't ",
        " never ",
        " instead of ",
        " wyłączon",
        " odrzucon",
        " cofnięt",
        " wycofan",
        " nie robimy",
        " nie używ",
        " już nie",
        " nigdy ",
        " zamiast ",
    ];
    let left = format!(" {} ", left.to_lowercase());
    let right = format!(" {} ", right.to_lowercase());
    let left_negative = REVERSAL_MARKERS.iter().any(|marker| left.contains(marker));
    let right_negative = REVERSAL_MARKERS.iter().any(|marker| right.contains(marker));
    left_negative ^ right_negative
}

struct UnionFind {
    parent: Vec<usize>,
}

impl UnionFind {
    fn new(len: usize) -> Self {
        Self {
            parent: (0..len).collect(),
        }
    }

    fn find(&mut self, index: usize) -> usize {
        if self.parent[index] != index {
            self.parent[index] = self.find(self.parent[index]);
        }
        self.parent[index]
    }

    fn union(&mut self, left: usize, right: usize) {
        let left = self.find(left);
        let right = self.find(right);
        if left != right {
            self.parent[right] = left;
        }
    }

    fn members(&mut self, root: usize) -> BTreeSet<usize> {
        (0..self.parent.len())
            .filter(|&index| self.find(index) == root)
            .collect()
    }
}

struct Candidate<'a> {
    anchor: &'a Anchor,
    match_kind: &'static str,
    confidence: f64,
}

fn attribution_candidates<'a>(
    text: &str,
    anchors: &'a [Anchor],
    repo: &Path,
) -> Vec<Candidate<'a>> {
    let normalized = text.replace('\\', "/");
    let lower = normalized.to_ascii_lowercase();
    let mut exact_paths = BTreeSet::new();
    let quoted_filename_re = Regex::new(r"`([^`/\s]+\.[A-Za-z0-9]+)`").expect("static regex");
    let quoted_filenames: BTreeSet<_> = quoted_filename_re
        .captures_iter(&normalized)
        .filter_map(|capture| capture.get(1))
        .map(|filename| filename.as_str().to_ascii_lowercase())
        .collect();
    let path_re = Regex::new(r"[A-Za-z0-9._@+-]+(?:/[A-Za-z0-9._@+-]+)+").expect("static regex");
    let mut saw_path = false;
    for capture in path_re.find_iter(&normalized) {
        saw_path = true;
        let path = capture
            .as_str()
            .trim_matches(|ch: char| ",;:()[]{}'\"`".contains(ch));
        if repo.join(path).is_file() {
            exact_paths.insert(path.to_ascii_lowercase());
        }
    }
    // Bucket-leak contract: when a card names a path but none of those paths
    // exist in the target repo, do not let a coincidental symbol/filename hit
    // smuggle cross-repo evidence into this overlay.
    if saw_path && exact_paths.is_empty() {
        return Vec::new();
    }
    let mut candidates = Vec::new();
    for anchor in anchors {
        let path_lower = anchor.normalized_path.to_ascii_lowercase();
        if !exact_paths.is_empty() && !exact_paths.contains(&path_lower) {
            continue;
        }
        let symbol_match = anchor
            .qualified_symbol
            .as_ref()
            .is_some_and(|symbol| contains_token(&lower, &symbol.to_ascii_lowercase()));
        if exact_paths.contains(&path_lower) && symbol_match {
            candidates.push(Candidate {
                anchor,
                match_kind: "qualified_symbol",
                confidence: 0.98,
            });
        } else if exact_paths.contains(&path_lower) && anchor.qualified_symbol.is_none() {
            candidates.push(Candidate {
                anchor,
                match_kind: "exact_path",
                confidence: 0.96,
            });
        } else if symbol_match {
            candidates.push(Candidate {
                anchor,
                match_kind: "qualified_symbol",
                // A bare symbol mention without a corroborating path is a
                // candidate, not truth. Keep it payload-only until a future
                // semantic resolver can prove the join.
                confidence: 0.88,
            });
        } else if anchor.qualified_symbol.is_none()
            && Path::new(&anchor.normalized_path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    let name = name.to_ascii_lowercase();
                    quoted_filenames.contains(&name)
                        && anchors
                            .iter()
                            .filter(|candidate| {
                                candidate.qualified_symbol.is_none()
                                    && Path::new(&candidate.normalized_path)
                                        .file_name()
                                        .and_then(|filename| filename.to_str())
                                        .is_some_and(|filename| {
                                            filename.eq_ignore_ascii_case(&name)
                                        })
                            })
                            .count()
                            == 1
                })
        {
            candidates.push(Candidate {
                anchor,
                match_kind: "filename_lexical",
                confidence: 0.92,
            });
        } else if anchor.qualified_symbol.is_none()
            && Path::new(&anchor.normalized_path)
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| contains_token(&lower, &name.to_ascii_lowercase()))
        {
            candidates.push(Candidate {
                anchor,
                match_kind: "filename_lexical",
                confidence: 0.72,
            });
        }
    }
    candidates.sort_by(|left, right| {
        right
            .confidence
            .total_cmp(&left.confidence)
            .then_with(|| left.anchor.anchor_id.cmp(&right.anchor.anchor_id))
    });
    candidates
}

fn relation_for(thesis: &str) -> &'static str {
    let lower = thesis.to_ascii_lowercase();
    if ["reject", "odrzu", "zakaz", "never", "nigdy"]
        .iter()
        .any(|word| lower.contains(word))
    {
        "rejects"
    } else if ["constraint", "constrain", "wymaga", "must", "musi"]
        .iter()
        .any(|word| lower.contains(word))
    {
        "constrains"
    } else if ["contradict", "sprzecz", "łamie"]
        .iter()
        .any(|word| lower.contains(word))
    {
        "contradicts"
    } else if ["because", "ponieważ", "wyjaśnia", "explains"]
        .iter()
        .any(|word| lower.contains(word))
    {
        "explains"
    } else {
        "implements"
    }
}

fn target_from_anchor(anchor: &Anchor) -> OverlayTarget {
    match &anchor.qualified_symbol {
        Some(symbol) => OverlayTarget::Symbol {
            path: Some(anchor.normalized_path.clone()),
            language: anchor.language.clone(),
            qualified_symbol: symbol.clone(),
            signature_hash: anchor.signature_hash.clone(),
            anchor_id: Some(anchor.anchor_id.clone()),
        },
        None => OverlayTarget::Path {
            path: anchor.normalized_path.clone(),
            language: Some(anchor.language.clone()),
            anchor_id: Some(anchor.anchor_id.clone()),
        },
    }
}

fn card_timestamp(card: &CanonicalCard) -> String {
    match &card.frame.timestamp {
        Known::Value(timestamp) if timestamp.contains('T') => timestamp.clone(),
        _ => card
            .frame
            .date
            .as_ref()
            .map(|date| format!("{date}T00:00:00Z"))
            .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_owned()),
    }
}

fn random_intent_id() -> Result<String> {
    let mut bytes = [0u8; 8];
    getrandom::fill(&mut bytes).map_err(|error| anyhow!("assign intent id: {error}"))?;
    Ok(format!("int1:{}", hex::encode(bytes)))
}

fn safe_token(value: &str) -> String {
    let clean: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
                ch
            } else {
                '-'
            }
        })
        .take(128)
        .collect();
    if clean.is_empty() {
        short_hash(value)
    } else {
        clean
    }
}

fn contains_token(haystack: &str, needle: &str) -> bool {
    haystack.match_indices(needle).any(|(start, _)| {
        let before = haystack[..start].chars().next_back();
        let after = haystack[start + needle.len()..].chars().next();
        before.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
            && after.is_none_or(|ch| !ch.is_alphanumeric() && ch != '_')
    })
}

fn content_hash(
    thesis: &str,
    status: &str,
    relations: &[OverlayRelation],
    refs: &[OverlayRef],
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(thesis.as_bytes());
    hasher.update(b"\0");
    hasher.update(status.as_bytes());
    for relation in relations {
        hasher.update(b"\0");
        hasher.update(relation.kind.as_bytes());
        hasher.update(b"\0");
        hasher.update(relation.intent_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(relation.evidence_ref.as_bytes());
        hasher.update(b"\0");
        hasher.update(relation.confidence.to_bits().to_le_bytes());
        hasher.update(b"\0");
        hasher.update(relation.observed_at.as_bytes());
    }
    for reference in refs {
        hasher.update(b"\0");
        hasher.update(reference.evidence_event_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(reference.opaque_ref.as_bytes());
    }
    format!("ch1:{}", hex::encode(hasher.finalize()))
}

fn short_hash(value: &str) -> String {
    hex::encode(Sha256::digest(value.as_bytes()))[..16].to_owned()
}

fn truncate_chars(value: &str, max: usize) -> String {
    if value.chars().count() <= max {
        value.to_owned()
    } else {
        value
            .chars()
            .take(max)
            .collect::<String>()
            .trim()
            .to_owned()
    }
}

fn atomic_write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("overlay index path has no parent")?;
    fs::create_dir_all(parent)?;
    let temp = parent.join(format!(".overlay-{}.tmp", std::process::id()));
    fs::write(&temp, serde_json::to_vec_pretty(value)?)?;
    fs::rename(&temp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::write_canonical_projection_at;
    use aicx_parser::engine::{
        AgentKind, BoundaryFlags, ParseStatus, TurnKind, VisibleCompleteness,
    };
    use aicx_parser::projections::{
        CANONICAL_CARD_SCHEMA, CanonicalProjection, ProjectAttribution, ProjectBucket,
        TimelineFrame,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    #[cfg(unix)]
    #[derive(Debug, Deserialize)]
    struct GoldenSet {
        repo_id: String,
        snapshot_commit: String,
        anchor_catalog_revision: String,
        producer_version: String,
        pairs: Vec<GoldenPair>,
    }

    #[cfg(unix)]
    #[derive(Debug, Deserialize)]
    struct GoldenPair {
        anchor_id: String,
        path: String,
        language: String,
        thesis_marker: String,
        evidence: GoldenEvidence,
    }

    #[cfg(unix)]
    #[derive(Debug, Deserialize)]
    struct GoldenEvidence {
        path: String,
        line: usize,
    }

    #[derive(Debug, Deserialize)]
    struct SemanticFixture {
        schema: String,
        source: String,
        cases: Vec<SemanticCase>,
    }

    #[derive(Debug, Deserialize)]
    struct SemanticCase {
        name: String,
        claims: Vec<SemanticClaim>,
    }

    #[derive(Debug, Deserialize)]
    struct SemanticClaim {
        intent_id: String,
        session: String,
        thesis: String,
        decided_at: String,
        evidence: String,
        target: OverlayTarget,
        embedding: Vec<f32>,
    }

    fn semantic_fixture() -> SemanticFixture {
        let fixture: SemanticFixture = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/overlay-intent-v1/dedup_semantic_2026-07-12.json"
        )))
        .unwrap();
        assert_eq!(fixture.schema, "aicx.overlay.semantic-fixture.v1");
        assert!(fixture.source.contains("2026-07-12"));
        fixture
    }

    fn semantic_case(fixture: &SemanticFixture, name: &str) -> (Vec<OverlayEntry>, Vec<Vec<f32>>) {
        let case = fixture
            .cases
            .iter()
            .find(|case| case.name == name)
            .unwrap_or_else(|| panic!("missing semantic fixture case {name}"));
        let entries = case
            .claims
            .iter()
            .map(|claim| {
                let refs = vec![OverlayRef {
                    evidence_event_id: claim.evidence.clone(),
                    opaque_ref: format!("session:{}#turn-1", claim.session),
                }];
                OverlayEntry {
                    intent_id: claim.intent_id.clone(),
                    content_hash: content_hash(&claim.thesis, "current", &[], &refs),
                    target: claim.target.clone(),
                    thesis: claim.thesis.clone(),
                    status: "current".to_owned(),
                    authority: "operator_confirmed".to_owned(),
                    verification_status: "unverified".to_owned(),
                    valid_from: claim.decided_at.clone(),
                    valid_to: None,
                    relations: Vec::new(),
                    attributions: Vec::new(),
                    refs,
                }
            })
            .collect();
        let embeddings = case
            .claims
            .iter()
            .map(|claim| claim.embedding.clone())
            .collect();
        (entries, embeddings)
    }

    #[test]
    fn overlay_dedup_semantic_merges_paraphrases_and_live_pathologies() {
        let fixture = semantic_fixture();
        for (name, expected_refs) in [
            ("three_session_paraphrases", 3),
            ("live_decision_why_swap", 2),
            ("live_intent_15x", 3),
        ] {
            let (entries, embeddings) = semantic_case(&fixture, name);
            let earliest = entries
                .iter()
                .min_by_key(|entry| (&entry.valid_from, &entry.intent_id))
                .unwrap();
            let expected_id = earliest.intent_id.clone();
            let expected_date = earliest.valid_from.clone();
            let evidence_before: BTreeSet<_> = entries
                .iter()
                .flat_map(|entry| entry.refs.iter())
                .map(|reference| reference.evidence_event_id.clone())
                .collect();
            let (groups, membership) =
                consolidate_entries(entries, embeddings, &BTreeMap::new()).unwrap();
            assert_eq!(groups.len(), 1, "semantic fixture case {name}");
            assert_eq!(groups[0].intent_id, expected_id);
            assert_eq!(groups[0].valid_from, expected_date);
            assert_eq!(groups[0].refs.len(), expected_refs);
            assert!(membership.values().all(|group| group == &expected_id));
            let evidence_after: BTreeSet<_> = groups[0]
                .refs
                .iter()
                .map(|reference| reference.evidence_event_id.clone())
                .collect();
            assert_eq!(
                evidence_before, evidence_after,
                "parser evidence ids changed"
            );
        }
    }

    #[test]
    fn overlay_dedup_semantic_respects_typed_target_and_persisted_group_id() {
        let fixture = semantic_fixture();
        let (entries, embeddings) =
            semantic_case(&fixture, "same_semantics_different_typed_targets");
        let (groups, _) = consolidate_entries(entries, embeddings, &BTreeMap::new()).unwrap();
        assert_eq!(groups.len(), 2, "different typed targets must not merge");

        let (entries, embeddings) = semantic_case(&fixture, "three_session_paraphrases");
        let established = BTreeMap::from([(
            entries[0].intent_id.clone(),
            "int1:persistedcluster".to_owned(),
        )]);
        let (groups, membership) = consolidate_entries(entries, embeddings, &established).unwrap();
        assert_eq!(groups[0].intent_id, "int1:persistedcluster");
        assert!(
            membership
                .values()
                .all(|group| group == "int1:persistedcluster")
        );
    }

    #[test]
    fn overlay_dedup_semantic_negation_veto_beats_embedding_similarity() {
        let fixture = semantic_fixture();
        let (entries, embeddings) = semantic_case(&fixture, "negation_veto_supersede");
        let similarity = intent_candidate_similarity(&embeddings[0], &embeddings[1]);
        assert!(
            similarity >= SEMANTIC_INTENT_CANDIDATE_THRESHOLD,
            "counterexample must be an embedding candidate, got {similarity}"
        );
        assert!(negation_veto(&entries[0].thesis, &entries[1].thesis));
        let (groups, _) = consolidate_entries(entries, embeddings, &BTreeMap::new()).unwrap();
        assert_eq!(groups.len(), 2, "vetoed candidate was falsely merged");
    }

    #[test]
    fn overlay_supersede_is_evidence_backed_and_current_sorts_first_per_target() {
        let fixture = semantic_fixture();
        let (entries, embeddings) = semantic_case(&fixture, "negation_veto_supersede");
        let old_hash = entries[0].content_hash.clone();
        let (groups, _) = consolidate_entries(entries, embeddings, &BTreeMap::new()).unwrap();
        let current = groups
            .iter()
            .find(|entry| entry.status == "current")
            .unwrap();
        let superseded = groups
            .iter()
            .find(|entry| entry.status == "superseded")
            .unwrap();
        assert_eq!(
            groups[0].status, "current",
            "current must sort first in target group"
        );
        assert!(current.valid_from > superseded.valid_from);
        assert_eq!(
            superseded.valid_to.as_deref(),
            Some(current.valid_from.as_str())
        );
        let forward = superseded
            .relations
            .iter()
            .find(|relation| relation.kind == "superseded_by")
            .expect("older decision needs superseded_by");
        assert_eq!(forward.intent_id, current.intent_id);
        assert!(forward.evidence_ref.starts_with("ev1:"));
        assert!(forward.confidence >= SEMANTIC_INTENT_CANDIDATE_THRESHOLD as f64);
        assert_eq!(forward.observed_at, current.valid_from);
        assert!(current.relations.iter().any(|relation| {
            relation.kind == "supersedes" && relation.intent_id == superseded.intent_id
        }));
        assert_ne!(
            superseded.content_hash, old_hash,
            "supersede must revise content hash"
        );
    }

    #[test]
    fn thesis_filter_drops_dispatch_boilerplate_and_caps_output() {
        assert!(distill_thesis("TASK: VIBECRAFTED_REPORT_PATH=/tmp/x").is_none());
        let long = format!("DECISION: {}", "a".repeat(240));
        assert_eq!(distill_thesis(&long).unwrap().chars().count(), 200);
        let report =
            "1 - docs/contracts/one.json — first contract\n2 - tools/two.py — second contract";
        assert_eq!(distill_theses(report).len(), 2);
    }

    #[test]
    fn exact_path_wins_and_filename_only_abstains() {
        let root =
            std::env::temp_dir().join(format!("aicx-overlay-resolve-{}", std::process::id()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let anchors = vec![Anchor {
            anchor_id: format!("anc1:{}", "a".repeat(64)),
            normalized_path: "src/main.rs".to_owned(),
            language: "rs".to_owned(),
            qualified_symbol: None,
            signature_hash: None,
        }];
        let exact = attribution_candidates("DECISION: keep src/main.rs", &anchors, &root);
        assert_eq!(exact[0].confidence, 0.96);
        let weak = attribution_candidates("DECISION: keep main.rs", &anchors, &root);
        assert_eq!(weak[0].confidence, 0.72);
        let symbol_anchors = [Anchor {
            qualified_symbol: Some("Thing".to_owned()),
            ..anchors[0].clone()
        }];
        let leaked = attribution_candidates(
            "DECISION: Thing belongs to other-repo/src/main.rs",
            &symbol_anchors,
            &root,
        );
        assert!(
            leaked.is_empty(),
            "cross-repo path must block symbol leakage"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn low_confidence_candidate_is_payload_only() {
        let root = unique_test_root("unresolved");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
        let intent = IndexedIntent {
            intent_id: "int1:0123456789abcdef".to_owned(),
            group_intent_id: String::new(),
            evidence_event_id: "ev1:codex:session-0001:000001:text:0123456789abcdef".to_owned(),
            claim_key: "claim2:test".to_owned(),
            session_id: "session-0001".to_owned(),
            turn_idx: 1,
            thesis: "main.rs remains the executable boundary".to_owned(),
            valid_from: "2026-07-12T12:00:00Z".to_owned(),
            authority: "operator_confirmed".to_owned(),
            embedding: vec![1.0, 0.0],
        };
        let index = SideIndex {
            schema: OVERLAY_INDEX_SCHEMA.to_owned(),
            repo_id: "Loctree/example".to_owned(),
            store_revision: format!("sr1:{}", "a".repeat(64)),
            embedding_model: "test".to_owned(),
            entries: vec![intent],
            groups: Vec::new(),
            unresolved_attributions: Vec::new(),
        };
        let catalog = AnchorCatalog {
            repo_id: "Loctree/example".to_owned(),
            snapshot_commit: "abc1234".to_owned(),
            anchor_catalog_revision: format!("acr1:{}", "b".repeat(64)),
            producer_version: "test".to_owned(),
            anchors: vec![Anchor {
                anchor_id: format!("anc1:{}", "c".repeat(64)),
                normalized_path: "src/main.rs".to_owned(),
                language: "rs".to_owned(),
                qualified_symbol: None,
                signature_hash: None,
            }],
        };
        let (entries, unresolved) = resolve_entries(&index.entries, &catalog, &root);
        assert!(entries.is_empty());
        assert_eq!(unresolved.len(), 1);
        assert_eq!(unresolved[0].confidence, 0.72);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn attribution_change_moves_overlay_revision_not_store_revision() {
        let catalog = AnchorCatalog {
            repo_id: "Loctree/loctree-suite".to_owned(),
            snapshot_commit: "abc".to_owned(),
            anchor_catalog_revision: format!("acr1:{}", "a".repeat(64)),
            producer_version: "1".to_owned(),
            anchors: Vec::new(),
        };
        let store = format!("sr1:{}", "b".repeat(64));
        let model = "cloud:test-model";
        let revision = overlay_revision(&catalog, &store, model);
        let changed = overlay_revision_with_attribution(&catalog, &store, "resolver.v2", model);
        let semantic_changed = overlay_revision(&catalog, &store, "cloud:next-model");
        assert!(revision.starts_with("ov1:"));
        assert_ne!(revision, changed);
        assert_ne!(revision, semantic_changed);
        assert_eq!(store, format!("sr1:{}", "b".repeat(64)));
    }

    #[cfg(unix)]
    #[test]
    fn golden_precision_side_index_and_warm_path_contract() {
        use std::os::unix::fs::PermissionsExt;

        let golden: GoldenSet = serde_json::from_str(include_str!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/golden-attributions/loctree-suite.json"
        )))
        .unwrap();
        assert!(golden.pairs.len() >= 20);
        let root = unique_test_root("golden");
        let repo = root.join("repo");
        let store = root.join("store");
        let index = root.join("index");
        fs::create_dir_all(&repo).unwrap();
        for pair in &golden.pairs {
            assert_eq!(pair.evidence.path, pair.path);
            assert_eq!(pair.evidence.line, 1);
            let path = repo.join(&pair.path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, format!("proof for {}\n", pair.thesis_marker)).unwrap();
        }
        let catalog = AnchorCatalog {
            repo_id: golden.repo_id.clone(),
            snapshot_commit: golden.snapshot_commit.clone(),
            anchor_catalog_revision: golden.anchor_catalog_revision.clone(),
            producer_version: golden.producer_version.clone(),
            anchors: golden
                .pairs
                .iter()
                .map(|pair| Anchor {
                    anchor_id: pair.anchor_id.clone(),
                    normalized_path: pair.path.clone(),
                    language: pair.language.clone(),
                    qualified_symbol: None,
                    signature_hash: None,
                })
                .collect(),
        };
        let catalog_path = root.join("anchors.json");
        fs::write(&catalog_path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        let loct = root.join("loct");
        fs::write(
            &loct,
            format!("#!/bin/sh\nexec /bin/cat '{}'\n", catalog_path.display()),
        )
        .unwrap();
        let mut permissions = fs::metadata(&loct).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&loct, permissions).unwrap();

        let projection_root = store.join("loctree-suite");
        let mut cards: Vec<_> = golden
            .pairs
            .iter()
            .enumerate()
            .map(|(index, pair)| canonical_card(index, pair))
            .collect();
        write_projection(&projection_root, &cards, 'a');
        let options = OverlayOptions {
            repo: repo.clone(),
            rebuild: false,
            loct_bin: Some(loct.clone()),
            store_root: Some(store.clone()),
            index_root: Some(index.clone()),
        };
        let (first, first_stats) = build_overlay(&options).unwrap();
        assert_eq!(first_stats.raw_session_files_opened, 0);
        let expected_groups = golden
            .pairs
            .iter()
            .map(|pair| (&pair.path, &pair.thesis_marker))
            .collect::<BTreeSet<_>>()
            .len();
        assert_eq!(first.entries.len(), expected_groups);
        assert!(first.entries.iter().all(|entry| {
            entry
                .refs
                .iter()
                .all(|reference| reference.evidence_event_id.starts_with("ev1:"))
        }));
        let mut emitted: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for entry in &first.entries {
            for attribution in &entry.attributions {
                emitted
                    .entry(attribution.target_anchor.clone())
                    .or_default()
                    .push(entry.thesis.clone());
            }
        }
        let mut hits = 0usize;
        let mut misses = 0usize;
        let mut abstentions = 0usize;
        for pair in &golden.pairs {
            match emitted.get(&pair.anchor_id) {
                None => abstentions += 1,
                Some(theses)
                    if theses
                        .iter()
                        .any(|thesis| thesis.contains(&pair.thesis_marker)) =>
                {
                    hits += 1
                }
                Some(_) => misses += 1,
            }
        }
        let precision = hits as f64 / (hits + misses).max(1) as f64;
        assert!(precision >= 0.90, "precision={precision}");
        assert_eq!(hits, golden.pairs.len(), "golden recall regressed");
        eprintln!(
            "golden precision={precision:.2} recall=1.00 abstentions={abstentions} hits={hits} misses={misses}"
        );

        let stable_ids: BTreeMap<_, _> = first
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.refs[0].evidence_event_id.clone(),
                    entry.intent_id.clone(),
                )
            })
            .collect();
        cards.push(canonical_card(99, &golden.pairs[0]));
        write_projection(&projection_root, &cards, 'b');
        let append_started = Instant::now();
        let (appended, append_stats) = build_overlay(&options).unwrap();
        assert!(append_started.elapsed().as_secs_f64() < 5.0);
        assert_eq!(append_stats.new_intents, 1);
        for entry in &appended.entries {
            if let Some(before) = stable_ids.get(&entry.refs[0].evidence_event_id) {
                assert_eq!(before, &entry.intent_id);
            }
        }
        assert_ne!(first.store_revision, appended.store_revision);
        assert_ne!(first.overlay_revision, appended.overlay_revision);

        let mut rebuild_options = options.clone();
        rebuild_options.rebuild = true;
        let rebuild_started = Instant::now();
        let (rebuilt, _) = build_overlay(&rebuild_options).unwrap();
        eprintln!(
            "overlay full rebuild elapsed={:?}",
            rebuild_started.elapsed()
        );
        let rebuilt_ids: BTreeMap<_, _> = rebuilt
            .entries
            .iter()
            .map(|entry| {
                (
                    entry.refs[0].evidence_event_id.clone(),
                    entry.intent_id.clone(),
                )
            })
            .collect();
        for (evidence, intent_id) in stable_ids {
            assert_eq!(rebuilt_ids.get(&evidence), Some(&intent_id));
        }

        let mut warm = Vec::new();
        for _ in 0..5 {
            let started = Instant::now();
            let (cached, stats) = build_overlay(&options).unwrap();
            warm.push(started.elapsed().as_secs_f64());
            assert_eq!(cached.overlay_revision, rebuilt.overlay_revision);
            assert_eq!(stats.raw_session_files_opened, 0);
        }
        warm.sort_by(f64::total_cmp);
        assert!(warm[2] < 1.0, "warm p50 exceeded CI tolerance: {warm:?}");
        assert!(
            *warm.last().unwrap() < 4.0,
            "warm p95 exceeded CI tolerance: {warm:?}"
        );
        if let Ok(path) = std::env::var("AICX_OVERLAY_TEST_OUTPUT") {
            fs::write(path, serde_json::to_vec_pretty(&rebuilt).unwrap()).unwrap();
        }
        if std::env::var_os("AICX_OVERLAY_TEST_ROOT").is_none() {
            let _ = fs::remove_dir_all(root);
        }
    }

    #[cfg(unix)]
    fn canonical_card(index: usize, pair: &GoldenPair) -> CanonicalCard {
        let evidence = format!("ev1:codex:session-{index:04}:{index:06}:text:{index:016x}");
        CanonicalCard {
            schema: CANONICAL_CARD_SCHEMA.to_owned(),
            id: format!("card3:{index:024x}"),
            session_id: format!("session-{index:04}"),
            project: ProjectBucket::normalized(
                "Loctree/loctree-suite",
                ProjectAttribution::OperatorOverride {
                    supplied: "Loctree/loctree-suite".to_owned(),
                },
            ),
            agent: AgentKind::Codex,
            model: Known::value("test-model".to_owned()),
            source_hash: format!("{index:064x}"),
            source_bytes: 100,
            frame: TimelineFrame {
                turn_idx: index as u64,
                segment_id: 0,
                role: TurnRole::User,
                kind: TurnKind::UserMsg,
                timestamp: Known::value("2026-07-12T12:00:00Z".to_owned()),
                date: Some("2026-07-12".to_owned()),
                cwd: Known::value("/repo/loctree-suite".to_owned()),
                branch: Known::value("feat/substrate-scaffold".to_owned()),
                text: format!("DECISION: {} applies in {}", pair.thesis_marker, pair.path),
                text_hash: format!("{index:064x}"),
                source_spans: Vec::new(),
                evidence_event_ids: vec![evidence.clone()],
            },
            evidence_event_ids: vec![evidence],
            parse_status: ParseStatus {
                visible_completeness: VisibleCompleteness::CompleteVisible,
                boundary_flags: BoundaryFlags::default(),
                malformed_tail_present: false,
                visible_event_lost: false,
            },
            usage_references: Vec::new(),
        }
    }

    #[cfg(unix)]
    fn write_projection(root: &Path, cards: &[CanonicalCard], revision_byte: char) {
        write_canonical_projection_at(
            root,
            &CanonicalProjection {
                schema: "aicx.store.canonical_projection.v1".to_owned(),
                extraction_schema: CANONICAL_CARD_SCHEMA.to_owned(),
                producer_version: "aicx-parser@test".to_owned(),
                store_revision: format!("sr1:{}", revision_byte.to_string().repeat(64)),
                cards: cards.to_vec(),
            },
        )
        .unwrap();
    }

    fn unique_test_root(label: &str) -> PathBuf {
        if let Some(root) = std::env::var_os("AICX_OVERLAY_TEST_ROOT") {
            let root = PathBuf::from(root);
            let _ = fs::remove_dir_all(&root);
            return root;
        }
        std::env::temp_dir().join(format!(
            "aicx-overlay-{label}-{}-{}",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::Relaxed)
        ))
    }
}
