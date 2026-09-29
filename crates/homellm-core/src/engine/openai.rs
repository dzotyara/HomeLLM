//! An external OpenAI-compatible server: Ollama (`http://localhost:11434`),
//! LM Studio (`http://localhost:1234`), llama-server.

use anyhow::{Result, bail};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{Engine, Message, TokenSink};

pub struct OpenAiEngine {
    base_url: String,
    model: String,
    client: reqwest::Client,
}

impl OpenAiEngine {
    pub fn new(base_url: &str, model: &str) -> Self {
        Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            model: model.to_string(),
            client: reqwest::Client::new(),
        }
    }
}

#[async_trait]
impl Engine for OpenAiEngine {
    fn name(&self) -> String {
        format!("{} @ {}", self.model, self.base_url)
    }

    async fn complete(&self, messages: &[Message], tokens: TokenSink) -> Result<String> {
        let body = json!({"model": self.model, "messages": messages, "stream": true});
        let response = self
            .client
            .post(format!("{}/v1/chat/completions", self.base_url))
            .json(&body)
            .send()
            .await?;
        if !response.status().is_success() {
            bail!(
                "{}: {}",
                response.status(),
                response.text().await.unwrap_or_default()
            );
        }
        // Server-sent events: `data: {json}` lines, finished by `data: [DONE]`.
        let mut stream = response.bytes_stream();
        let mut pending = String::new();
        let mut text = String::new();
        while let Some(chunk) = stream.next().await {
            pending.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(end) = pending.find('\n') {
                let line = pending[..end].trim().to_string();
                pending.drain(..=end);
                let Some(data) = line.strip_prefix("data:").map(str::trim) else {
                    continue;
                };
                if data == "[DONE]" {
                    return Ok(text);
                }
                let event: Value = serde_json::from_str(data)?;
                if let Some(piece) = event["choices"][0]["delta"]["content"].as_str() {
                    text.push_str(piece);
                    if let Some(tx) = &tokens {
                        let _ = tx.send(piece.to_string());
                    }
                }
            }
        }
        Ok(text)
    }
}
