//! An external OpenAI-compatible server: Ollama (`http://localhost:11434`),
//! LM Studio (`http://localhost:1234`), llama-server.

use anyhow::{Result, bail};
use async_trait::async_trait;
use base64::Engine as _;
use futures_util::StreamExt;
use serde_json::{Value, json};

use super::{Engine, Message, TokenSink, keep_last_images};

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

    /// The server decides: a model without vision answers with an error.
    fn sees_images(&self) -> bool {
        true
    }

    async fn complete(&self, messages: &[Message], tokens: TokenSink) -> Result<String> {
        let messages: Vec<Value> = keep_last_images(messages).iter().map(to_json).collect();
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

/// Text as is; with pictures, OpenAI's content parts with the pictures inlined as data URLs.
fn to_json(m: &Message) -> Value {
    if m.images.is_empty() {
        return json!({"role": m.role, "content": m.content});
    }
    let mut parts = vec![json!({"type": "text", "text": m.content})];
    for path in &m.images {
        let Ok(bytes) = std::fs::read(path) else {
            continue;
        };
        let mime = match path.rsplit('.').next().map(str::to_lowercase).as_deref() {
            Some("jpg" | "jpeg") => "image/jpeg",
            Some("webp") => "image/webp",
            Some("gif") => "image/gif",
            _ => "image/png",
        };
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        parts.push(json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}}));
    }
    json!({"role": m.role, "content": parts})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_go_as_content_parts() {
        let path = std::env::temp_dir().join(format!("homellm-{}.png", std::process::id()));
        std::fs::write(&path, b"png").unwrap();
        let m = Message::with_images(
            "user",
            "что это?",
            vec![path.to_string_lossy().into_owned()],
        );
        let v = to_json(&m);
        assert_eq!(v["content"][0]["text"], "что это?");
        assert_eq!(
            v["content"][1]["image_url"]["url"],
            "data:image/png;base64,cG5n"
        );
        assert_eq!(to_json(&Message::new("user", "hi"))["content"], "hi");
        std::fs::remove_file(path).unwrap();
    }
}
