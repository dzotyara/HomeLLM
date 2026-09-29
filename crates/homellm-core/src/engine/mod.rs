//! Inference engines. Built in: llama.cpp (`llama` feature). External: any
//! OpenAI-compatible server — Ollama, LM Studio, llama-server.

#[cfg(feature = "llama")]
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
}

impl Message {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
        }
    }
}

/// Where streamed tokens go (the UI prints them as they come).
pub type TokenSink = Option<UnboundedSender<String>>;

#[async_trait]
pub trait Engine: Send + Sync {
    fn name(&self) -> String;
    /// Generates the assistant's next message, streaming pieces to `tokens`, and returns the whole text.
    async fn complete(&self, messages: &[Message], tokens: TokenSink) -> Result<String>;
}
