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

/// «18:00» → the next moment it is 18:00 local time (today or tomorrow), unix seconds.
pub fn at_time(hhmm: &str) -> Option<u64> {
    use chrono::{Local, NaiveTime, TimeZone};
    let time = NaiveTime::parse_from_str(hhmm.trim(), "%H:%M").ok()?;
    let now = Local::now();
    let mut day = now.date_naive();
    if time <= now.time() {
        day = day.succ_opt()?;
    }
    Local
        .from_local_datetime(&day.and_time(time))
        .earliest()
        .map(|t| t.timestamp() as u64)
}

/// Adds a reminder due at `due` (unix seconds); returns it.
pub fn add(due: u64, text: &str) -> Reminder {
    let mut all = load();
    let reminder = Reminder {
        id: all.iter().map(|r| r.id).max().unwrap_or(0) + 1,
        due,
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

/// Removes reminders whose text contains the words; returns what was removed.
pub fn cancel(about: &str) -> Vec<Reminder> {
    let about = about.trim().to_lowercase();
    let (gone, kept): (Vec<_>, Vec<_>) = load()
        .into_iter()
        .partition(|r| !about.is_empty() && r.text.to_lowercase().contains(&about));
    save(&kept);
    gone
}

/// "в 18:00 (через 2 ч 5 мин)" for the chat.
pub fn when(due: u64) -> String {
    let clock = chrono::DateTime::from_timestamp(due as i64, 0)
        .map(|t| t.with_timezone(&chrono::Local).format("%H:%M").to_string())
        .unwrap_or_default();
    format!("в {clock} ({})", left(due))
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
