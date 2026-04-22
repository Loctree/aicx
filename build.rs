//! Build script for aicx native embedder.
//!
//! Generates `embedded_embedder_data.rs` in OUT_DIR when the `native-embedder`
//! feature is active AND the model is present in the HuggingFace cache.
//! Without cache hits the build still succeeds; the runtime path falls back to
//! reading from HF cache at load time.
//!
//! Controls:
//!   AICX_BUILD_PROFILE — embedder build preset: base (default), dev, premium
//!   AICX_EMBEDDER_REPO  — HF repo (default: sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2)
//!   AICX_EMBEDDER_PATH  — explicit model directory (overrides HF cache)
//!   AICX_NO_EMBED       — set to `1` to skip embedding even when available
//!
//! Vibecrafted with AI Agents by VetCoders (c)2026 VetCoders

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

/// Default embedder repository. Conservative choice that fits the 1.1 GB bundle
/// budget when embedded (fp16 ~224 MB). Operators can swap in suggested
/// alternatives via `AICX_EMBEDDER_REPO`:
///   harrier-oss/harrier-oss-0.6b     (~0.6B params, code-focused)
///   F2-LLM/F2-LLM-v2-1.7b            (~1.7B params, larger recall budget)
const DEFAULT_EMBEDDER_REPO: &str = "sentence-transformers/paraphrase-multilingual-MiniLM-L12-v2";
const DEV_EMBEDDER_REPO: &str = "harrier-oss/harrier-oss-0.6b";
const PREMIUM_EMBEDDER_REPO: &str = "F2-LLM/F2-LLM-v2-1.7b";

#[derive(Clone, Copy)]
enum BuildProfile {
    Base,
    Dev,
    Premium,
}

impl BuildProfile {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "base" => Some(Self::Base),
            "dev" => Some(Self::Dev),
            "premium" => Some(Self::Premium),
            _ => None,
        }
    }

    fn repo(self) -> &'static str {
        match self {
            Self::Base => DEFAULT_EMBEDDER_REPO,
            Self::Dev => DEV_EMBEDDER_REPO,
            Self::Premium => PREMIUM_EMBEDDER_REPO,
        }
    }
}

fn resolve_embedder_repo() -> String {
    if let Ok(repo) = env::var("AICX_EMBEDDER_REPO") {
        let repo = repo.trim();
        if !repo.is_empty() {
            return repo.to_string();
        }
    }

    match env::var("AICX_BUILD_PROFILE") {
        Ok(raw) => match BuildProfile::parse(&raw) {
            Some(profile) => profile.repo().to_string(),
            None => {
                println!(
                    "cargo:warning=aicx: unknown AICX_BUILD_PROFILE='{}'. Falling back to base profile.",
                    raw
                );
                DEFAULT_EMBEDDER_REPO.to_string()
            }
        },
        Err(_) => DEFAULT_EMBEDDER_REPO.to_string(),
    }
}

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-env-changed=AICX_BUILD_PROFILE");
    println!("cargo:rerun-if-env-changed=AICX_EMBEDDER_REPO");
    println!("cargo:rerun-if-env-changed=AICX_EMBEDDER_PATH");
    println!("cargo:rerun-if-env-changed=AICX_NO_EMBED");
    println!("cargo:rerun-if-env-changed=CARGO_FEATURE_NATIVE_EMBEDDER");

    // Tell rustc to accept our custom cfg flag when analyzing the crate.
    println!("cargo:rustc-check-cfg=cfg(aicx_embed_embedder)");

    let feature_enabled = env::var_os("CARGO_FEATURE_NATIVE_EMBEDDER").is_some();
    if !feature_enabled {
        return;
    }

    if env::var_os("AICX_NO_EMBED").is_some() {
        println!("cargo:warning=aicx: AICX_NO_EMBED set, skipping embedder include_bytes");
        return;
    }

    let out_dir = env::var("OUT_DIR").expect("OUT_DIR should be set by cargo");
    let repo = resolve_embedder_repo();

    let model_path = resolve_model_path(repo.as_str());
    let Some(model_path) = model_path else {
        println!(
            "cargo:warning=aicx: embedder model not found in HF cache for '{}'. Runtime HF-cache lookup will be used.",
            repo
        );
        return;
    };

    let config = model_path.join("config.json");
    let tokenizer = model_path.join("tokenizer.json");
    let weights = resolve_weights_path(&model_path);

    let Some(weights) = weights else {
        println!(
            "cargo:warning=aicx: embedder snapshot missing model.safetensors in {}. Runtime fallback active.",
            model_path.display()
        );
        return;
    };

    if !config.exists() || !tokenizer.exists() {
        println!(
            "cargo:warning=aicx: embedder snapshot incomplete (missing config.json or tokenizer.json) in {}. Runtime fallback active.",
            model_path.display()
        );
        return;
    }

    println!("cargo:rerun-if-changed={}", config.display());
    println!("cargo:rerun-if-changed={}", tokenizer.display());
    println!("cargo:rerun-if-changed={}", weights.display());

    let dest_path = Path::new(&out_dir).join("embedded_embedder_data.rs");
    let content = format!(
        r#"
pub static CONFIG: &[u8] = include_bytes!(r"{config_path}");
pub static TOKENIZER: &[u8] = include_bytes!(r"{tokenizer_path}");
pub static WEIGHTS: &[u8] = include_bytes!(r"{weights_path}");
pub static REPO: &str = "{repo}";
"#,
        config_path = config.display(),
        tokenizer_path = tokenizer.display(),
        weights_path = weights.display(),
        repo = repo,
    );

    fs::write(&dest_path, content).expect("write embedded_embedder_data.rs");
    println!("cargo:rustc-cfg=aicx_embed_embedder");
    println!(
        "cargo:warning=aicx: embedding {} from {}",
        repo,
        model_path.display()
    );
}

fn resolve_model_path(repo: &str) -> Option<PathBuf> {
    if let Ok(explicit) = env::var("AICX_EMBEDDER_PATH") {
        let p = PathBuf::from(explicit.trim());
        if p.join("config.json").exists() {
            return Some(p);
        }
    }

    for base in hf_cache_bases() {
        if let Some(snapshot) = find_snapshot_in_base(&base, repo) {
            return Some(snapshot);
        }
    }
    None
}

fn resolve_weights_path(model_path: &Path) -> Option<PathBuf> {
    for candidate in [
        "model.safetensors",
        "pytorch_model.safetensors",
        "weights.safetensors",
    ] {
        let p = model_path.join(candidate);
        if p.exists() {
            return Some(p);
        }
    }
    None
}

fn hf_cache_bases() -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(path) = env::var("AICX_HF_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HUGGINGFACE_HUB_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HF_HUB_CACHE") {
        out.push(PathBuf::from(path));
    }
    if let Ok(path) = env::var("HF_HOME") {
        out.push(PathBuf::from(path).join("hub"));
    }
    if let Some(home) = dirs::home_dir() {
        out.push(home.join(".cache").join("huggingface").join("hub"));
        out.push(home.join(".aicx").join("embeddings"));
        out.push(home.join(".aicx").join("embeddings").join("hub"));
    }
    out.sort();
    out.dedup();
    out
}

fn find_snapshot_in_base(base: &Path, repo: &str) -> Option<PathBuf> {
    let repo_dir = base.join(format!("models--{}", repo.replace('/', "--")));
    let snapshots_dir = repo_dir.join("snapshots");

    let snapshots_dir = if snapshots_dir.exists() {
        snapshots_dir
    } else {
        let target = repo.to_ascii_lowercase();
        let mut matched: Option<PathBuf> = None;
        if let Ok(entries) = fs::read_dir(base) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if !name.starts_with("models--") {
                    continue;
                }
                let repo_id = name
                    .strip_prefix("models--")
                    .unwrap_or("")
                    .replace("--", "/");
                if repo_id.to_ascii_lowercase() == target {
                    matched = Some(entry.path().join("snapshots"));
                    break;
                }
            }
        }
        matched?
    };

    let entries = fs::read_dir(&snapshots_dir).ok()?;
    let mut best: Option<(std::time::SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let modified = entry
            .metadata()
            .and_then(|m| m.modified())
            .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
        match &best {
            Some((best_time, _)) if *best_time >= modified => {}
            _ => best = Some((modified, path)),
        }
    }

    best.map(|(_, p)| p)
}
