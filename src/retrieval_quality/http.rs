// 𝚅𝚒𝚋𝚎𝚌𝚛𝚊𝚏𝚝𝚎𝚍. with AI Agents by Vetcoders (c)2024-2026 LibraxisAI

use anyhow::{Result, anyhow, bail};
use reqwest::{Client, Error, Response, header, redirect};
use serde::{Deserialize, de::DeserializeOwned};
use std::time::Duration;

use super::config::{Endpoint, LOCAL_RERANKER_API, local_reranker};

const MAX_RESPONSE: usize = 256 * 1024;

fn transport_error(error: Error) -> anyhow::Error {
    // reqwest's Display may contain a credential-bearing URL.
    if error.is_timeout() {
        anyhow!("endpoint timed out")
    } else {
        anyhow!("endpoint request failed")
    }
}

fn client(loopback: bool) -> Result<Client> {
    let builder = Client::builder().redirect(redirect::Policy::none());
    let builder = if loopback {
        builder.no_proxy()
    } else {
        builder
    };
    builder
        .build()
        .map_err(|_| anyhow!("cannot initialize HTTP client"))
}

pub(super) async fn discover_local_reranker() -> Result<Endpoint> {
    #[derive(Deserialize)]
    struct ModelList {
        data: Vec<Model>,
    }
    #[derive(Deserialize)]
    struct Model {
        id: String,
        #[serde(rename = "type")]
        kind: Option<String>,
    }

    let response = client(true)?
        .get(format!("{LOCAL_RERANKER_API}/models"))
        .timeout(Duration::from_millis(350))
        .send()
        .await
        .map_err(|_| anyhow!("local reranker catalog unavailable"))?;
    let models: ModelList = read_json(response).await?;
    let mut models = models
        .data
        .into_iter()
        .filter(|model| model.kind.as_deref() == Some("reranker"));
    let model = models
        .next()
        .ok_or_else(|| anyhow!("local catalog advertises no reranker"))?;
    if models.next().is_some() {
        bail!("local catalog advertises multiple rerankers; configure a model");
    }
    local_reranker(model.id)
}

pub(super) async fn post<T: DeserializeOwned>(
    endpoint: &Endpoint,
    payload: &serde_json::Value,
) -> Result<T> {
    let loopback = matches!(
        endpoint.url.host_str(),
        Some("127.0.0.1" | "localhost" | "[::1]")
    );
    let client = client(loopback)?;
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
    read_json(request.send().await.map_err(transport_error)?).await
}

async fn read_json<T: DeserializeOwned>(mut response: Response) -> Result<T> {
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
