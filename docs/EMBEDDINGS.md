# AICX Native Embeddings

> Foundation layer for Vibecrafted products: local, explicit, portable
> embeddings without turning every install into a multi-GB surprise.

## Product Truth

AICX now treats the native embedder as its first-choice local embedding path,
not as a fallback behind rust-memex.

The split is:

- **AICX** owns the canonical corpus, steering surfaces, MCP front door, and a
  reusable local embedding library.
- **Roost/rust-memex** owns the advanced retrieval/operator plane: heavier
  provider routing, richer indexing, and premium retrieval workflows.

This removes the old schizophrenia where AICX pretended to be rust-memex while also
depending on rust-memex internals for the same job.

## Library Boundary

The reusable code lives in `crates/aicx-embeddings`.

`aicx` re-exports it under `aicx::embedder::*` for compatibility, but
rust-memex can depend on the smaller crate directly later:

```rust
use aicx_embeddings::{EmbeddingConfig, EmbeddingEngine};

let mut engine = EmbeddingEngine::with_config(EmbeddingConfig::from_env())?;
let vector = engine.embed("hello local retrieval")?;
```

Core API:

- `LocalEmbeddingProvider` — minimal provider trait for future rust-memex adaptation.
- `EmbeddingEngine` — backend-hiding runtime wrapper.
- `EmbeddingConfig` — env/config-driven model selection.
- `EmbeddingModelInfo` — model id, backend, dimension, profile, source.

## Backend

The production backend is GGUF through `llama-cpp-2`.

Why GGUF:

- quantized model files are dramatically smaller than fp16 safetensors
- one model file is easier to hydrate, verify, and cache
- llama.cpp already exposes pooled embeddings for BERT/F2LLM-style GGUF models
- Codescribe's Candle path is a good architectural precedent, but it was built
  for MiniLM/BERT safetensors, not for the F2LLM quant line

Runtime details:

- llama.cpp runs with embeddings enabled
- pooling is explicit `Mean`
- attention is explicit `NonCausal`
- vectors are L2-normalized before returning
- models are loaded from an explicit `.gguf` path or from the local HF cache
- the crate never downloads from the network at runtime

## Profiles

| Profile | Model file | Dim | Approx download | Role |
|---|---:|---:|---:|---|
| `base` | `F2LLM-v2-0.6B.Q4_K_M.gguf` | 1024 | ~397 MB | portable default |
| `dev` | `F2LLM-v2-1.7B.Q4_K_M.gguf` | 2048 | ~1.1 GB | workstation tier |
| `premium` | `F2LLM-v2-1.7B.Q6_K.gguf` | 2048 | ~1.4 GB | stronger local tier |

Default repos:

```text
base:    mradermacher/F2LLM-v2-0.6B-GGUF
dev:     mradermacher/F2LLM-v2-1.7B-GGUF
premium: mradermacher/F2LLM-v2-1.7B-GGUF
```

The old MiniLM/Harrier/fp16 config values are treated as legacy. If a stale
`~/.aicx/embedder.toml` still points at one of those repos without an explicit
GGUF filename, the resolver falls back to the selected F2LLM GGUF profile
instead of trying to load an incompatible safetensors snapshot as GGUF.

## Feature Flags

Default AICX builds stay lean.

```bash
cargo build --release
cargo build --release --features native-embedder
```

Feature mapping:

- `native-embedder` — enables `aicx-embeddings/gguf`
- `native-embedder-metal` — enables the crate's Metal feature for macOS builds
- `native-embedder-openmp` — enables OpenMP when the target has a known-good toolchain

The `llama-cpp-2` dependency is pinned at `0.1.145` and uses
`default-features = false` to avoid accidental OpenMP/linker surprises in the
portable path.

## Config

`~/.aicx/embedder.toml` is the AICX native embedder config.

Recommended shape:

```toml
[native_embedder]
backend = "gguf"
profile = "base"
repo = "mradermacher/F2LLM-v2-0.6B-GGUF"
filename = "F2LLM-v2-0.6B.Q4_K_M.gguf"
prefer_embedded = false
max_length = 512
```

Explicit local file:

```toml
[native_embedder]
backend = "gguf"
path = "/absolute/path/to/F2LLM-v2-0.6B.Q4_K_M.gguf"
max_length = 512
```

Resolution order:

1. `AICX_EMBEDDER_PATH`
2. `AICX_EMBEDDER_REPO` + `AICX_EMBEDDER_FILENAME`
3. `AICX_EMBEDDER_CONFIG`
4. `$AICX_HOME/config.toml` or `[storage].home`/`config.toml`
5. `$AICX_HOME/embedder.toml` or `[storage].home`/`embedder.toml`
6. bootstrap `~/.aicx/config.toml`
7. bootstrap `~/.aicx/embedder.toml`
8. profile defaults

Persistent AICX root relocation:

```toml
[storage]
# `AICX_HOME` env still wins for one-shot commands.
# Use an absolute path or ~/...
home = "~/aicx"
```

When `AICX_HOME` is unset, AICX reads bootstrap `~/.aicx/config.toml` first
only to discover `[storage].home`. The runtime root then becomes
`$home/store`, `$home/indexed`, `$home/state`, and `$home/embeddings`.
This avoids typing `AICX_HOME=...` for every command while keeping env as the
explicit override.

Installer UX:

```bash
bash install.sh --pick-home
# or non-interactive:
bash install.sh --aicx-home="$HOME/.aicx"
```

The default root remains `~/.aicx`, so the default semantic index remains
`~/.aicx/indexed/_all/embeddings.ndjson`. Picking a custom AICX home moves the
same layout under that root: `<AICX_HOME>/indexed/_all/embeddings.ndjson`.

Useful env vars:

| Variable | Effect |
|---|---|
| `AICX_EMBEDDER_CONFIG` | explicit config file |
| `AICX_EMBEDDER_PROFILE` | `base`, `dev`, or `premium` |
| `AICX_EMBEDDER_PATH` | explicit `.gguf` file or directory |
| `AICX_EMBEDDER_REPO` | HF repo override |
| `AICX_EMBEDDER_FILENAME` | exact GGUF file in the repo |
| `AICX_EMBEDDER_MAX_LENGTH` | max tokens per text |
| `AICX_EMBEDDER_THREADS` | llama.cpp thread count |
| `AICX_EMBEDDER_GPU_LAYERS` | optional GPU offload count |
| `AICX_HF_CACHE` | extra HF cache base to search first |

`AICX_RUNTIME_PROFILE` is accepted as a compatibility alias for profile
selection, but prefer `AICX_EMBEDDER_PROFILE` for new AICX-native flows.

## Hydration

Install does not silently download a model.

Interactive picker:

```bash
bash install.sh --pick-embedder
```

Manual hydration:

```bash
hf download mradermacher/F2LLM-v2-0.6B-GGUF F2LLM-v2-0.6B.Q4_K_M.gguf
hf download mradermacher/F2LLM-v2-1.7B-GGUF F2LLM-v2-1.7B.Q4_K_M.gguf
hf download mradermacher/F2LLM-v2-1.7B-GGUF F2LLM-v2-1.7B.Q6_K.gguf
```

Lookup paths:

- `AICX_HF_CACHE`
- `HUGGINGFACE_HUB_CACHE`
- `HF_HUB_CACHE`
- `HF_HOME/hub`
- `~/.cache/huggingface/hub`
- `~/.aicx/embeddings`
- `~/.aicx/embeddings/hub`

## Hybrid Generations — One Dense Payload

Since W2-03, hybrid retrieval artifacts are generation-scoped and the dense
vectors are materialized exactly once per generation.

Layout under each bucket:

```text
<AICX_HOME>/indexed/<bucket>/embeddings.ndjson        # canonical committed semantic index (build source)
<AICX_HOME>/indexed/<bucket>/hybrid/
├── CURRENT                                           # pointer file naming the published generation
└── generations/<generation>/
    ├── tantivy_lex/                                  # lexical index
    ├── dense.exact_mmap_v1.bin                       # THE dense payload (aicx.dense.exact_mmap.v1)
    └── manifest.json                                 # generation authority, written last
```

Contract:

- **One dense payload per generation.** Vectors are written once, into the
  versioned mmap artifact. The old `hybrid/dense_brute_force.ndjson` twin
  (a near-copy of `embeddings.ndjson`, 15 GB + 15 GB on the live `_all`
  bucket) is never written by new builds; existing copies remain on disk as
  migration read input only.
- **Manifest last, pointer flip publishes.** A build writes lexical and dense
  payloads first, `manifest.json` after them, and only then atomically
  renames the `CURRENT` pointer. A build killed at any earlier boundary
  leaves an unreferenced generation directory; readers keep resolving the
  previous complete generation. Incomplete directories are quarantinable and
  never alter current-generation resolution.
- **Manifest binds the payload.** `manifest.json` carries the generation id,
  the blake3 source hash of the committed semantic index, embedder model /
  url hash / dimension / distance, dense kind + row count, and the lexical
  commit id + doc count. Validation rejects drift on source, model,
  dimension, distance, lexical generation, and partial-build divergence
  (kind or count mismatch between manifest and artifacts).
- **Legacy stores migrate on the next full build.** A bucket without a
  `CURRENT` pointer resolves to the legacy root layout for reads; the next
  `aicx index` full rebuild materializes a single-payload generation and
  publishes it. No index files are deleted by the migration.

Old generation directories accumulate until the doctor/quarantine surface
reclaims them; this cut intentionally does not delete anything.

## Dense Migration Benchmark

`tools/bench_dense_migration.sh` is the W4 falsification harness for the dense
replacement. It builds an isolated `AICX_HOME` shaped like the live store,
including exact/case-drift/bare/underscore project identities, writes the
legacy duplicate pair (`embeddings.ndjson` plus `hybrid/dense_brute_force.ndjson`),
materializes a byte-compatible `dense.exact_mmap_v1.bin`, and compares both
paths on identical global and project-filtered top-k queries.

Fast contract gate:

```bash
bash tools/bench_dense_migration.sh --verify-only
```

Production-scale falsification should keep the live `~/.aicx` read-only and
point the harness at an isolated filesystem budget, for example:

```bash
bash tools/bench_dense_migration.sh --rows 300000 --dim 4096 --queries 40 --output reports/dense-migration.json --keep
```

The hard budgets remain unchanged from the W4 brief:

- dense mmap payload <= 60% of the legacy duplicate pair
- peak RSS <= 1.25 GiB at the observed ~300k x 4096 scale
- warm project-filter p95 <= 2 s; warm global p95 <= 8 s
- exact top-k parity is 100% for equal ranking inputs
- corrupt or interrupted generation copies must leave `CURRENT` untouched

Measured verdict on 2026-07-22 (W4-01c): **OUTCOME 1 — budgets met by the
release-mode engine**. The isolated 500000×1024 corpus still produces a
2,133,416,794 byte dense mmap payload (≥2 GiB), keeps disk duplication low
(0.0971× of the legacy pair), preserves exact top-k and reverse-order parity at
1.0, and leaves `CURRENT` untouched after corrupt and interrupted copies.

The 317.87 s global / 55.00 s project numbers from the first W4-01 scale run are
**reference-model latency**, not engine latency. `run_mmap()` inside
`tools/bench_dense_migration.sh` is pure Python: it struct-unpacks and
dot-products every row in the interpreter and never executes the Rust
`MmapDenseAdapter` (debug or release). After the hot-loop repair (`16310b3`) the
unmodified harness re-ran at ~321.96 s / 55.37 s — still the Python path —
while the release-mode adapter at the same scale measured:

| Path | Warm latency | Frozen budget |
|---|---:|---:|
| global (empty filters) | 0.371–0.468 s | 8 s |
| project-scoped | 0.212–0.221 s | 2 s |

Scores stay bit-identical to the brute-force leg. The LanceDB / memex-search
contingency in `backlog/memex-search-transplant.md` stays shelved. Do not re-read
the shell harness's wall times as engine miss; do not relax the frozen budgets
in `tests/retrieval_eval/baseline.json`.

`--verify-only` is intentionally not production proof. It is the synchronous CI
gate that proves the benchmark contract, byte layout, failed-copy safety checks,
reverse-order query parity, and budget accounting still work before an operator
runs the larger isolated corpus.

## Relationship To Roost/Rust-Memex

Do not conflate config planes:

- `~/.aicx/embedder.toml` controls AICX local embeddings.
- `RUST_MEMEX_CONFIG` / `~/.rmcp-servers/rust-memex/config.toml` controls the
  Roost/rust-memex retrieval plane.

AICX local embeddings are enough for portable steering and lightweight local
retrieval. Roost/rust-memex is still the right home for premium retrieval,
operator settings, alternate providers, and larger indexing pipelines.

## Testing

```bash
cargo test -p aicx-embeddings
cargo test -p aicx-embeddings --features gguf
cargo test -p aicx --features native-embedder --test native_embedder
```

The integration test self-skips when the configured GGUF model is not present
in the local cache, so the command is safe on clean CI runners.

## Credits

Codescribe proved the in-process local model shape is comfortable in Rust.
AICX keeps that architectural lesson but switches the production model format
to GGUF because quantized F2LLM is the sharper distribution path here.
