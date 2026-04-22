# aicx Native Embeddings

> Foundation tool for the Vibecrafted framework — a lean, in-process embedder
> that can ship with the binary when explicitly embedded and otherwise
> gracefully falls back to the HuggingFace cache.

## Why

`aicx` already speaks to an external rmcp-memex embedding service over HTTP.
That path is excellent on developer workstations connected to the LibraxisAI
MLX stack, but it has three failure modes on customer machines:

1. `qwen3-embedding:8b` can push even a capable laptop to its knees — neither
   Codex nor Claude users want to run an 8B model just to semantic-search their
   own context.
2. HTTP providers add a hard dependency on reachable infrastructure. Offline
   installs, airgapped enterprise deployments, and CI environments can't wait
   on a network endpoint that might not exist.
3. Different namespaces inside a shared Lance store can drift in dimension
   while the ingest path blindly overwrites vectors — a silent corruption.

The native embedder fixes all three: it runs locally, in-process, with a
predictable memory footprint, and the compatibility checks refuse to corrupt
namespaces whose embedding truth has diverged from the current runtime.

Important shipping truth:

- current public release bundles stay slim and do **not** auto-bundle model weights
- current public release bundles do **not** silently download a heavy model during install
- native embedder selection is therefore a preference + hydration story, not a hidden payload

## Feature flag

Native embeddings live behind the `native-embedder` cargo feature. Default
builds stay lean (no Candle, no Tokenizers pulled in) so operators only pay the
compile-time and binary-size cost when they opt in:

```bash
cargo build --release --features native-embedder
```

Build profile resolution:
- `AICX_BUILD_PROFILE=base` or unset: embed `sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2`
- `AICX_BUILD_PROFILE=dev`: embed `harrier-oss/harrier-oss-0.6b`
- `AICX_BUILD_PROFILE=premium`: embed `F2-LLM/F2-LLM-v2-1.7b`
- `AICX_EMBEDDER_REPO=<owner/name>` still wins as the exact override

To keep a complete bundle under ~1.1 GB the default `base` profile uses the
conservative `sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2`
model (~224 MB fp16). Operators can opt into a stronger code-focused bundle or
an even larger local model with the build profiles above:

| Model                                  | Params | Size (fp16) | Include-in-bundle? |
|----------------------------------------|-------:|------------:|--------------------|
| MiniLM L12 multilingual (default)      | 118M   | ~224 MB     | yes                |
| `harrier-oss/harrier-oss-0.6b`         | 0.6B   | ~1.1 GB     | yes (bundle cap)   |
| `F2-LLM/F2-LLM-v2-1.7b`                | 1.7B   | ~3.4 GB     | runtime-only       |

For the 1.7B tier we recommend leaving the model in the HF cache and loading it
at runtime — embedding a 3+ GB weights file would blow past our 1.1 GB total
budget and tip the user experience into "enterprise-only" territory.

## Build-time vs runtime

There are two supply paths for the model and `aicx` transparently prefers the
sealed one when both are present:

### 1. Embedded (build-time include_bytes)

`build.rs` inspects the HuggingFace cache on the host that compiles the binary.
If a snapshot for `AICX_EMBEDDER_REPO` exists with `config.json`,
`tokenizer.json`, and a safetensors weights file, it generates
`OUT_DIR/embedded_embedder_data.rs` containing three `include_bytes!` slices
and sets the custom `aicx_embed_embedder` cfg flag.

From the consumer's point of view that means the binary is self-contained:

```rust
use aicx::embedder::{EmbedderEngine, EmbedderConfig};

let mut engine = EmbedderEngine::with_config(EmbedderConfig::from_env())?;
let vector = engine.embed("hello vibe")?;
```

No HTTP calls, no filesystem lookups, no runtime downloads — the model is
literally part of the ELF.

### 2. Runtime HF cache

When no model was embedded (developer build, missing cache at build time, or
`AICX_NO_EMBED=1`), the same API call reads from the HuggingFace cache at first
use:

```
~/.aicx/embeddings/hub/models--*/snapshots/<sha>/
~/.cache/huggingface/hub/models--*/snapshots/<sha>/
```

Cache lookup honours, in order:

1. `AICX_EMBEDDER_PATH` — explicit directory override (bypasses repo lookup)
2. `AICX_HF_CACHE`, `HUGGINGFACE_HUB_CACHE`, `HF_HUB_CACHE`, `HF_HOME/hub`
3. `~/.cache/huggingface/hub`
4. `~/.aicx/embeddings` and `~/.aicx/embeddings/hub`

The newest snapshot with all three required files wins.

## Operator config files

Two different config surfaces exist today and they should not be conflated:

1. Active memex retrieval provider config:
   - usually `~/.rmcp-servers/rust-memex/config.toml`
   - or an explicit file via `RUST_MEMEX_CONFIG`
   - this governs the current `memex-sync` HTTP/provider path
2. Native embedder preference config:
   - `~/.aicx/embedder.toml`
   - or an explicit file via `AICX_EMBEDDER_CONFIG`
   - this governs which native embedder repo/path a native-embedder build will try to load

Recommended native embedder config:

```toml
[native_embedder]
profile = "base"
repo = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
prefer_embedded = true
```

`install.sh --pick-embedder` writes that file for you without silently pulling a
heavy model into the bundle.

## Environment variables

| Variable                | Scope          | Effect                                                    |
|-------------------------|----------------|-----------------------------------------------------------|
| `AICX_EMBEDDER_REPO`    | build + runtime| HF repo id to embed / load (`owner/name`).                |
| `AICX_BUILD_PROFILE`    | build          | Build preset: `base`, `dev`, or `premium`.                |
| `AICX_EMBEDDER_PATH`    | build + runtime| Absolute path to a model directory — bypasses HF cache.   |
| `AICX_EMBEDDER_CONFIG`  | build + runtime| Explicit config file overriding `~/.aicx/embedder.toml`.  |
| `AICX_NO_EMBED=1`       | build          | Skip `include_bytes!` even if cache has the model.        |
| `AICX_HF_CACHE`         | build + runtime| Extra HF cache base to search first.                      |

All four are honoured by both `build.rs` and the runtime engine, so you never
have to remember which one is which.

## Non-destructive namespace handling

`aicx` records per-namespace metadata under `~/.aicx/memex/semantic-index-<namespace>.json`
and refuses to mix embeddings with conflicting dimensions. When the runtime
truth (embedding model, dimension, paths) diverges from what was previously
recorded for a namespace, `sync_new_chunk_paths` fails with an explicit error
suggesting `aicx memex-sync --reindex --namespace <namespace>`.

`reset_semantic_index(namespace)` is correspondingly narrow:

- If the target namespace is the **only** namespace in the Lance store, the
  full `~/.aicx/lancedb`, BM25, and sync-state paths are wiped — identical to
  the previous destructive behaviour.
- If other namespaces exist, only documents whose `namespace` column matches
  are deleted (via `StorageManager::delete_namespace_documents`). Sibling
  namespaces with a different dimension are left intact.
- Per-namespace metadata is always removed so the next ingest recomputes the
  truth from scratch.

This keeps multi-project stores alive across embedder swaps. Flipping between
MiniLM, Harrier, and F2-LLM on a single namespace is now a reindex command, not
a hand-rolled dance around `rm -rf ~/.aicx/lancedb`.

## Testing

```bash
cargo test --features native-embedder --test native_embedder
```

The integration tests self-skip when no model is available in either supply
path, so the same command is safe on clean CI runners and on loaded developer
machines. On a machine with the default MiniLM snapshot in `~/.cache/huggingface`
they verify:

- Embedded dimension hint is positive when `aicx_embed_embedder` fired.
- `embed_batch` returns the configured dimension and L2-normalised vectors.
- Identical input produces self-similarity ≥ 0.999.
- `NativeEmbeddingSource` correctly reports Embedded / HfCache / ExplicitPath.

## Relationship to `rmcp-memex`

The native embedder is **not** a full replacement for the `rmcp-memex` HTTP
path today. It is the foundation layer for Vibecrafted-native products that
need deterministic, offline-capable embeddings. Future work will expose it as
an additional `EmbeddingBackend` inside the sync pipeline so operators can
choose between HTTP (Qwen3/MLX on the LAN) and in-process (Harrier/F2-LLM on
the laptop) per project.

On the HTTP memex path, `aicx` now also exposes explicit runtime presets:
- `base` (default): 1024-dim Qwen 0.6B
- `dev`: 2560-dim Qwen 4B
- `premium`: 4096-dim Qwen 8B

Select them with `aicx memex-sync --profile ...`, `AICX_RUNTIME_PROFILE`, or
the active `rust-memex` config file (`RUST_MEMEX_CONFIG`, usually
`~/.rmcp-servers/rust-memex/config.toml`).

## Credits

Patterned after the `core/embedder` module of the sibling repo
[`CodeScribe`](../../CodeScribe), which proved that shipping a
Candle-powered BERT embedder inside a Rust binary is not only feasible but
actively pleasant.
