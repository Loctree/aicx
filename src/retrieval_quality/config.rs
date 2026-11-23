// 𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI

use std::{
    env,
    fs::OpenOptions,
    io::{ErrorKind, Read},
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{Result, anyhow, bail};
use reqwest::Url;
use serde::Deserialize;

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum Protocol {
    #[default]
    Responses,
    ChatCompletions,
}

#[derive(Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Table {
    #[serde(rename = "enabled")]
    _enabled: bool,
    url: String,
    model: String,
    api_key_env: Option<String>,
    timeout_secs: Option<u64>,
    max_candidates: Option<usize>,
    max_doc_bytes: Option<usize>,
    protocol: Protocol,
    max_output_tokens: Option<usize>,
    max_refinements: Option<u8>,
}

pub(super) struct Endpoint {
    pub url: Url,
    pub model: String,
    pub api_key_env: Option<String>,
    pub timeout: Duration,
    pub max_candidates: usize,
    pub max_doc_bytes: usize,
    pub protocol: Protocol,
    pub max_output_tokens: usize,
    pub max_refinements: u8,
}

pub(super) enum Setting {
    Disabled,
    Configured(Endpoint),
    Invalid(String),
}

impl Setting {
    fn resolve(value: Result<Option<Endpoint>>) -> Self {
        match value {
            Ok(Some(endpoint)) => Self::Configured(endpoint),
            Ok(None) => Self::Disabled,
            Err(err) => Self::Invalid(err.to_string()),
        }
    }

    fn inspection(&self) -> serde_json::Value {
        match self {
            Self::Configured(endpoint) => {
                serde_json::json!({"enabled": true, "model": endpoint.model})
            }
            Self::Disabled => serde_json::json!({"enabled": false}),
            Self::Invalid(detail) => {
                serde_json::json!({"status": "failed", "failure_detail": detail})
            }
        }
    }
}

pub struct QualityConfig {
    pub(super) reranker: Setting,
    pub(super) decision: Setting,
}

fn read_config(home: &Path) -> Result<Option<toml::Value>> {
    let explicit = env::var("AICX_EMBEDDER_CONFIG").ok();
    let bootstrap = crate::os_user_home()
        .filter(|_| env::var_os("AICX_HOME").is_none_or(|value| value.is_empty()))
        .map(|p| p.join(".aicx/config.toml"));
    let paths = [
        explicit
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(PathBuf::from),
        Some(home.join("config.toml")),
        bootstrap,
    ];
    for path in paths.into_iter().flatten() {
        // This startup-configured path may live outside session/source roots.
        // It never comes from a search parameter. Use the same explicit read-only
        // open and 1 MiB cap as the canonical embedder config reader.
        const MAX_BYTES: u64 = 1024 * 1024;
        let file = match OpenOptions::new().read(true).open(&path) {
            Ok(file) => file,
            Err(err) if err.kind() == ErrorKind::NotFound => continue,
            Err(_) => bail!("cannot open AICX config; check the path and permissions"),
        };
        let mut text = String::new();
        file.take(MAX_BYTES + 1)
            .read_to_string(&mut text)
            .map_err(|_| anyhow!("cannot read AICX config as UTF-8"))?;
        if text.len() as u64 > MAX_BYTES {
            bail!("AICX config exceeds the 1 MiB limit");
        }
        return toml::from_str(&text)
            .map(Some)
            .map_err(|_| anyhow!("invalid TOML in AICX config; check config.toml syntax"));
    }
    Ok(None)
}

fn endpoint(
    retrieval: Option<&toml::Value>,
    name: &str,
    requested: Option<bool>,
) -> Result<Option<Endpoint>> {
    let section = retrieval.and_then(|r| r.get(name));
    if requested == Some(false) || (requested.is_none() && section.is_none()) {
        return Ok(None);
    }
    let label = format!("[retrieval.{name}]");
    let section = section.ok_or_else(|| anyhow!("configure {label} with url and model"))?;
    let enabled = section
        .get("enabled")
        .map(|value| {
            value
                .as_bool()
                .ok_or_else(|| anyhow!("{label}.enabled must be boolean"))
        })
        .transpose()?;
    if !requested.unwrap_or(enabled.unwrap_or(false)) {
        return Ok(None);
    }
    let cfg: Table = section
        .clone()
        .try_into()
        .map_err(|_| anyhow!("invalid {label} fields; check protocol, limits and value types"))?;
    let url =
        Url::parse(&cfg.url).map_err(|_| anyhow!("set {label}.url to a complete HTTP endpoint"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("set {label}.url to an HTTP endpoint without userinfo or fragment");
    }
    let model = cfg.model.trim();
    if model.is_empty() || model.len() > 256 || model.chars().any(char::is_control) {
        bail!("set {label}.model to a nonempty model identifier of at most 256 bytes");
    }
    if let Some(key) = &cfg.api_key_env
        && (key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            || key.as_bytes()[0].is_ascii_digit())
    {
        bail!("{label}.api_key_env must name an environment variable");
    }
    let decision = name == "decision";
    let (timeout, ceiling, candidates, bytes) = if decision {
        (30, 180, 12, 2048)
    } else {
        (15, 120, 50, 4096)
    };
    Ok(Some(Endpoint {
        url,
        model: model.to_owned(),
        api_key_env: cfg.api_key_env,
        timeout: Duration::from_secs(cfg.timeout_secs.unwrap_or(timeout).clamp(1, ceiling)),
        max_candidates: cfg
            .max_candidates
            .unwrap_or(candidates)
            .clamp(1, candidates),
        max_doc_bytes: cfg.max_doc_bytes.unwrap_or(bytes).clamp(1, bytes),
        protocol: cfg.protocol,
        max_output_tokens: cfg.max_output_tokens.unwrap_or(512).clamp(1, 2048),
        max_refinements: cfg.max_refinements.unwrap_or(1).min(1),
    }))
}

impl QualityConfig {
    pub fn load(home: &Path, rerank: Option<bool>, decide: Option<bool>) -> Self {
        let file = read_config(home);
        let resolve = |name, requested| match &file {
            Ok(file) => Setting::resolve(endpoint(
                file.as_ref().and_then(|v| v.get("retrieval")),
                name,
                requested,
            )),
            Err(_) if requested == Some(false) => Setting::Disabled,
            Err(err) => Setting::Invalid(err.to_string()),
        };
        Self {
            reranker: resolve("reranker", rerank),
            decision: resolve("decision", decide),
        }
    }

    pub fn active(&self) -> bool {
        !matches!(self.reranker, Setting::Disabled) || !matches!(self.decision, Setting::Disabled)
    }

    /// Inspection never resolves bearer credentials or displays endpoint URLs.
    pub fn inspection(&self) -> serde_json::Value {
        serde_json::json!({"reranker": self.reranker.inspection(), "decision": self.decision.inspection()})
    }
}
