//! Automations: a scenario on a schedule — «at 10:00 save and close Word, then shut down». Made
//! in the Automations tab or from a phrase in the chat; run by a loop that checks the clock.
//! They run unattended: the user approved every step by creating it, so risky tools do not ask.

use std::path::PathBuf;

use chrono::{Datelike, Local, NaiveTime};
use serde::{Deserialize, Serialize};

use crate::scenarios::Step;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Automation {
    #[serde(default)]
    pub id: u64,
    pub name: String,
    #[serde(default = "on")]
    pub enabled: bool,
    /// «HH:MM», local time.
    pub time: String,
    /// Days of the week, 1 = Monday … 7 = Sunday; empty = every day.
    #[serde(default)]
    pub days: Vec<u8>,
    /// Runs once, then switches itself off.
    #[serde(default)]
    pub once: bool,
    pub steps: Vec<Step>,
    /// «YYYY-MM-DD HH:MM» of the last run: never twice in one minute.
    #[serde(default)]
    pub last_run: String,
    /// What the last run reported, for the tab.
    #[serde(default)]
    pub last_result: String,
}

fn on() -> bool {
    true
}

fn file() -> PathBuf {
    homellm_core::data_dir().join("automations.json")
}

pub fn load() -> Vec<Automation> {
    std::fs::read_to_string(file())
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn save_all(all: &[Automation]) {
    let path = file();
    let _ = std::fs::create_dir_all(path.parent().unwrap());
    let _ = std::fs::write(path, serde_json::to_string_pretty(all).unwrap_or_default());
}

/// «9:5» and «09:05» alike; `None` for anything that is not a time.
pub fn normalize_time(time: &str) -> Option<String> {
    NaiveTime::parse_from_str(time.trim(), "%H:%M")
        .ok()
        .map(|t| t.format("%H:%M").to_string())
}

/// Checks and stores it: a new one (id 0) gets an id; returns the stored one.
pub fn save(mut automation: Automation) -> Result<Automation, String> {
    automation.name = automation.name.trim().to_string();
    if automation.name.is_empty() {
        return Err("нужно название".into());
    }
    automation.time =
        normalize_time(&automation.time).ok_or("время — в виде ЧЧ:ММ, например 10:00")?;
    if automation.steps.is_empty() {
        return Err("нужно хотя бы одно действие".into());
    }
    automation.days.retain(|d| (1..=7).contains(d));
    automation.days.sort_unstable();
    automation.days.dedup();
    let mut all = load();
    if automation.id == 0 {
        automation.id = all.iter().map(|a| a.id).max().unwrap_or(0) + 1;
    }
    match all.iter_mut().find(|a| a.id == automation.id) {
        Some(existing) => *existing = automation.clone(),
        None => all.push(automation.clone()),
    }
    save_all(&all);
    Ok(automation)
}

pub fn delete(id: u64) -> bool {
    let mut all = load();
    let before = all.len();
    all.retain(|a| a.id != id);
    save_all(&all);
    all.len() != before
}

pub fn set_enabled(id: u64, enabled: bool) {
    let mut all = load();
    if let Some(a) = all.iter_mut().find(|a| a.id == id) {
        a.enabled = enabled;
    }
    save_all(&all);
}

/// Records a run: the time, what it reported, and switches a one-time automation off.
pub fn finished(id: u64, stamp: &str, result: &str) {
    let mut all = load();
    if let Some(a) = all.iter_mut().find(|a| a.id == id) {
        a.last_run = stamp.to_string();
        a.last_result = result.to_string();
        if a.once {
            a.enabled = false;
        }
    }
    save_all(&all);
}

/// Due right now (this minute, a matching day) and not run yet this minute.
pub fn due_now() -> Vec<Automation> {
    let now = Local::now();
    let stamp = now.format("%Y-%m-%d %H:%M").to_string();
    let time = now.format("%H:%M").to_string();
    let weekday = now.weekday().number_from_monday() as u8;
    load()
        .into_iter()
        .filter(|a| is_due(a, &time, weekday, &stamp))
        .collect()
}

fn is_due(a: &Automation, time: &str, weekday: u8, stamp: &str) -> bool {
    a.enabled
        && a.time == time
        && (a.days.is_empty() || a.days.contains(&weekday))
        && a.last_run != stamp
}

pub fn stamp() -> String {
    Local::now().format("%Y-%m-%d %H:%M").to_string()
}

/// «по будням в 10:00», «каждый день в 22:30», «один раз в 09:00».
pub fn when(a: &Automation) -> String {
    const NAMES: [&str; 7] = ["пн", "вт", "ср", "чт", "пт", "сб", "вс"];
    let days = match a.days.as_slice() {
        [] => "каждый день".to_string(),
        [1, 2, 3, 4, 5] => "по будням".to_string(),
        [6, 7] => "по выходным".to_string(),
        days => days
            .iter()
            .filter_map(|d| NAMES.get(*d as usize - 1))
            .copied()
            .collect::<Vec<_>>()
            .join(", "),
    };
    if a.once {
        format!("один раз в {}", a.time)
    } else {
        format!("{days} в {}", a.time)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn automation(days: Vec<u8>) -> Automation {
        Automation {
            id: 1,
            name: "Конец дня".into(),
            enabled: true,
            time: "10:00".into(),
            days,
            once: false,
            steps: vec![],
            last_run: String::new(),
            last_result: String::new(),
        }
    }

    #[test]
    fn due_at_its_minute_on_its_days_once_per_minute() {
        let weekdays = automation(vec![1, 2, 3, 4, 5]);
        assert!(is_due(&weekdays, "10:00", 1, "2026-10-05 10:00"));
        assert!(!is_due(&weekdays, "10:01", 1, "2026-10-05 10:01"));
        assert!(
            !is_due(&weekdays, "10:00", 6, "2026-10-03 10:00"),
            "not on Saturday"
        );
        let mut ran = weekdays.clone();
        ran.last_run = "2026-10-05 10:00".into();
        assert!(
            !is_due(&ran, "10:00", 1, "2026-10-05 10:00"),
            "not twice in the minute"
        );
        let mut off = weekdays;
        off.enabled = false;
        assert!(!is_due(&off, "10:00", 1, "2026-10-05 10:00"));
        assert!(
            is_due(&automation(vec![]), "10:00", 7, "2026-10-04 10:00"),
            "every day"
        );
    }

    #[test]
    fn times_are_normalized_and_schedules_described() {
        assert_eq!(normalize_time("9:05").as_deref(), Some("09:05"));
        assert_eq!(normalize_time("25:00"), None);
        assert_eq!(when(&automation(vec![1, 2, 3, 4, 5])), "по будням в 10:00");
        assert_eq!(when(&automation(vec![])), "каждый день в 10:00");
        assert_eq!(when(&automation(vec![2, 4])), "вт, чт в 10:00");
    }
}
