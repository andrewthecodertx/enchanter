//! Query provider /models endpoints for real context window information.
//!
//! Most OpenAI-compatible providers expose a `/models` endpoint that returns
//! model metadata. Some include context window size; many don't. We try it,
//! extract what we can, and fall back to the hardcoded table when unavailable.
//!
//! The endpoint is derived by stripping `/chat/completions` from the base_url
//! and appending `/models`. For base_urls that don't follow this pattern, we
//! skip the query silently.

use std::collections::HashMap;

use serde::Deserialize;

use crate::tui::state::{ContextSource, ModelContextInfo};

/// Fetch model metadata from a provider and extract context window sizes.
///
/// `base_url` is the full chat completions endpoint URL (e.g.
/// `https://api.openai.com/v1/chat/completions`). We derive the models
/// endpoint by stripping `/chat/completions` and appending `/models`.
pub async fn fetch_model_context_info(
    base_url: &str,
    api_key: Option<&str>,
) -> HashMap<String, ModelContextInfo> {
    let models_url = match derive_models_url(base_url) {
        Some(u) => u,
        None => return HashMap::new(),
    };

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .unwrap_or_default();

    let mut req = client.get(&models_url);
    if let Some(key) = api_key
        && !key.is_empty() {
            req = req.header("Authorization", format!("Bearer {}", key));
        }

    match req.send().await {
        Ok(resp) => {
            if !resp.status().is_success() {
                return HashMap::new();
            }
            match resp.json::<ModelsResponse>().await {
                Ok(body) => parse_models_response(&body),
                Err(_) => HashMap::new(),
            }
        }
        Err(_) => HashMap::new(),
    }
}

/// Derive the /models endpoint URL from a chat completions base_url.
fn derive_models_url(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim_end_matches('/');
    if let Some(prefix) = trimmed.strip_suffix("/chat/completions") {
        Some(format!("{}/models", prefix))
    } else { trimmed.strip_suffix("/v1").map(|prefix| format!("{}/v1/models", prefix)) }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelData>,
}

#[derive(Debug, Deserialize)]
struct ModelData {
    #[serde(default)]
    id: String,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    max_context_length: Option<u64>,
    #[serde(default)]
    context_window: Option<u64>,
    #[serde(default)]
    top_context_length: Option<u64>,
}

fn parse_models_response(body: &ModelsResponse) -> HashMap<String, ModelContextInfo> {
    let mut map = HashMap::new();
    for m in &body.data {
        let size = m
            .context_length
            .or(m.max_context_length)
            .or(m.context_window)
            .or(m.top_context_length);

        if let Some(s) = size {
            map.insert(
                m.id.clone(),
                ModelContextInfo {
                    context_size: Some(s),
                    source: ContextSource::ApiQuery,
                },
            );
        }
    }
    map
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derive_models_url_standard() {
        assert_eq!(
            derive_models_url("https://api.openai.com/v1/chat/completions"),
            Some("https://api.openai.com/v1/models".to_string())
        );
    }

    #[test]
    fn derive_models_url_ollama() {
        assert_eq!(
            derive_models_url("http://localhost:11434/v1/chat/completions"),
            Some("http://localhost:11434/v1/models".to_string())
        );
    }

    #[test]
    fn derive_models_url_unknown_pattern() {
        // A URL without /chat/completions or /v1 suffix — can't derive /models.
        assert_eq!(
            derive_models_url("https://api.perplexity.ai/v1/agent"),
            None,
        );
    }

    #[test]
    fn parse_response_with_context_length() {
        let json = r#"{
            "data": [
                {"id": "llama3:8b", "context_length": 8192},
                {"id": "gpt-4o", "context_length": 128000}
            ]
        }"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let map = parse_models_response(&resp);
        assert_eq!(map.get("llama3:8b").unwrap().context_size, Some(8192));
        assert_eq!(map.get("gpt-4o").unwrap().context_size, Some(128000));
    }

    #[test]
    fn parse_response_no_context() {
        let json = r#"{
            "data": [
                {"id": "some-model"}
            ]
        }"#;
        let resp: ModelsResponse = serde_json::from_str(json).unwrap();
        let map = parse_models_response(&resp);
        assert!(map.is_empty());
    }
}
