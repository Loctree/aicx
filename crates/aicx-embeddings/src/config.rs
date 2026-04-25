use std::env;
use std::fs;
#[cfg(feature = "gguf")]
use std::path::Path;
use std::path::PathBuf;

use tracing::debug;

use crate::{
    BackendPreference, EmbeddingConfig, EmbeddingProfile, EmbeddingProfileSpec,
    NativeEmbedderConfigFile, ResolvedEmbeddingModel,
};

pub const DEFAULT_BASE_REPO: &str = "mradermacher/F2LLM-v2-0.6B-GGUF";
pub const DEFAULT_BASE_FILENAME: &str = "F2LLM-v2-0.6B.Q4_K_M.gguf";
pub const DEFAULT_DEV_REPO: &str = "mradermacher/F2LLM-v2-1.7B-GGUF";
pub const DEFAULT_DEV_FILENAME: &str = "F2LLM-v2-1.7B.Q4_K_M.gguf";
pub const DEFAULT_PREMIUM_REPO: &str = "mradermacher/F2LLM-v2-1.7B-GGUF";
pub const DEFAULT_PREMIUM_FILENAME: &str = "F2LLM-v2-1.7B.Q6_K.gguf";

const LEGACY_MINILM_REPO: &str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2";
const LEGACY_HARRIER_REPO: &str = "microsoft/harrier-oss-v1-0.6b";
const LEGACY_F2_REPO: &str = "codefuse-ai/F2LLM-v2-1.7B";

pub fn profile_spec(profile: EmbeddingProfile) -> EmbeddingProfileSpec {
    match profile {
        EmbeddingProfile::Base => EmbeddingProfileSpec {
            profile,
            repo: DEFAULT_BASE_REPO,
            filename: DEFAULT_BASE_FILENAME,
            dimension_hint: 1024,
            approx_size: "~397 MB",
            description: "portable F2LLM 0.6B Q4_K_M",
        },
        EmbeddingProfile::Dev => EmbeddingProfileSpec {
            profile,
            repo: DEFAULT_DEV_REPO,
            filename: DEFAULT_DEV_FILENAME,
            dimension_hint: 2048,
            approx_size: "~1.1 GB",
            description: "workstation F2LLM 1.7B Q4_K_M",
        },
        EmbeddingProfile::Premium => EmbeddingProfileSpec {
            profile,
            repo: DEFAULT_PREMIUM_REPO,
            filename: DEFAULT_PREMIUM_FILENAME,
            dimension_hint: 2048,
            approx_size: "~1.4 GB",
            description: "stronger F2LLM 1.7B Q6_K",
        },
    }
}

pub fn load_from_env() -> EmbeddingConfig {
    let mut cfg = EmbeddingConfig::default();

    if let Some(file_cfg) = load_config_file() {
        apply_section(&mut cfg, file_cfg);
    }

    if let Some(backend) =
        env_string("AICX_EMBEDDER_BACKEND").and_then(|raw| BackendPreference::parse(&raw))
    {
        cfg.backend = backend;
    }
    if let Some(profile) = env_string("AICX_EMBEDDER_PROFILE")
        .or_else(|| env_string("AICX_RUNTIME_PROFILE"))
        .and_then(|raw| EmbeddingProfile::parse(&raw))
    {
        cfg.profile = profile;
    }
    if let Some(repo) = env_string("AICX_EMBEDDER_REPO") {
        cfg.repo = Some(repo);
    }
    if let Some(filename) =
        env_string("AICX_EMBEDDER_FILENAME").or_else(|| env_string("AICX_EMBEDDER_FILE"))
    {
        cfg.filename = Some(filename);
    }
    if let Some(path) = env_string("AICX_EMBEDDER_PATH") {
        cfg.model_path = Some(PathBuf::from(path));
    }
    if let Some(max_length) = env_usize("AICX_EMBEDDER_MAX_LENGTH") {
        cfg.max_length = Some(max_length);
    }
    if let Some(threads) = env_i32("AICX_EMBEDDER_THREADS") {
        cfg.threads = Some(threads);
    }
    if let Some(gpu_layers) = env_u32("AICX_EMBEDDER_GPU_LAYERS") {
        cfg.gpu_layers = Some(gpu_layers);
    }
    if let Some(prefer_embedded) = env_bool("AICX_EMBEDDER_PREFER_EMBEDDED") {
        cfg.prefer_embedded = prefer_embedded;
    }

    cfg
}

pub fn resolve_model(cfg: &EmbeddingConfig) -> ResolvedEmbeddingModel {
    let spec = profile_spec(cfg.profile);
    let raw_repo = cfg.repo.as_deref().map(str::trim).filter(|v| !v.is_empty());
    let raw_filename = cfg
        .filename
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty());
    let legacy = raw_repo.is_some_and(is_legacy_non_gguf_repo) && raw_filename.is_none();

    if legacy {
        return ResolvedEmbeddingModel {
            profile: cfg.profile,
            repo: spec.repo.to_string(),
            filename: spec.filename.to_string(),
            dimension_hint: spec.dimension_hint,
            approx_size: spec.approx_size.to_string(),
            from_legacy_repo: true,
        };
    }

    ResolvedEmbeddingModel {
        profile: cfg.profile,
        repo: raw_repo.unwrap_or(spec.repo).to_string(),
        filename: raw_filename.unwrap_or(spec.filename).to_string(),
        dimension_hint: spec.dimension_hint,
        approx_size: spec.approx_size.to_string(),
        from_legacy_repo: false,
    }
}

pub fn config_search_paths() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Some(path) = env_string("AICX_EMBEDDER_CONFIG") {
        out.push(PathBuf::from(path));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".aicx").join("embedder.toml"));
        out.push(home.join(".aicx").join("config.toml"));
    }
    out
}

fn load_config_file() -> Option<crate::NativeEmbedderConfigSection> {
    for path in config_search_paths() {
        if !path.exists() {
            continue;
        }
        let raw = match fs::read_to_string(&path) {
            Ok(raw) => raw,
            Err(err) => {
                debug!(
                    target: "aicx_embeddings::config",
                    "failed to read embedder config {}: {}",
                    path.display(),
                    err
                );
                continue;
            }
        };
        let parsed: NativeEmbedderConfigFile = match toml::from_str(&raw) {
            Ok(parsed) => parsed,
            Err(err) => {
                debug!(
                    target: "aicx_embeddings::config",
                    "failed to parse embedder config {}: {}",
                    path.display(),
                    err
                );
                continue;
            }
        };
        let mut merged = parsed.top_level;
        if let Some(section) = parsed.embedder {
            merged.merge_from(section);
        }
        if let Some(section) = parsed.native_embedder {
            merged.merge_from(section);
        }
        return Some(merged);
    }
    None
}

fn apply_section(cfg: &mut EmbeddingConfig, section: crate::NativeEmbedderConfigSection) {
    if let Some(backend) = section
        .backend
        .as_deref()
        .and_then(BackendPreference::parse)
    {
        cfg.backend = backend;
    }
    if let Some(profile) = section.profile.as_deref().and_then(EmbeddingProfile::parse) {
        cfg.profile = profile;
    }
    if let Some(repo) = non_empty(section.repo) {
        cfg.repo = Some(repo);
    }
    if let Some(filename) = non_empty(section.filename).or_else(|| non_empty(section.file)) {
        cfg.filename = Some(filename);
    }
    if section.path.is_some() {
        cfg.model_path = section.path;
    }
    if section.prefer_embedded.is_some() {
        cfg.prefer_embedded = section.prefer_embedded.unwrap_or(false);
    }
    if section.max_length.is_some() {
        cfg.max_length = section.max_length;
    }
    if section.threads.is_some() {
        cfg.threads = section.threads;
    }
    if section.gpu_layers.is_some() {
        cfg.gpu_layers = section.gpu_layers;
    }
}

pub fn find_cached_model_file(repo: &str, filename: &str) -> Option<PathBuf> {
    crate::hf_cache::find_snapshot_with_file(repo, filename).map(|snapshot| snapshot.join(filename))
}

#[cfg(feature = "gguf")]
pub fn resolve_explicit_model_path(path: &Path, filename: Option<&str>) -> Option<PathBuf> {
    if path.is_file() {
        return Some(path.to_path_buf());
    }
    if !path.is_dir() {
        return None;
    }
    if let Some(filename) = filename {
        let candidate = path.join(filename);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    let mut candidates = fs::read_dir(path)
        .ok()?
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("gguf"))
        })
        .collect::<Vec<_>>();
    candidates.sort();
    candidates.into_iter().next()
}

fn is_legacy_non_gguf_repo(repo: &str) -> bool {
    let repo = repo.trim();
    repo.eq_ignore_ascii_case(LEGACY_MINILM_REPO)
        || repo.eq_ignore_ascii_case(LEGACY_HARRIER_REPO)
        || repo.eq_ignore_ascii_case(LEGACY_F2_REPO)
}

fn non_empty(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_string(name: &str) -> Option<String> {
    env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn env_usize(name: &str) -> Option<usize> {
    env_string(name).and_then(|value| value.parse().ok())
}

fn env_i32(name: &str) -> Option<i32> {
    env_string(name).and_then(|value| value.parse().ok())
}

fn env_u32(name: &str) -> Option<u32> {
    env_string(name).and_then(|value| value.parse().ok())
}

fn env_bool(name: &str) -> Option<bool> {
    env_string(name).and_then(|value| match value.to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    })
}
