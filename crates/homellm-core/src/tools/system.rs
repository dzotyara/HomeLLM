//! The computer itself: shut down, restart, sleep, lock, and a pause between the steps of a
//! scenario or an automation. Shutting down and restarting wait a minute and say so, so the
//! user can still cancel.

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::{Risk, Tool};

/// Seconds before a shutdown or a restart: time to save and cancel.
const POWER_DELAY: u32 = 60;
const MAX_WAIT: u64 = 600;

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "power",
            description: "Компьютер: shutdown (выключить), restart (перезагрузить) — через минуту, с предупреждением; cancel — отменить выключение/перезагрузку; sleep (сон); lock (заблокировать).",
            parameters: || {
                json!({"type": "object", "properties": {"action": {"type": "string",
                    "enum": ["shutdown", "restart", "cancel", "sleep", "lock"]}}, "required": ["action"]})
            },
            risk: Risk::Confirm,
            run: power,
        },
        Tool {
            name: "wait",
            description: "Подождать seconds секунд (до 600) — между шагами сценария или автоматизации.",
            parameters: || {
                json!({"type": "object", "properties": {"seconds": {"type": "integer", "minimum": 1, "maximum": 600}},
                    "required": ["seconds"]})
            },
            risk: Risk::Safe,
            run: wait,
        },
    ]
}

fn wait(args: &Value) -> Result<String> {
    let seconds = args["seconds"]
        .as_u64()
        .or_else(|| args["seconds"].as_str().and_then(|s| s.trim().parse().ok()))
        .ok_or_else(|| anyhow!("missing argument `seconds`"))?
        .clamp(1, MAX_WAIT);
    std::thread::sleep(std::time::Duration::from_secs(seconds));
    Ok(format!("подождал {seconds} с"))
}

fn power(args: &Value) -> Result<String> {
    let action = args["action"]
        .as_str()
        .ok_or_else(|| anyhow!("missing argument `action`"))?;
    platform(action)?;
    Ok(match action {
        "shutdown" => {
            format!("компьютер выключится через {POWER_DELAY} с; отменить — power cancel")
        }
        "restart" => {
            format!("компьютер перезагрузится через {POWER_DELAY} с; отменить — power cancel")
        }
        "cancel" => "выключение отменено".into(),
        "sleep" => "компьютер уходит в сон".into(),
        "lock" => "компьютер заблокирован".into(),
        other => bail!("unknown action {other}"),
    })
}

#[cfg(windows)]
fn platform(action: &str) -> Result<()> {
    use std::os::windows::process::CommandExt;
    let run = |program: &str, args: &[&str]| -> Result<()> {
        let status = std::process::Command::new(program)
            .args(args)
            .creation_flags(0x0800_0000)
            .status()?;
        // `shutdown /a` with nothing scheduled fails: nothing to cancel is fine too.
        if !status.success() && !(program == "shutdown" && args.first() == Some(&"/a")) {
            bail!("{program} не сработал");
        }
        Ok(())
    };
    let delay = POWER_DELAY.to_string();
    let note =
        "HomeLLM: компьютер выключится через минуту. Отменить — «отмени выключение» в HomeLLM.";
    match action {
        "shutdown" => run("shutdown", &["/s", "/t", &delay, "/c", note]),
        "restart" => run("shutdown", &["/r", "/t", &delay, "/c", note]),
        "cancel" => run("shutdown", &["/a"]),
        "lock" => run("rundll32.exe", &["user32.dll,LockWorkStation"]),
        "sleep" => {
            // SAFETY: a plain Win32 call; false = sleep, not hibernate.
            if unsafe { windows::Win32::System::Power::SetSuspendState(false, false, false) } {
                Ok(())
            } else {
                bail!("компьютер не ушёл в сон")
            }
        }
        other => bail!("unknown action {other}"),
    }
}

#[cfg(target_os = "macos")]
fn platform(action: &str) -> Result<()> {
    let script = match action {
        "shutdown" => "delay 60\ntell application \"System Events\" to shut down",
        "restart" => "delay 60\ntell application \"System Events\" to restart",
        "sleep" => "tell application \"System Events\" to sleep",
        "lock" => {
            "tell application \"System Events\" to keystroke \"q\" using {control down, command down}"
        }
        "cancel" => bail!("на macOS отменить можно только в системном окне"),
        other => bail!("unknown action {other}"),
    };
    std::process::Command::new("osascript")
        .args(["-e", script])
        .spawn()?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn platform(action: &str) -> Result<()> {
    let args: &[&str] = match action {
        "shutdown" => &["shutdown", "-h", "+1"],
        "restart" => &["shutdown", "-r", "+1"],
        "cancel" => &["shutdown", "-c"],
        "sleep" => &["systemctl", "suspend"],
        "lock" => &["loginctl", "lock-session"],
        other => bail!("unknown action {other}"),
    };
    let status = std::process::Command::new(args[0])
        .args(&args[1..])
        .status()?;
    if !status.success() {
        bail!("{} не сработал", args[0]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wait_is_capped_and_reads_strings() {
        let started = std::time::Instant::now();
        assert_eq!(wait(&json!({"seconds": "1"})).unwrap(), "подождал 1 с");
        assert!(started.elapsed().as_millis() >= 1000);
        assert!(wait(&json!({})).is_err());
    }

    #[test]
    fn unknown_power_actions_are_refused_before_anything_runs() {
        assert!(power(&json!({"action": "explode"})).is_err());
        assert!(power(&json!({})).is_err());
    }
}
