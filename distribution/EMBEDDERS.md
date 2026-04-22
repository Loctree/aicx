# Embedder Distribution Contract

This document is the cross-repo source of truth for how VetCoders products
should ship optional embedders without turning every binary into a giant blob.

## Product truth

There are two separate concerns and they must stay separate:

1. Shipping the product binary.
2. Hydrating an optional local embedding model.

Default rule:

- the product binary stays lean
- model weights are not bundled automatically
- install does not silently download a heavy model behind the user's back

## Operator-visible flow

Recommended user journey:

1. Install the product bundle or package.
2. Optionally run an installer picker.
3. The picker writes a small deterministic config file.
4. The picker may optionally prime the local model cache, but only with explicit user consent.
5. Runtime loads the configured model from embedded bytes or from the local cache.

## Config contract

Native embedder preference file:

- `~/.aicx/embedder.toml`
- or override with `AICX_EMBEDDER_CONFIG`

Suggested file shape:

```toml
[native_embedder]
profile = "base"
repo = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2"
prefer_embedded = true
```

Optional explicit path:

```toml
[native_embedder]
path = "/absolute/path/to/model-snapshot"
prefer_embedded = true
```

## Resolution order

For native embedder selection:

1. `AICX_EMBEDDER_PATH`
2. `AICX_EMBEDDER_REPO`
3. `AICX_EMBEDDER_CONFIG`
4. `~/.aicx/embedder.toml`
5. `~/.aicx/config.toml`
6. build/runtime defaults

For active memex HTTP/provider retrieval, use the runtime provider config of
the retrieval engine itself. In `aicx` today that is `rust-memex`, typically:

- `~/.rmcp-servers/rust-memex/config.toml`
- or `RUST_MEMEX_CONFIG`

Do not pretend these are the same file. They govern different layers.

## Download timing

Model hydration should happen in exactly one of these visible moments:

1. Before build:
   - the operator primes HuggingFace cache manually
   - build embeds bytes if the repo and feature flags allow it
2. During install:
   - only if the picker explicitly asks and the operator says yes
3. After install:
   - operator runs the documented hydration command explicitly
4. First runtime use:
   - allowed only when the runtime knows how to read from an already-present local cache
   - runtime should not silently fetch multi-GB payloads from the network

## Non-goals

- no hidden multi-GB download in package postinstall
- no surprise binary size explosion
- no mixing "active retrieval backend config" with "native embedder preference"
- no silent fallback that makes users think one embedder ran while another actually did
