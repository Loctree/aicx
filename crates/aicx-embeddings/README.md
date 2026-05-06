# aicx-embeddings

Reusable local embedding providers for AICX and, later, rust-memex.

The default production direction is GGUF/F2LLM loaded from a local file or
HuggingFace cache. The crate does not download models on its own and does not
silently bundle model bytes into downstream binaries.
