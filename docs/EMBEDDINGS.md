# AICX Native Embeddings

> Foundation layer for Vibecrafted products: local, explicit, portable
> embeddings without turning every install into a multi-GB surprise.

## Product Truth

AICX now treats the native embedder as its first-choice local embedding path,
not as a fallback behind memex.

The split is:

- **AICX** owns the canonical corpus, steering surfaces, MCP front door, and a
  reusable local embedding library.
- **Roost/rust-memex** owns the advanced retrieval/operator plane: heavier
  provider routing, richer indexing, and premium retrieval workflows.

This removes the old schizophrenia where AICX pretended to be memex while also
depending on memex internals for the same job.

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
- CodeScribe's Candle path is a good architectural precedent, but it was built
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
4. `~/.aicx/embedder.toml`
5. `~/.aicx/config.toml`
6. profile defaults

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

CodeScribe proved the in-process local model shape is comfortable in Rust.
AICX keeps that architectural lesson but switches the production model format
to GGUF because quantized F2LLM is the sharper distribution path here.
