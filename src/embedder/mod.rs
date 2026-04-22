//! Native embedder foundation for aicx and the Vibecrafted framework.
//!
//! Provides a BERT-style text embedder that runs fully in-process via Candle.
//! Two provisioning modes are supported:
//!
//! - **Embedded (build-time)** — weights and tokenizer are sealed into the
//!   binary via `include_bytes!` when `native-embedder` feature is active and
//!   the model is present in the HuggingFace cache at build time.
//! - **Runtime HF cache** — if no embedded model is present, the embedder is
//!   hydrated from the user's HuggingFace cache at first use. `AICX_EMBEDDER_REPO`
//!   and `AICX_EMBEDDER_PATH` control which model is loaded. Persistent
//!   operator preferences can also live in `~/.aicx/embedder.toml` or a file
//!   pointed to by `AICX_EMBEDDER_CONFIG`.
//!
//! The module is compiled only when the `native-embedder` feature is enabled so
//! that default builds stay lean (no Candle, no tokenizers crate pulled in).
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

#![cfg(feature = "native-embedder")]

mod embedded;
mod engine;

pub use embedded::{EmbeddedModel, embedded_dimension, is_embedded_available};
pub use engine::{EmbedderConfig, EmbedderEngine, NativeEmbeddingSource};
