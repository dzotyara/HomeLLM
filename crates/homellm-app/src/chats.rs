//! Saved conversations: one JSON file in the data folder.

use std::path::PathBuf;

use homellm_core::engine::Message;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: u64,
    pub title: String,
    /// Unix seconds of the last message: the list is newest first.
    pub updated: u64,
    /// What the model saw, without the system prompt; the window renders it.
    pub history: Vec<Message>,
}

fn file() -> PathBuf {
    homellm_core::data_dir().join("chats.json")
}

pub fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

pub fn load() -> Vec<Chat> {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(chats: &[Chat]) -> anyhow::Result<()> {
    let path = file();
    std::fs::create_dir_all(path.parent().unwrap())?;
    std::fs::write(path, serde_json::to_string(chats)?)?;
    Ok(())
}

/// The first user message, cut to a sidebar-sized title.
pub fn title_from(text: &str) -> String {
    let line = text.lines().next().unwrap_or("").trim();
    let mut title: String = line.chars().take(40).collect();
    if line.chars().count() > 40 {
        title.push('…');
    }
    if title.is_empty() {
        "Новый чат".into()
    } else {
        title
    }
}
