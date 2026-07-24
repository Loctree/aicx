// Vibecrafted with AI Agents by Vetcoders (c)2024-2026 LibraxisAI
use std::borrow::Cow;
use std::collections::HashMap;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};
use tantivy::collector::{FilterCollector, TopDocs};
use tantivy::query::{
    AllQuery, BooleanQuery, ConstScoreQuery, PhraseQuery, Query, QueryParser, TermQuery,
};
use tantivy::schema::{
    FAST, Field, INDEXED, IndexRecordOption, STORED, STRING, Schema, TantivyDocument,
    TextFieldIndexing, TextOptions, Value,
};
use tantivy::tokenizer::{
    LowerCaser, RemoveLongFilter, SimpleTokenizer, TextAnalyzer, Token, TokenFilter, TokenStream,
    Tokenizer,
};
use tantivy::{DocAddress, Index, IndexWriter, Score, Searcher, Term, doc};
use tantivy_stemmers::algorithms;

use crate::{ChunkRef, FilterSet, Hit, LexicalCommitId, LexicalIndex, LexicalQuery};

pub const TANTIVY_KIND: &str = "tantivy_lexical";
pub const TANTIVY_SCHEMA_VERSION: &str = "tantivy_lexical_v2_fast_body";
pub const TANTIVY_INDEX_DIR: &str = "tantivy_lex";

const BODY_TOKENIZER: &str = "aicx_body_pl_en";
const WRITER_HEAP_BYTES: usize = 50_000_000;

/// Tantivy-backed BM25 implementation of [`LexicalIndex`].
pub struct TantivyAdapter {
    pub index: Index,
    pub schema: Schema,
    pub writer: Option<IndexWriter>,
    pub commit_id: LexicalCommitId,
    pub doc_count: usize,
    pub dir: PathBuf,
    fields: TantivyFields,
}

#[derive(Clone)]
struct TantivyFields {
    id: Field,
    source_path: Field,
    body: Field,
    agent: Field,
    date: Field,
    project: Field,
    metadata_json: Field,
    agent_filter: Field,
    date_filter: Field,
    project_filter: Field,
}

impl TantivyAdapter {
    pub fn new(base_dir: PathBuf) -> Result<Self> {
        let dir = base_dir.join(TANTIVY_INDEX_DIR);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create tantivy dir {}", dir.display()))?;

        let (schema, _) = build_schema();
        let index = if dir.join("meta.json").exists() {
            Index::open_in_dir(&dir)
                .with_context(|| format!("open tantivy index {}", dir.display()))?
        } else {
            Index::create_in_dir(&dir, schema.clone())
                .with_context(|| format!("create tantivy index {}", dir.display()))?
        };
        register_tokenizers(&index);

        let schema = index.schema();
        let fields = TantivyFields::from_schema(&schema)?;
        let commit_id = read_commit_id(&index)?;
        let doc_count = read_doc_count(&index)?;

        Ok(Self {
            index,
            schema,
            writer: None,
            commit_id,
            doc_count,
            dir,
            fields,
        })
    }

    pub fn index_dir(&self) -> &Path {
        &self.dir
    }

    /// Scan committed document metadata without requiring a lexical query.
    ///
    /// This is the canonical metadata-only retrieval surface for callers such
    /// as `aicx steer`. It examines every live document before applying the
    /// predicate, so rare exact metadata matches cannot disappear behind an
    /// arbitrary lexical top-N boundary. The caller-provided limit bounds only
    /// returned values; the committed corpus remains the search authority.
    pub fn scan_metadata(
        &self,
        limit: usize,
        mut predicate: impl FnMut(&serde_json::Value) -> bool,
    ) -> Result<Vec<serde_json::Value>> {
        if limit == 0 || self.doc_count == 0 {
            return Ok(Vec::new());
        }

        let reader = self.index.reader().context("open tantivy reader")?;
        let searcher = reader.searcher();
        let collector = TopDocs::with_limit(self.doc_count).order_by_score();
        let docs = searcher
            .search(&AllQuery, &collector)
            .context("scan committed tantivy metadata")?;
        let mut matches = Vec::with_capacity(limit.min(docs.len()));
        for (rank, (score, address)) in docs.into_iter().enumerate() {
            let hit = self.hit_from_doc(&searcher, score, rank, address)?;
            if predicate(&hit.metadata) {
                matches.push(hit.metadata);
                if matches.len() >= limit {
                    break;
                }
            }
        }
        Ok(matches)
    }

    fn refresh_stats(&mut self) -> Result<LexicalCommitId> {
        self.commit_id = read_commit_id(&self.index)?;
        self.doc_count = read_doc_count(&self.index)?;
        Ok(self.commit_id.clone())
    }

    fn writer_mut(&mut self) -> Result<&mut IndexWriter> {
        if self.writer.is_none() {
            self.writer = Some(
                self.index
                    .writer_with_num_threads(1, WRITER_HEAP_BYTES)
                    .context("create tantivy index writer")?,
            );
        }
        self.writer
            .as_mut()
            .ok_or_else(|| anyhow!("tantivy writer was not initialized"))
    }

    fn add_chunk_to_writer(
        fields: &TantivyFields,
        writer: &mut IndexWriter,
        chunk: &ChunkRef,
    ) -> Result<()> {
        let metadata_json =
            serde_json::to_string(&chunk.metadata).context("serialize chunk metadata")?;
        let agent = metadata_string(&chunk.metadata, "agent");
        let date = metadata_string(&chunk.metadata, "date");
        let project = metadata_string(&chunk.metadata, "project");

        writer.add_document(doc!(
            fields.id => chunk.id.as_str(),
            fields.source_path => chunk.source_path.as_str(),
            fields.body => chunk.text.as_str(),
            fields.agent => agent.as_str(),
            fields.date => date.as_str(),
            fields.project => project.as_str(),
            fields.metadata_json => metadata_json.as_str(),
            fields.agent_filter => stable_filter_key(&agent),
            fields.date_filter => stable_filter_key(&date),
            fields.project_filter => stable_filter_key(&project),
        ))?;
        Ok(())
    }

    fn parse_query(&self, text: &str) -> Result<Box<dyn Query>> {
        if text.trim().is_empty() {
            return Ok(Box::new(AllQuery));
        }
        let parser = QueryParser::for_index(&self.index, vec![self.fields.body, self.fields.id]);
        let parsed = parser
            .parse_query(text)
            .with_context(|| format!("parse lexical query {text:?}"))?;

        // A catalog document is one whole session, so ordinary BM25 length
        // normalization can bury the right long conversation beneath a short
        // file that repeats only one query word. Add a small field-length-
        // independent score per distinct body term while retaining BM25 for
        // frequency and rarity. This makes term coverage the first useful
        // discriminator without turning search into strict conjunction.
        let mut analyzer = self
            .index
            .tokenizer_for_field(self.fields.body)
            .context("open body query tokenizer")?;
        let mut phrase_terms = Vec::new();
        analyzer.token_stream(text).process(&mut |token| {
            phrase_terms.push(Term::from_field_text(self.fields.body, &token.text));
        });
        let mut body_terms = phrase_terms.clone();
        body_terms.dedup();
        if body_terms.len() < 2 {
            return Ok(parsed);
        }
        let coverage = BooleanQuery::union(
            body_terms
                .into_iter()
                .map(|term| {
                    Box::new(ConstScoreQuery::new(
                        Box::new(TermQuery::new(term, IndexRecordOption::Basic)),
                        5.0,
                    )) as Box<dyn Query>
                })
                .collect(),
        );
        let phrase = ConstScoreQuery::new(Box::new(PhraseQuery::new(phrase_terms)), 20.0);
        Ok(Box::new(BooleanQuery::union(vec![
            parsed,
            Box::new(coverage),
            Box::new(phrase),
        ])))
    }

    fn search_top_docs(
        &self,
        searcher: &Searcher,
        query: &dyn Query,
        limit: usize,
        filters: &FilterSet,
    ) -> Result<Vec<(Score, DocAddress)>> {
        let collector = TopDocs::with_limit(limit).order_by_score();

        let docs = match primary_filter(filters) {
            Some((filter_field, filter_value)) => {
                let field = match filter_field {
                    "agent" => "agent_filter",
                    "date" => "date_filter",
                    "project" => "project_filter",
                    _ => {
                        return searcher
                            .search(query, &collector)
                            .context("run tantivy query");
                    }
                };
                let expected = stable_filter_key(&filter_value);
                let filtered: FilterCollector<_, _, u64> = FilterCollector::new(
                    field.to_string(),
                    move |value| value == expected,
                    collector,
                );
                searcher
                    .search(query, &filtered)
                    .with_context(|| format!("run tantivy query with {filter_field} pre-filter"))?
            }
            None => searcher
                .search(query, &collector)
                .context("run tantivy query")?,
        };

        Ok(docs)
    }

    fn hit_from_doc(
        &self,
        searcher: &Searcher,
        score: Score,
        rank: usize,
        address: DocAddress,
    ) -> Result<Hit> {
        let document: TantivyDocument =
            searcher.doc(address).context("load tantivy hit document")?;

        let chunk_id = required_text(&document, self.fields.id, "id")?;
        let source_path = required_text(&document, self.fields.source_path, "source_path")?;
        let agent = text_or_empty(&document, self.fields.agent);
        let date = text_or_empty(&document, self.fields.date);
        let project = text_or_empty(&document, self.fields.project);
        let metadata_json = text_or_empty(&document, self.fields.metadata_json);
        let mut metadata = serde_json::from_str(&metadata_json)
            .unwrap_or_else(|_| serde_json::Value::Object(Default::default()));
        ensure_metadata_field(&mut metadata, "source_path", source_path);
        ensure_metadata_field(&mut metadata, "agent", agent);
        ensure_metadata_field(&mut metadata, "date", date);
        ensure_metadata_field(&mut metadata, "project", project);
        Ok(Hit {
            chunk_id,
            score,
            rank,
            source: TANTIVY_KIND.to_string(),
            metadata,
        })
    }

    /// Extract query-time body context only for already-ranked candidates.
    ///
    /// Whole-session bodies can be large. Calling Tantivy's snippet generator
    /// for the full 500-hit recency window cost seconds; exact-id enrichment
    /// lets the caller pay only for the small set it may display.
    pub fn query_snippets(
        &self,
        query_text: &str,
        chunk_ids: &[String],
    ) -> Result<HashMap<String, Vec<String>>> {
        if query_text.trim().is_empty() || chunk_ids.is_empty() {
            return Ok(HashMap::new());
        }

        let reader = self.index.reader().context("open tantivy reader")?;
        let searcher = reader.searcher();
        let mut snippets = HashMap::with_capacity(chunk_ids.len());

        for chunk_id in chunk_ids {
            let id_query = TermQuery::new(
                Term::from_field_text(self.fields.id, chunk_id),
                IndexRecordOption::Basic,
            );
            let Some((_, address)) = searcher
                .search(&id_query, &TopDocs::with_limit(1).order_by_score())
                .with_context(|| format!("find tantivy chunk {chunk_id} for snippet"))?
                .into_iter()
                .next()
            else {
                continue;
            };
            let document: TantivyDocument = searcher
                .doc(address)
                .with_context(|| format!("load tantivy chunk {chunk_id} for snippet"))?;
            let body = text_or_empty(&document, self.fields.body);
            let lines = query_context_lines(&body, query_text);
            if !lines.is_empty() {
                snippets.insert(chunk_id.clone(), lines);
            }
        }

        Ok(snippets)
    }
}

fn query_context_lines(body: &str, query: &str) -> Vec<String> {
    const MAX_CONTEXT_CHARS: usize = 800;
    let terms = query
        .split_whitespace()
        .map(|term| {
            term.trim_matches(|ch: char| !ch.is_alphanumeric() && ch != '-' && ch != '_')
                .to_lowercase()
        })
        .filter(|term| term.chars().count() >= 3)
        .collect::<Vec<_>>();
    if terms.is_empty() {
        return Vec::new();
    }
    let prefixes = terms
        .iter()
        .map(|term| (term.chars().count() >= 5).then(|| term.chars().take(5).collect::<String>()))
        .collect::<Vec<_>>();
    let normalized_query = query.to_lowercase();

    let lines = body.lines().collect::<Vec<_>>();
    let mut best = None::<(usize, i32)>;
    let mut best_clean_full_match = None::<(usize, i32)>;
    for (index, line) in lines.iter().enumerate() {
        let normalized = line.to_lowercase();
        let term_score = terms
            .iter()
            .zip(&prefixes)
            .filter(|pair| {
                let term = pair.0;
                let prefix = pair.1;
                normalized.contains(term.as_str())
                    || prefix
                        .as_deref()
                        .is_some_and(|prefix| normalized.contains(prefix))
            })
            .count() as i32;
        let phrase_score = if normalized.contains(&normalized_query) {
            2
        } else {
            0
        };
        let benchmark = normalized.contains("aicx search")
            || normalized.contains("acceptance")
            || normalized.contains("golden")
            || normalized.contains("latency")
            || normalized.contains("wall time")
            || normalized.contains(" warm");
        let score = term_score + phrase_score;
        // Prefer the newest equally good context. Continued sessions often
        // repeat an old acceptance prompt before the actual later answer.
        if score >= best.map(|(_, score)| score).unwrap_or(0) {
            best = Some((index, score));
        }
        // A continued session can later quote the release benchmark verbatim.
        // Prefer a real line containing every query term; when no such line
        // exists, retain the benchmark line so the caller can demote the echo.
        if !benchmark
            && term_score == terms.len() as i32
            && score >= best_clean_full_match.map(|(_, score)| score).unwrap_or(0)
        {
            best_clean_full_match = Some((index, score));
        }
    }
    let Some((best_index, score)) = best_clean_full_match.or(best) else {
        return Vec::new();
    };
    if score <= 0 {
        return Vec::new();
    }

    let mut context = Vec::new();
    let mut chars = 0usize;
    let start = best_index.saturating_sub(1);
    let end = (best_index + 3).min(lines.len().saturating_sub(1));
    for line in &lines[start..=end] {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let remaining = MAX_CONTEXT_CHARS.saturating_sub(chars);
        if remaining == 0 {
            break;
        }
        let excerpt = line.chars().take(remaining).collect::<String>();
        chars += excerpt.chars().count();
        context.push(excerpt);
    }
    context
}

impl LexicalIndex for TantivyAdapter {
    fn schema_version(&self) -> &str {
        TANTIVY_SCHEMA_VERSION
    }

    fn build(&mut self, chunks: &[ChunkRef]) -> Result<LexicalCommitId> {
        // Bug B: build into a sibling staging dir and atomically swap it in,
        // instead of `remove_dir_all`-ing the live index up front. The old
        // index stays queryable until the swap, so search never hits an empty
        // directory mid-rebuild.

        // Drop the writer first so its lock on `self.dir` is released before we
        // rename directories underneath it.
        self.writer = None;

        let staging = {
            let name = self
                .dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| TANTIVY_INDEX_DIR.to_string());
            let mut p = self.dir.clone();
            p.set_file_name(format!("{name}.building"));
            p
        };
        if staging.exists() {
            fs::remove_dir_all(&staging)
                .with_context(|| format!("clear stale staging dir {}", staging.display()))?;
        }
        fs::create_dir_all(&staging)
            .with_context(|| format!("create staging tantivy dir {}", staging.display()))?;

        let (schema, fields) = build_schema();
        {
            let index = Index::create_in_dir(&staging, schema.clone())
                .with_context(|| format!("create staging tantivy index {}", staging.display()))?;
            register_tokenizers(&index);
            let mut writer = index
                .writer_with_num_threads(1, WRITER_HEAP_BYTES)
                .context("create tantivy index writer")?;

            for chunk in chunks {
                Self::add_chunk_to_writer(&fields, &mut writer, chunk)?;
            }
            writer.commit().context("commit tantivy build")?;
            // writer + index dropped here → all handles on `staging` released
            // before the rename swap below.
        }

        // Promote staging -> live, preserving the previous index until the
        // final rename (last-good stays available throughout the build).
        atomic_swap_dir(&staging, &self.dir)?;

        // Reopen handles from the promoted final location.
        let index = Index::open_in_dir(&self.dir)
            .with_context(|| format!("open promoted tantivy index {}", self.dir.display()))?;
        register_tokenizers(&index);
        let writer = index
            .writer_with_num_threads(1, WRITER_HEAP_BYTES)
            .context("reopen tantivy index writer after swap")?;

        self.index = index;
        self.schema = schema;
        self.fields = fields;
        self.writer = Some(writer);
        self.refresh_stats()
    }

    fn insert(&mut self, chunk: &ChunkRef) -> Result<()> {
        let fields = self.fields.clone();
        {
            let writer = self.writer_mut()?;
            Self::add_chunk_to_writer(&fields, writer, chunk)?;
            writer.commit().context("commit tantivy insert")?;
        }
        self.refresh_stats()?;
        Ok(())
    }

    fn query(&self, q: &LexicalQuery) -> Result<Vec<Hit>> {
        if q.limit == 0 {
            return Ok(Vec::new());
        }

        let query = self.parse_query(&q.text)?;
        let reader = self.index.reader().context("open tantivy reader")?;
        let searcher = reader.searcher();
        let search_limit = if q.filters.values.is_empty() {
            q.limit
        } else {
            self.doc_count.max(q.limit)
        };
        let top_docs = self.search_top_docs(&searcher, query.as_ref(), search_limit, &q.filters)?;

        let mut hits = Vec::new();
        for (score, address) in top_docs {
            let hit = self.hit_from_doc(&searcher, score, hits.len(), address)?;
            if filter_matches(&hit.metadata, &q.filters) {
                hits.push(hit);
            }
            if hits.len() == q.limit {
                break;
            }
        }
        for (rank, hit) in hits.iter_mut().enumerate() {
            hit.rank = rank;
        }
        Ok(hits)
    }

    fn commit_id(&self) -> &LexicalCommitId {
        &self.commit_id
    }

    fn doc_count(&self) -> usize {
        self.doc_count
    }
}

impl TantivyFields {
    fn from_schema(schema: &Schema) -> Result<Self> {
        Ok(Self {
            id: schema.get_field("id")?,
            source_path: schema.get_field("source_path")?,
            body: schema.get_field("body")?,
            agent: schema.get_field("agent")?,
            date: schema.get_field("date")?,
            project: schema.get_field("project")?,
            metadata_json: schema.get_field("metadata_json")?,
            agent_filter: schema.get_field("agent_filter")?,
            date_filter: schema.get_field("date_filter")?,
            project_filter: schema.get_field("project_filter")?,
        })
    }
}

fn build_schema() -> (Schema, TantivyFields) {
    let raw_fast = (STRING | STORED).set_fast(None);
    let body_text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default()
                .set_tokenizer(BODY_TOKENIZER)
                .set_index_option(IndexRecordOption::WithFreqsAndPositions),
        )
        .set_stored();
    let stored_only = TextOptions::default().set_stored();

    let mut builder = Schema::builder();
    let id = builder.add_text_field("id", STRING | STORED);
    let source_path = builder.add_text_field("source_path", STRING | STORED);
    let body = builder.add_text_field("body", body_text);
    let agent = builder.add_text_field("agent", raw_fast.clone());
    let date = builder.add_text_field("date", raw_fast.clone());
    let project = builder.add_text_field("project", raw_fast);
    let metadata_json = builder.add_text_field("metadata_json", stored_only);
    let agent_filter = builder.add_u64_field("agent_filter", FAST | INDEXED);
    let date_filter = builder.add_u64_field("date_filter", FAST | INDEXED);
    let project_filter = builder.add_u64_field("project_filter", FAST | INDEXED);
    let schema = builder.build();
    let fields = TantivyFields {
        id,
        source_path,
        body,
        agent,
        date,
        project,
        metadata_json,
        agent_filter,
        date_filter,
        project_filter,
    };
    (schema, fields)
}

fn register_tokenizers(index: &Index) {
    let tokenizer = TextAnalyzer::builder(SimpleTokenizer::default())
        .filter(RemoveLongFilter::limit(40))
        .filter(LowerCaser)
        .filter(PolishParticipleTrim)
        .filter(TantivyStemmersFilter::new(algorithms::english_porter_2))
        .build();
    index.tokenizers().register(BODY_TOKENIZER, tokenizer);
}

fn read_commit_id(index: &Index) -> Result<LexicalCommitId> {
    let id = index
        .searchable_segment_ids()
        .context("read tantivy searchable segments")?
        .first()
        .map(|segment| segment.uuid_string())
        .unwrap_or_else(|| "empty".to_string());
    Ok(LexicalCommitId(format!("{TANTIVY_SCHEMA_VERSION}:{id}")))
}

fn read_doc_count(index: &Index) -> Result<usize> {
    let reader = index.reader().context("open tantivy reader")?;
    Ok(reader.searcher().num_docs() as usize)
}

fn metadata_string(metadata: &serde_json::Value, key: &str) -> String {
    metadata
        .get(key)
        .and_then(json_filter_value)
        .unwrap_or_default()
}

fn json_filter_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn primary_filter(filters: &FilterSet) -> Option<(&str, String)> {
    for key in ["agent", "date", "project"] {
        if let Some(value) = filters.values.get(key).and_then(json_filter_value) {
            return Some((key, value));
        }
    }
    None
}

fn filter_matches(metadata: &serde_json::Value, filters: &FilterSet) -> bool {
    for (key, expected) in &filters.values {
        match metadata.get(key) {
            Some(actual) if actual == expected => continue,
            _ => return false,
        }
    }
    true
}

fn stable_filter_key(value: &str) -> u64 {
    let mut hasher = StableHasher::default();
    value.hash(&mut hasher);
    hasher.finish()
}

#[derive(Default)]
struct StableHasher(u64);

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        let mut hash = if self.0 == 0 {
            0xcbf2_9ce4_8422_2325
        } else {
            self.0
        };
        for byte in bytes {
            hash ^= u64::from(*byte);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
        self.0 = hash;
    }
}

fn required_text(document: &TantivyDocument, field: Field, name: &str) -> Result<String> {
    document
        .get_first(field)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .ok_or_else(|| anyhow!("tantivy hit missing stored field {name}"))
}

fn text_or_empty(document: &TantivyDocument, field: Field) -> String {
    document
        .get_first(field)
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_default()
}

fn ensure_metadata_field(metadata: &mut serde_json::Value, key: &str, value: String) {
    if value.is_empty() {
        return;
    }
    let Some(object) = metadata.as_object_mut() else {
        return;
    };
    object
        .entry(key.to_string())
        .or_insert_with(|| serde_json::Value::String(value));
}

#[derive(Clone)]
struct TantivyStemmersFilter {
    algorithm: algorithms::Algorithm,
}

impl TantivyStemmersFilter {
    fn new(algorithm: algorithms::Algorithm) -> Self {
        Self { algorithm }
    }
}

impl TokenFilter for TantivyStemmersFilter {
    type Tokenizer<T: Tokenizer> = TantivyStemmersFilterWrapper<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        TantivyStemmersFilterWrapper {
            tokenizer,
            algorithm: self.algorithm,
        }
    }
}

#[derive(Clone)]
struct TantivyStemmersFilterWrapper<T> {
    tokenizer: T,
    algorithm: algorithms::Algorithm,
}

impl<T: Tokenizer> Tokenizer for TantivyStemmersFilterWrapper<T> {
    type TokenStream<'a> = TantivyStemmersTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        TantivyStemmersTokenStream {
            tail: self.tokenizer.token_stream(text),
            algorithm: self.algorithm,
            buffer: String::new(),
        }
    }
}

struct TantivyStemmersTokenStream<T> {
    tail: T,
    algorithm: algorithms::Algorithm,
    buffer: String,
}

impl<T: TokenStream> TokenStream for TantivyStemmersTokenStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let token = self.tail.token_mut();
        match (self.algorithm)(&token.text) {
            Cow::Owned(stemmed) => token.text = stemmed,
            Cow::Borrowed(stemmed) => {
                self.buffer.clear();
                self.buffer.push_str(stemmed);
                std::mem::swap(&mut token.text, &mut self.buffer);
            }
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

#[derive(Clone)]
struct PolishParticipleTrim;

impl TokenFilter for PolishParticipleTrim {
    type Tokenizer<T: Tokenizer> = PolishParticipleTrimWrapper<T>;

    fn transform<T: Tokenizer>(self, tokenizer: T) -> Self::Tokenizer<T> {
        PolishParticipleTrimWrapper { tokenizer }
    }
}

#[derive(Clone)]
struct PolishParticipleTrimWrapper<T> {
    tokenizer: T,
}

impl<T: Tokenizer> Tokenizer for PolishParticipleTrimWrapper<T> {
    type TokenStream<'a> = PolishParticipleTrimTokenStream<T::TokenStream<'a>>;

    fn token_stream<'a>(&'a mut self, text: &'a str) -> Self::TokenStream<'a> {
        PolishParticipleTrimTokenStream {
            tail: self.tokenizer.token_stream(text),
        }
    }
}

struct PolishParticipleTrimTokenStream<T> {
    tail: T,
}

impl<T: TokenStream> TokenStream for PolishParticipleTrimTokenStream<T> {
    fn advance(&mut self) -> bool {
        if !self.tail.advance() {
            return false;
        }
        let token = self.tail.token_mut();
        if token.text.len() > 7 {
            for suffix in ["iony", "iona", "ione", "ieni"] {
                if token.text.ends_with(suffix) {
                    token.text.truncate(token.text.len() - suffix.len());
                    break;
                }
            }
        }
        true
    }

    fn token(&self) -> &Token {
        self.tail.token()
    }

    fn token_mut(&mut self) -> &mut Token {
        self.tail.token_mut()
    }
}

/// Atomically replace the `target` index directory with a fully-built
/// `staging` directory on the same filesystem (Bug B).
///
/// `target` stays intact and queryable until the final rename, so a concurrent
/// reader never sees a half-built index — only the sub-millisecond gap between
/// two `rename(2)` calls (vs the multi-minute window left by the old
/// `remove_dir_all` + rebuild-in-place). The previous index is moved to a
/// `<name>.old` sibling and removed best-effort after promotion; a leftover
/// backup is non-fatal because the new index is already live.
///
/// Caller MUST drop all open tantivy handles (writer + index) on both `target`
/// and `staging` before calling this — an open handle blocks directory rename
/// on Windows and risks stale inodes elsewhere.
fn atomic_swap_dir(staging: &Path, target: &Path) -> Result<()> {
    let file_name = target
        .file_name()
        .ok_or_else(|| anyhow!("swap target has no file name: {}", target.display()))?
        .to_string_lossy()
        .into_owned();
    let mut backup = target.to_path_buf();
    backup.set_file_name(format!("{file_name}.old"));

    if backup.exists() {
        fs::remove_dir_all(&backup)
            .with_context(|| format!("clear stale backup {}", backup.display()))?;
    }
    if target.exists() {
        fs::rename(target, &backup)
            .with_context(|| format!("back up {} -> {}", target.display(), backup.display()))?;
    }
    if let Err(err) = fs::rename(staging, target) {
        // Promotion failed AFTER the live index was moved to backup — roll the
        // backup back so the last-good index stays queryable, instead of
        // leaving an empty path at `target`.
        if backup.exists() && !target.exists() {
            let _ = fs::rename(&backup, target);
        }
        return Err(err)
            .with_context(|| format!("promote {} -> {}", staging.display(), target.display()));
    }
    if backup.exists() {
        // New index is live; a leftover backup is not fatal.
        let _ = fs::remove_dir_all(&backup);
    }
    Ok(())
}

#[cfg(test)]
mod swap_tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn atomic_swap_replaces_target_with_staging() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("idx");
        let staging = tmp.path().join("idx.building");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("marker"), "OLD").unwrap();
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("marker"), "NEW").unwrap();

        atomic_swap_dir(&staging, &target).unwrap();

        assert_eq!(fs::read_to_string(target.join("marker")).unwrap(), "NEW");
        assert!(!staging.exists(), "staging must be consumed by the rename");
        assert!(
            !tmp.path().join("idx.old").exists(),
            "backup must be cleaned up after a successful swap"
        );
    }

    #[test]
    fn atomic_swap_when_target_absent() {
        let tmp = TempDir::new().unwrap();
        let target = tmp.path().join("idx");
        let staging = tmp.path().join("idx.building");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("marker"), "NEW").unwrap();

        atomic_swap_dir(&staging, &target).unwrap();

        assert_eq!(fs::read_to_string(target.join("marker")).unwrap(), "NEW");
        assert!(!staging.exists());
    }

    #[test]
    fn lexical_query_attaches_body_snippet_at_the_actual_match() {
        let tmp = TempDir::new().unwrap();
        let mut adapter = TantivyAdapter::new(tmp.path().to_path_buf()).unwrap();
        adapter
            .build(&[ChunkRef {
                id: "session-1".to_string(),
                source_path: "/sessions/session-1.md".to_string(),
                text: [
                    "Unrelated session preamble.",
                    "Acceptance: aicx search 'routing strzałek taby' must be fast.",
                    "## assistant",
                    "The durable answer is that routing strzałek taby landed in W2-B-4c.",
                    "Unrelated footer.",
                ]
                .join("\n"),
                metadata: serde_json::json!({
                    "agent": "codex",
                    "date": "2026-07-22",
                    "project": "vetcoders/vc-frame",
                }),
            }])
            .unwrap();

        let hits = adapter
            .query(&LexicalQuery {
                text: "routing strzałek taby".to_string(),
                limit: 1,
                filters: FilterSet::default(),
            })
            .unwrap();

        let snippets = adapter
            .query_snippets("routing strzałek taby", &[hits[0].chunk_id.to_string()])
            .unwrap();
        let snippets = &snippets["session-1"];
        assert!(
            snippets.iter().any(|line| line.contains("W2-B-4c")),
            "snippet must come from the matched body region: {snippets:?}"
        );
    }

    #[test]
    fn lexical_query_prefers_multi_term_coverage_over_short_single_term_spam() {
        let tmp = TempDir::new().unwrap();
        let mut adapter = TantivyAdapter::new(tmp.path().to_path_buf()).unwrap();
        adapter
            .build(&[
                ChunkRef {
                    id: "short-spam".to_string(),
                    source_path: "/sessions/short.md".to_string(),
                    text: "arrows arrows arrows arrows arrows".to_string(),
                    metadata: serde_json::json!({}),
                },
                ChunkRef {
                    id: "long-answer".to_string(),
                    source_path: "/sessions/long.md".to_string(),
                    text: format!(
                        "{}\nThe answer connects arrows with routing.",
                        "unrelated session history ".repeat(2_000)
                    ),
                    metadata: serde_json::json!({}),
                },
            ])
            .unwrap();

        let hits = adapter
            .query(&LexicalQuery {
                text: "arrows routing".to_string(),
                limit: 2,
                filters: FilterSet::default(),
            })
            .unwrap();

        assert_eq!(hits[0].chunk_id, "long-answer");
    }

    #[test]
    fn lexical_query_boosts_the_exact_phrase_above_a_reordered_bag_of_words() {
        let tmp = TempDir::new().unwrap();
        let mut adapter = TantivyAdapter::new(tmp.path().to_path_buf()).unwrap();
        adapter
            .build(&[
                ChunkRef {
                    id: "reordered".to_string(),
                    source_path: "/sessions/reordered.md".to_string(),
                    text: "vc-frame has many notes; arrows are elsewhere".to_string(),
                    metadata: serde_json::json!({}),
                },
                ChunkRef {
                    id: "exact".to_string(),
                    source_path: "/sessions/exact.md".to_string(),
                    text: "The operator searched for arrows vc-frame.".to_string(),
                    metadata: serde_json::json!({}),
                },
            ])
            .unwrap();

        let hits = adapter
            .query(&LexicalQuery {
                text: "arrows vc-frame".to_string(),
                limit: 2,
                filters: FilterSet::default(),
            })
            .unwrap();

        assert_eq!(hits[0].chunk_id, "exact");
    }

    #[test]
    fn query_context_prefers_real_pensieve_hit_over_later_aicx_benchmark() {
        let body = [
            "pensieve to znajduje [Pensieve search 'arrows vc-frame']",
            "unrelated release paragraph",
            "Golden queries: `aicx search 'arrows vc-frame'` 0.056 s",
        ]
        .join("\n");

        let lines = query_context_lines(&body, "arrows vc-frame");

        assert_eq!(
            lines.first().map(String::as_str),
            Some("pensieve to znajduje [Pensieve search 'arrows vc-frame']")
        );
    }
}
