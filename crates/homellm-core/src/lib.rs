//! HomeLLM core: model catalog, hardware check, downloads, inference engines,
//! PC-control tools and the agent loop. No UI here: the CLI (and later the
//! Tauri app) are thin shells around this crate.

pub mod agent;
pub mod catalog;
pub mod download;
pub mod engine;
pub mod hardware;
pub mod mcp;
pub mod settings;
pub mod spotify;
pub mod tools;

use std::path::PathBuf;

/// Where downloaded models live: `%APPDATA%\HomeLLM\data\models` on Windows,
/// `~/Library/Application Support/HomeLLM/models` on macOS, `~/.local/share/homellm/models` on Linux.
/// `HOMELLM_MODELS_DIR` or the settings override it.
pub fn models_dir() -> PathBuf {
    if let Some(dir) = settings::value("HOMELLM_MODELS_DIR", |s| s.models_dir.clone()) {
        return PathBuf::from(dir);
    }
    data_dir().join("models")
}

/// The app's data folder: models (by default), chats.
pub fn data_dir() -> PathBuf {
    directories::ProjectDirs::from("", "", "HomeLLM")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."))
}
