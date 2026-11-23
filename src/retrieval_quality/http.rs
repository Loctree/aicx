// 𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI

use anyhow::{Result, anyhow, bail};
use reqwest::{Client, Error, header, redirect};
use serde::de::DeserializeOwned;

use super::config::Endpoint;

const MAX_RESPONSE: usize = 256 * 1024;

fn transport_error(error: Error) -> anyhow::Error {
    // reqwest's Display may contain a credential-bearing URL.
    if error.is_timeout() {
        anyhow!("endpoint timed out")
    } else {
        anyhow!("endpoint request failed")
    }
}

pub(super) async fn post<T: DeserializeOwned>(
    endpoint: &Endpoint,
    payload: &serde_json::Value,
) -> Result<T> {
    let client = Client::builder()
        .redirect(redirect::Policy::none())
        .build()
        .map_err(|_| anyhow!("cannot initialize HTTP client"))?;
    let mut request = client
        .post(endpoint.url.clone())
        .timeout(endpoint.timeout)
        .header(header::CONTENT_TYPE, "application/json")
        .body(serde_json::to_vec(payload)?);
    if let Some(key) = endpoint
        .api_key_env
        .as_ref()
        .and_then(|name| std::env::var(name).ok())
        .filter(|key| !key.is_empty())
    {
        request = request.bearer_auth(key);
    }
    let mut response = request.send().await.map_err(transport_error)?;
    if !response.status().is_success() {
        bail!("endpoint returned HTTP {}", response.status().as_u16());
    }
    let body = match response.content_length() {
        Some(size) if size > MAX_RESPONSE as u64 => bail!("response exceeds byte limit"),
        Some(_) => response.bytes().await.map_err(transport_error)?.to_vec(),
        None => {
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
                if chunk.len() > MAX_RESPONSE - body.len() {
                    bail!("response exceeds byte limit");
                }
                body.extend_from_slice(&chunk);
            }
            body
        }
    };
    serde_json::from_slice(&body).map_err(|_| anyhow!("invalid endpoint response schema"))
}
