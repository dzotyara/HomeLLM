//! Reminders set in the chat: kept in a JSON file, so they survive a restart,
//! and fired by a loop that shows a system notification.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reminder {
    pub id: u64,
    /// Unix seconds.
    pub due: u64,
    pub text: String,
}

fn file() -> PathBuf {
    homellm_core::data_dir().join("reminders.json")
}

pub fn load() -> Vec<Reminder> {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

pub fn save(reminders: &[Reminder]) {
    let path = file();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(path, serde_json::to_string(reminders).unwrap_or_default());
}

/// Adds a reminder `minutes` from now; returns it.
pub fn add(minutes: f64, text: &str) -> Reminder {
    let mut all = load();
    let reminder = Reminder {
        id: all.iter().map(|r| r.id).max().unwrap_or(0) + 1,
        due: crate::chats::now() + (minutes.max(0.0) * 60.0).round() as u64,
        text: text.trim().to_string(),
    };
    all.push(reminder.clone());
    save(&all);
    reminder
}

/// Takes the reminders that are due (a missed one fires as soon as the app starts).
pub fn take_due() -> Vec<Reminder> {
    let now = crate::chats::now();
    let (due, rest): (Vec<_>, Vec<_>) = load().into_iter().partition(|r| r.due <= now);
    if !due.is_empty() {
        save(&rest);
    }
    due
}

/// "через 5 мин" / "через 1 ч 20 мин" for the chat.
pub fn left(due: u64) -> String {
    let mins = due.saturating_sub(crate::chats::now()).div_ceil(60);
    if mins >= 60 {
        format!("через {} ч {} мин", mins / 60, mins % 60)
    } else {
        format!("через {mins} мин")
    }
}
