# Reproducible retrieval acceptance

The canonical harness builds an isolated source catalog from the public fixtures
in `queries.toml`, publishes through the actual index builder and searches the
result. It also exercises the built CLI and MCP stdio server on the tail fixture.
It never enumerates provider homes or writes the production index.

Every fixed question must retrieve its known session in the top five **and**
return the expected answer in its evidence snippet. The corpus includes Polish
and English decisions, privacy, model identity and a decision placed beyond the
first 8192 characters of a long conversation. Queries themselves are not copied
into the indexed conversations. Acceptance floors are authored requirements;
they cannot be lowered by recording another run.

Portable lexical gate:

```sh
make test-retrieval-eval
```

Bounded real-model hybrid evaluation (explicit model configuration is required):

```sh
AICX_EMBEDDER_CONFIG=/absolute/path/embedder.toml \
AICX_RETRIEVAL_EVAL_REPORT=/absolute/path/hybrid-result.json \
make test-retrieval-eval-live
```

For a cloud-only developer build, select the same real-model mode without
building the native backend:

```sh
AICX_EMBEDDER_CONFIG=/absolute/path/embedder.toml \
AICX_RETRIEVAL_EVAL_REAL_MODEL=1 \
cargo test --no-default-features --features app,cloud-embedder \
  --test retrieval_eval_harness -- --nocapture
```

Real-model mode also uses an English paraphrase of a Polish TLS decision. A
lexical control must miss that target across the entire fixture corpus, and
hybrid retrieval must put it at rank1 with the expected evidence. Constant or
unrelated vectors cannot pass just by inheriting the lexical ranking.

The receipt reports each question's rank, recall, snippet acceptance and latency,
plus aggregate recall, MRR, model identity, manifest schema and transport acceptance. A passing lexical run is not real-model evidence.
The small public fixture proves a bounded acceptance set, not general quality on
all historical sessions. `history/pre-passages-zero-recall.json` preserves the
former host-dependent zero-recall measurement; it is not an acceptance floor.

`search_quality_seed.toml` and `aicx eval search-quality` retain the separate
human-facing historical evidence matrix. Transcript Builder/Memex comparisons
need matching source fixtures and actual donor output; this fixture does not
claim a measured win against either.
