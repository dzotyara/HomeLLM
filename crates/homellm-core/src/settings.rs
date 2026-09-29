//! User settings, saved as JSON next to the app's data. Environment variables
//! (`HOMELLM_MODELS_DIR`, `HOMELLM_MUSIC_DIR`, `HOMELLM_MUSIC_SEARCH`) win over the file.

use std::path::PathBuf;
use std::sync::RwLock;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Settings {
    /// Where models are stored; empty = the app's data folder.
    #[serde(default)]
    pub models_dir: String,
    /// A folder with music files `play_music` looks in first.
    #[serde(default)]
    pub music_dir: String,
    /// The search page for music; empty = Yandex Music.
    #[serde(default)]
    pub music_search: String,
    /// The model the app starts with (catalog id).
    #[serde(default)]
    pub last_model: String,
    /// The window's colour theme: mint (default), lime, violet or amber.
    #[serde(default)]
    pub theme: String,
    /// Downloads in progress: resumed when the app starts again.
    #[serde(default)]
    pub downloads: Vec<String>,
}

static CURRENT: RwLock<Option<Settings>> = RwLock::new(None);

fn file() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "HomeLLM").map(|d| d.config_dir().join("settings.json"))
}

pub fn get() -> Settings {
    if let Some(s) = CURRENT.read().unwrap().clone() {
        return s;
    }
    let saved: Settings = file()
        .and_then(|f| std::fs::read_to_string(f).ok())
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default();
    *CURRENT.write().unwrap() = Some(saved.clone());
    saved
}

pub fn save(settings: Settings) -> Result<()> {
    if let Some(path) = file() {
        std::fs::create_dir_all(path.parent().unwrap())?;
        std::fs::write(path, serde_json::to_string_pretty(&settings)?)?;
    }
    *CURRENT.write().unwrap() = Some(settings);
    Ok(())
}

/// An environment variable, else the saved value; `None` when both are empty.
pub fn value(env: &str, saved: impl Fn(&Settings) -> String) -> Option<String> {
    std::env::var(env)
        .ok()
        .or_else(|| Some(saved(&get())))
        .filter(|v| !v.trim().is_empty())
}
