# Regression Delivery Tracker

## W1-C1 — batch semantic-index embedding

- [x] `src/vector_index.rs` uses the live `embed_batch` path for index builds.
- [x] Batch grouping, retry, poison isolation, and single-item fallback tests pass.
- [x] `aicx-embeddings` batch-size configuration tests pass.
- [x] Required clippy gate passes with warnings denied.

### Delivery verifier (operator command, verbatim)

The supplied verifier exits `2` before running Cargo because `git grep -c`
prefixes its count with the filename (`src/vector_index.rs:16`), which is not an
integer accepted by `test -ge`:

```text
zsh:test:1: integer expression expected: src/vector_index.rs:16
```

### Delivery verifier (filename prefix removed)

The equivalent verifier using `cut -d: -f2` exits `0`:

```text
test result: ok. 51 passed; 0 failed; 0 ignored; 0 measured; 746 filtered out; finished in 1.15s

```

### Release runtime walk-around

`AICX_EMBED_BATCH=2 cargo run --release -- index --dry-run --sample 2`
completed through one two-item batch:

```text
aicx index — dry-run report
  chunks_total:        356275
  chunks_sampled:      2
  embeddings_computed: 2
  embed_errors:        0
  dimension:           4096
  model:               qwen3-embedding:8b
  profile:             base
  elapsed_ms:          71960
  full_index_eta_secs: 12818775 (estimated)
  note: dry-run only; omit `--dry-run` to materialize the semantic index.
```
