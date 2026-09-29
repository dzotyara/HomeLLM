//! HomeLLM core: model catalog, hardware check, downloads, inference engines,
//! PC-control tools and the agent loop. No UI here: the CLI (and later the
//! Tauri app) are thin shells around this crate.

pub mod agent;
pub mod catalog;
pub mod download;
pub mod engine;
pub mod hardware;
pub mod tools;

use std::path::PathBuf;

/// Where downloaded models live: `%APPDATA%\HomeLLM\data\models` on Windows,
/// `~/Library/Application Support/HomeLLM/models` on macOS, `~/.local/share/homellm/models` on Linux.
/// `HOMELLM_MODELS_DIR` overrides it.
pub fn models_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("HOMELLM_MODELS_DIR") {
        return PathBuf::from(dir);
    }
    directories::ProjectDirs::from("", "", "HomeLLM")
        .map(|d| d.data_dir().join("models"))
        .unwrap_or_else(|| PathBuf::from("models"))
}
