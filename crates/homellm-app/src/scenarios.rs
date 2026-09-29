//! Scenarios: named lists of tool calls («режим кино» = громкость 30 + YouTube),
//! saved from the chat and run with one phrase.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Step {
    pub tool: String,
    #[serde(default)]
    pub args: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub steps: Vec<Step>,
}

fn file() -> PathBuf {
    homellm_core::data_dir().join("scenarios.json")
}

pub fn load() -> Vec<Scenario> {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_all(all: &[Scenario]) {
    let path = file();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(path, serde_json::to_string_pretty(all).unwrap_or_default());
}

/// Names match case-insensitively: «Режим кино» finds «режим кино».
pub fn key(name: &str) -> String {
    name.trim().to_lowercase()
}

pub fn find(name: &str) -> Option<Scenario> {
    load().into_iter().find(|s| key(&s.name) == key(name))
}

pub fn save(scenario: Scenario) {
    let mut all = load();
    all.retain(|s| key(&s.name) != key(&scenario.name));
    all.push(scenario);
    save_all(&all);
}

pub fn delete(name: &str) -> bool {
    let mut all = load();
    let before = all.len();
    all.retain(|s| key(&s.name) != key(name));
    save_all(&all);
    all.len() != before
}
