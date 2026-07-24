//! Cloud embedding provider — OpenAI-compatible `/v1/embeddings` HTTP backend.
//!
//! Schema: POST `{url}` with header `Authorization: Bearer ${env_var}` and
//! JSON body `{"model": "...", "input": ["text1", "text2"]}`. Response is
//! `{"data": [{"embedding": [floats]}, ...]}`. Many providers honor this
//! contract — OpenAI, Voyage AI, Together, LM Studio cloud, vLLM, plus
//! various OpenAI-proxy gateways (OpenRouter, LiteLLM).
//!
//! API key resolution is **env-var based**: secrets never sit in the
//! config file. The config holds only the var name (e.g.
//! `api_key_env = "OPENAI_API_KEY"`), and the provider reads
//! `std::env::var(...)` per request. That keeps `~/.aicx/config.toml`
//! safe to commit, sync, or share without leaking credentials.
//!
//! Vibecrafted with AI Agents by Vetcoders (c)2026 Vetcoders

use std::collections::HashMap;

use anyhow::{Result, anyhow};
use serde::{Deserialize, Serialize};

/// Default request timeout when not pinned in config.
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Default OpenAI text-embedding-3-small dimension. Used as a fallback
/// when the operator does not pin a dimension in the config; many
/// providers do not echo dimension in the response, so the trust source
/// is the operator declaration.
pub const DEFAULT_CLOUD_DIMENSION: usize = 1536;

/// Default number of chunk texts embedded per HTTP request during an
/// `aicx index` build. OpenAI-compatible `/v1/embeddings` accepts an
/// `input` array, so one round-trip amortizes across the whole batch.
/// 16 is a conservative default: large enough to collapse ~10× of the
/// per-request latency (measured warm on a local Ollama endpoint) yet
/// small enough to keep the POST body and provider-side memory bounded.
/// Override per-config with `[embedder.cloud] batch_size` or per-run with
/// `AICX_EMBED_BATCH`.
pub const DEFAULT_CLOUD_EMBED_BATCH: usize = 16;

/// Cloud embedding endpoint configuration.
///
/// All fields are deserialized from `[embedder.cloud]` in
/// `~/.aicx/config.toml` (or `[cloud]` inside `[embedder]`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct CloudEmbeddingConfig {
    /// Endpoint URL (e.g. `https://api.openai.com/v1/embeddings`).
    #[serde(default)]
    pub url: String,
    /// Model identifier (e.g. `"text-embedding-3-small"`).
    #[serde(default)]
    pub model: String,
    /// Env var name holding the API key (e.g. `"OPENAI_API_KEY"`).
    /// Resolved at call time so secrets never sit in config files.
    #[serde(default)]
    pub api_key_env: Option<String>,
    /// Additional headers to send with each request. Useful for
    /// providers that require non-standard auth or routing headers.
    #[serde(default)]
    pub headers: HashMap<String, String>,
    /// Operator-declared output dimension. Defaults to 1536 (OpenAI
    /// text-embedding-3-small). Override when using providers with a
    /// different vector size.
    #[serde(default)]
    pub dimension: Option<usize>,
    /// Request timeout in seconds. Default 30.
    #[serde(default)]
    pub timeout_secs: Option<u64>,
    /// Number of chunk texts to embed per HTTP request during an index
    /// build. `None` falls back to [`DEFAULT_CLOUD_EMBED_BATCH`]. The
    /// `AICX_EMBED_BATCH` env var overrides this at runtime. Values below
    /// 1 are clamped to 1 (serial embedding).
    #[serde(default)]
    pub batch_size: Option<usize>,
}

impl CloudEmbeddingConfig {
    /// Returns the effective dimension after applying defaults.
    pub fn effective_dimension(&self) -> usize {
        self.dimension.unwrap_or(DEFAULT_CLOUD_DIMENSION)
    }

    /// Returns the effective timeout after applying defaults.
    pub fn effective_timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(DEFAULT_TIMEOUT_SECS).max(1)
    }

    /// Validate the operator-facing fields and return a clear error if
    /// the configuration is incomplete. The check is intentionally
    /// strict so misconfiguration surfaces before the first HTTP call.
    pub fn validate(&self) -> Result<()> {
        if self.url.trim().is_empty() {
            return Err(anyhow!(
                "cloud embedder url is empty; set [embedder.cloud].url in ~/.aicx/config.toml"
            ));
        }
        if self.model.trim().is_empty() {
            return Err(anyhow!(
                "cloud embedder model is empty; set [embedder.cloud].model in ~/.aicx/config.toml"
            ));
        }
        Ok(())
    }
}

#[cfg(feature = "cloud")]
mod cloud_impl {
    use super::*;
    use std::time::Duration;

    use anyhow::Context;
    use reqwest::blocking::Client;

    use crate::{
        EmbeddingModelInfo, EmbeddingProfile, LocalEmbeddingProvider, NativeEmbeddingSource,
    };

    /// Stateful cloud embedder. Holds the HTTP client (with keep-alive
    /// to amortize TLS handshake) plus the resolved config and info.
    pub struct CloudEmbeddingProvider {
        info: EmbeddingModelInfo,
        config: CloudEmbeddingConfig,
        client: Client,
    }

    impl CloudEmbeddingProvider {
        pub fn new(config: CloudEmbeddingConfig) -> Result<Self> {
            config.validate()?;
            let client = Client::builder()
                .timeout(Duration::from_secs(config.effective_timeout_secs()))
                .build()
                .context("failed to build cloud embedder HTTP client")?;
            let info = EmbeddingModelInfo {
                model_id: config.model.clone(),
                dimension: config.effective_dimension(),
                backend: "cloud".to_string(),
                profile: EmbeddingProfile::Base,
                source: NativeEmbeddingSource::CloudEndpoint(config.url.clone()),
            };
            Ok(Self {
                info,
                config,
                client,
            })
        }

        fn resolve_api_key(&self) -> Result<Option<String>> {
            match &self.config.api_key_env {
                Some(env_name) => match std::env::var(env_name) {
                    Ok(value) if !value.trim().is_empty() => Ok(Some(value)),
                    Ok(_) => Err(anyhow!("env var {} is set but empty", env_name)),
                    Err(_) => Err(anyhow!(
                        "env var {} is not set; export it before running aicx, or remove api_key_env from config to send requests unauthenticated",
                        env_name
                    )),
                },
                None => Ok(None),
            }
        }
    }

    #[derive(Deserialize)]
    struct EmbeddingsResponse {
        data: Vec<EmbeddingsEntry>,
    }

    #[derive(Deserialize)]
    struct EmbeddingsEntry {
        embedding: Vec<f32>,
    }

    impl LocalEmbeddingProvider for CloudEmbeddingProvider {
        fn info(&self) -> &EmbeddingModelInfo {
            &self.info
        }

        fn embed_batch(&mut self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            if texts.is_empty() {
                return Ok(Vec::new());
            }
            // D-9: cap each input before serializing the HTTP body so a
            // pathological caller cannot pin a remote endpoint with a 200 MB
            // POST. Whitespace-only inputs also fail-fast here.
            for (idx, text) in texts.iter().enumerate() {
                crate::enforce_embed_input_budget(text)
                    .with_context(|| format!("cloud embed input #{idx} rejected"))?;
            }
            let api_key = self.resolve_api_key()?;
            let body = serde_json::json!({
                "model": self.config.model,
                "input": texts,
            });
            let mut req = self
                .client
                .post(&self.config.url)
                .header("Content-Type", "application/json")
                .json(&body);
            if let Some(key) = api_key {
                req = req.header("Authorization", format!("Bearer {key}"));
            }
            for (k, v) in &self.config.headers {
                req = req.header(k, v);
            }
            let resp = req
                .send()
                .with_context(|| format!("cloud embedder POST {} failed", self.config.url))?;
            let status = resp.status();
            if !status.is_success() {
                let body = resp.text().unwrap_or_default();
                let snippet: String = body.chars().take(500).collect();
                return Err(anyhow!(
                    "cloud embedder returned HTTP {}: {}",
                    status,
                    snippet
                ));
            }
            let parsed: EmbeddingsResponse = resp
                .json()
                .context("failed to parse OpenAI-compat embeddings response")?;
            if parsed.data.is_empty() {
                return Err(anyhow!("cloud embedder returned empty `data` array"));
            }
            if parsed.data.len() != texts.len() {
                return Err(anyhow!(
                    "cloud embedder returned {} embeddings for {} inputs",
                    parsed.data.len(),
                    texts.len()
                ));
            }
            Ok(parsed.data.into_iter().map(|e| e.embedding).collect())
        }
    }
}

#[cfg(feature = "cloud")]
pub use cloud_impl::CloudEmbeddingProvider;

#[cfg(feature = "cloud")]
pub fn probe(url: &str, model: &str) -> std::result::Result<String, String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("failed to build client: {e}"))?;

    let body = serde_json::json!({
        "model": model,
        "input": ["aicx doctor probe"],
    });

    let resp = client
        .post(url)
        .json(&body)
        .send()
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if status.is_client_error() || status.is_server_error() {
        return Err(format!("HTTP {}", status));
    }

    // We can't easily parse EmbeddingsResponse without duplicating it or making it pub in cloud_impl.
    // Let's just parse it as serde_json::Value
    let parsed: serde_json::Value = resp.json().map_err(|e| e.to_string())?;
    let data = parsed
        .get("data")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing data array".to_string())?;
    if data.is_empty() {
        return Err("empty data array".to_string());
    }
    let embedding = data[0]
        .get("embedding")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "missing embedding array".to_string())?;
    if embedding.is_empty() {
        return Err("zero dimension".to_string());
    }
    Ok(format!("HTTP {} (dim: {})", status, embedding.len()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_rejects_empty_url() {
        let cfg = CloudEmbeddingConfig {
            url: String::new(),
            model: "text-embedding-3-small".into(),
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("url"));
        assert!(err.contains("config.toml"));
    }

    #[test]
    fn validate_rejects_empty_model() {
        let cfg = CloudEmbeddingConfig {
            url: "https://api.example.com/v1/embeddings".into(),
            model: String::new(),
            ..Default::default()
        };
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("model"));
    }

    #[test]
    fn validate_accepts_minimal_complete_config() {
        let cfg = CloudEmbeddingConfig {
            url: "https://api.openai.com/v1/embeddings".into(),
            model: "text-embedding-3-small".into(),
            ..Default::default()
        };
        assert!(cfg.validate().is_ok());
    }

    #[test]
    fn dimension_falls_back_to_openai_default() {
        let cfg = CloudEmbeddingConfig::default();
        assert_eq!(cfg.effective_dimension(), DEFAULT_CLOUD_DIMENSION);
    }

    #[test]
    fn dimension_uses_operator_value_when_set() {
        let cfg = CloudEmbeddingConfig {
            dimension: Some(4096),
            ..Default::default()
        };
        assert_eq!(cfg.effective_dimension(), 4096);
    }

    #[test]
    fn timeout_clamps_zero_to_one() {
        let cfg = CloudEmbeddingConfig {
            timeout_secs: Some(0),
            ..Default::default()
        };
        assert_eq!(cfg.effective_timeout_secs(), 1);
    }

    #[test]
    fn timeout_default_is_thirty_secs() {
        let cfg = CloudEmbeddingConfig::default();
        assert_eq!(cfg.effective_timeout_secs(), DEFAULT_TIMEOUT_SECS);
    }

    #[test]
    fn cloud_config_round_trips_toml() {
        let raw = r#"
url = "https://api.openai.com/v1/embeddings"
model = "text-embedding-3-small"
api_key_env = "OPENAI_API_KEY"
dimension = 1536
timeout_secs = 60

[headers]
"X-Trace" = "vetcoders"
"#;
        let cfg: CloudEmbeddingConfig = toml::from_str(raw).expect("parse cloud config");
        assert_eq!(cfg.url, "https://api.openai.com/v1/embeddings");
        assert_eq!(cfg.model, "text-embedding-3-small");
        assert_eq!(cfg.api_key_env.as_deref(), Some("OPENAI_API_KEY"));
        assert_eq!(cfg.dimension, Some(1536));
        assert_eq!(cfg.timeout_secs, Some(60));
        assert_eq!(
            cfg.headers.get("X-Trace").map(String::as_str),
            Some("vetcoders")
        );
    }
}
