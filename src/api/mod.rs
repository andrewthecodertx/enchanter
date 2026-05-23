//! OpenAI-compatible chat completions client.

use anyhow::{Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
}

impl Message {
    pub fn system(content: impl Into<String>) -> Self {
        Self { role: "system".into(), content: content.into() }
    }

    pub fn user(content: impl Into<String>) -> Self {
        Self { role: "user".into(), content: content.into() }
    }

    pub fn assistant(content: impl Into<String>) -> Self {
        Self { role: "assistant".into(), content: content.into() }
    }
}

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<Message>,
    stream: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct Usage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct StreamDelta {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct StreamChoice {
    delta: DeltaContent,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DeltaContent {
    content: Option<String>,
}

pub struct LlmClient {
    client: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
}

impl LlmClient {
    pub fn new(base_url: &str, api_key: Option<&str>, model: &str) -> Self {
        Self {
            client: Client::new(),
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: api_key.map(|s| s.to_string()),
            model: model.to_string(),
        }
    }

    fn apply_auth(&self, req: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        match &self.api_key {
            Some(key) => req.header("Authorization", format!("Bearer {}", key)),
            None => req,
        }
    }

    pub async fn chat_stream(&self, messages: Vec<Message>) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);

        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            stream: true,
            temperature: None,
        };

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request);
        let response = self.apply_auth(response)
            .send()
            .await
            .with_context(|| format!("connecting to {}", url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }

        let mut full_response = String::new();
        let mut stream = response.bytes_stream();
        use futures_util::StreamExt;

        let mut buffer = String::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading stream chunk")?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));

            while let Some(newline_pos) = buffer.find('\n') {
                let line = buffer[..newline_pos].trim().to_string();
                buffer = buffer[newline_pos + 1..].to_string();

                if line.starts_with("data: ") {
                    let data = &line[6..];
                    if data == "[DONE]" {
                        break;
                    }

                    if let Ok(delta) = serde_json::from_str::<StreamDelta>(data) {
                        for choice in &delta.choices {
                            if let Some(content) = &choice.delta.content {
                                full_response.push_str(content);
                                print!("{}", content);
                            }
                        }
                    }
                }
            }
        }

        println!();
        Ok(full_response)
    }

    pub async fn chat(&self, messages: Vec<Message>) -> Result<String> {
        let url = format!("{}/chat/completions", self.base_url);

        let request = ChatRequest {
            model: self.model.clone(),
            messages,
            stream: false,
            temperature: None,
        };

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&request);
        let response = self.apply_auth(response)
            .send()
            .await
            .with_context(|| format!("connecting to {}", url))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("API error {}: {}", status, body);
        }

        let chat_response: ChatResponse = response
            .json()
            .await
            .context("parsing API response")?;

        let content = chat_response
            .choices
            .first()
            .and_then(|c| c.message.content.clone())
            .unwrap_or_default();

        Ok(content)
    }
}