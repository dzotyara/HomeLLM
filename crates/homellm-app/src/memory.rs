//! What the assistant remembers about the user across chats («запомни, что я люблю Кино»).

use std::path::PathBuf;

fn file() -> PathBuf {
    homellm_core::data_dir().join("memory.json")
}

pub fn load() -> Vec<String> {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save(facts: &[String]) {
    let path = file();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(
        path,
        serde_json::to_string_pretty(facts).unwrap_or_default(),
    );
}

pub fn remember(fact: &str) -> bool {
    let fact = fact.trim();
    let mut facts = load();
    if fact.is_empty() || facts.iter().any(|f| f.eq_ignore_ascii_case(fact)) {
        return false;
    }
    facts.push(fact.to_string());
    save(&facts);
    true
}

/// Forgets every fact that contains the words; returns what was removed.
pub fn forget(about: &str) -> Vec<String> {
    let about = about.trim().to_lowercase();
    let (gone, kept): (Vec<_>, Vec<_>) = load()
        .into_iter()
        .partition(|f| !about.is_empty() && f.to_lowercase().contains(&about));
    save(&kept);
    gone
}

/// For the system prompt.
pub fn context() -> String {
    let facts = load();
    if facts.is_empty() {
        return String::new();
    }
    format!(
        "Что ты знаешь о пользователе (он сам попросил запомнить):\n- {}",
        facts.join("\n- ")
    )
}
