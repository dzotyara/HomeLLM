//! Inference engines. Built in: llama.cpp (`llama` feature). External: any
//! OpenAI-compatible server — Ollama, LM Studio, llama-server.

#[cfg(feature = "llama-cpu")]
pub mod llama;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: String,
    /// Pictures that go with the text (paths), for models that see.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
}

impl Message {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self::with_images(role, content, vec![])
    }

    pub fn with_images(role: &str, content: impl Into<String>, images: Vec<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            images,
        }
    }
}

/// Only the latest pictures are shown to the model: each one costs hundreds of tokens.
/// Earlier ones become a note in the text.
pub fn keep_last_images(messages: &[Message]) -> Vec<Message> {
    let last = messages.iter().rposition(|m| !m.images.is_empty());
    messages
        .iter()
        .enumerate()
        .map(|(i, m)| {
            let mut m = m.clone();
            if Some(i) != last && !m.images.is_empty() {
                m.content = format!(
                    "{}
[картинка: {} шт., уже обсуждали]",
                    m.content,
                    m.images.len()
                );
                m.images.clear();
            }
            m
        })
        .collect()
}

/// Where streamed tokens go (the UI prints them as they come).
pub type TokenSink = Option<UnboundedSender<String>>;

#[async_trait]
pub trait Engine: Send + Sync {
    fn name(&self) -> String;
    /// Understands pictures in `Message::images`.
    fn sees_images(&self) -> bool {
        false
    }
    /// Generates the assistant's next message, streaming pieces to `tokens`, and returns the whole text.
    async fn complete(&self, messages: &[Message], tokens: TokenSink) -> Result<String>;
}
