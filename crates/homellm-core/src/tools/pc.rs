//! Desktop control: media keys, music, links, programs, system info.
//! Mobile builds will get their own, much smaller set.

use anyhow::{Context, Result, anyhow, bail};
use enigo::{Direction, Enigo, Key, Keyboard, Settings};
use serde_json::{Value, json};

use super::{Risk, Tool};

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "media",
            description: "Пауза/продолжить, следующий/предыдущий трек, звук громче/тише на шаг, выключить звук. «Прибавь», «убавь», «потише» — это volume_up/volume_down.",
            parameters: || {
                json!({"type": "object", "properties": {"action": {"type": "string",
                    "enum": ["play_pause", "next", "previous", "volume_up", "volume_down", "mute"]}},
                    "required": ["action"]})
            },
            risk: Risk::Safe,
            run: media,
        },
        Tool {
            name: "play_music",
            description: "Включить музыку: песню, исполнителя или жанр. Без запроса — продолжить воспроизведение.",
            parameters: || json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            risk: Risk::Safe,
            run: play_music,
        },
        Tool {
            name: "open_url",
            description: "Открыть веб-страницу в браузере.",
            parameters: || json!({"type": "object", "properties": {"url": {"type": "string"}}, "required": ["url"]}),
            risk: Risk::Safe,
            run: open_url,
        },
        Tool {
            name: "open_app",
            description: "Запустить программу по имени (например notepad, calc, spotify, telegram).",
            parameters: || json!({"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}),
            risk: Risk::Confirm,
            run: open_app,
        },
        Tool {
            name: "set_volume",
            description: "Выставить точную громкость, ТОЛЬКО когда пользователь назвал число процентов («громкость 30%»). Для «прибавь/убавь» используй media.",
            parameters: || {
                json!({"type": "object", "properties": {"percent": {"type": "integer", "minimum": 0, "maximum": 100}},
                    "required": ["percent"]})
            },
            risk: Risk::Safe,
            run: set_volume,
        },
        Tool {
            name: "clipboard_read",
            description: "Прочитать текст из буфера обмена («что я скопировал», «переведи скопированное»).",
            parameters: || json!({"type": "object", "properties": {}}),
            risk: Risk::Safe,
            run: clipboard_read,
        },
        Tool {
            name: "clipboard_write",
            description: "Положить текст в буфер обмена («скопируй это»).",
            parameters: || json!({"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"]}),
            risk: Risk::Safe,
            run: clipboard_write,
        },
        Tool {
            name: "system_info",
            description: "Текущие дата и время, ОС, загрузка памяти.",
            parameters: || json!({"type": "object", "properties": {}}),
            risk: Risk::Safe,
            run: system_info,
        },
    ]
}

fn arg<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args[name]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or_else(|| anyhow!("missing argument `{name}`"))
}

fn press(key: Key) -> Result<()> {
    let mut enigo = Enigo::new(&Settings::default()).map_err(|e| anyhow!("{e}"))?;
    enigo.key(key, Direction::Click).map_err(|e| anyhow!("{e}"))
}

fn media(args: &Value) -> Result<String> {
    let key = match arg(args, "action")? {
        "play_pause" => Key::MediaPlayPause,
        "next" => Key::MediaNextTrack,
        "previous" => Key::MediaPrevTrack,
        "volume_up" => Key::VolumeUp,
        "volume_down" => Key::VolumeDown,
        "mute" => Key::VolumeMute,
        other => bail!("unknown action {other}"),
    };
    // One volume key press is ~2%: step by ~10% so the change is noticeable.
    let times = if matches!(key, Key::VolumeUp | Key::VolumeDown) {
        5
    } else {
        1
    };
    for _ in 0..times {
        press(key)?;
    }
    Ok("готово".into())
}

/// A local file from `HOMELLM_MUSIC_DIR` whose name contains the query, else a search
/// on `HOMELLM_MUSIC_SEARCH` (Yandex Music by default).
fn play_music(args: &Value) -> Result<String> {
    let Ok(query) = arg(args, "query") else {
        press(Key::MediaPlayPause)?;
        return Ok("воспроизведение переключено".into());
    };
    if let Some(file) = find_local_track(query) {
        open::that_detached(&file)?;
        return Ok(format!("играет файл {}", file.display()));
    }
    let search = crate::settings::value("HOMELLM_MUSIC_SEARCH", |s| s.music_search.clone())
        .unwrap_or_else(|| "https://music.yandex.ru/search?text=".into());
    let url = format!("{search}{}", encode(query));
    open::that_detached(&url)?;
    Ok(format!("открыт поиск: {url}"))
}

fn find_local_track(query: &str) -> Option<std::path::PathBuf> {
    let dir = crate::settings::value("HOMELLM_MUSIC_DIR", |s| s.music_dir.clone())?;
    let query = query.to_lowercase();
    let mut stack = vec![std::path::PathBuf::from(dir)];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let name = path.file_name()?.to_string_lossy().to_lowercase();
            let audio = [".mp3", ".flac", ".ogg", ".m4a", ".wav", ".opus"]
                .iter()
                .any(|e| name.ends_with(e));
            if audio && name.contains(&query) {
                return Some(path);
            }
        }
    }
    None
}

fn encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            b' ' => "+".into(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn open_url(args: &Value) -> Result<String> {
    let url = arg(args, "url")?;
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        bail!("only http(s) links are allowed");
    }
    open::that_detached(url)?;
    Ok(format!("открыто: {url}"))
}

fn open_app(args: &Value) -> Result<String> {
    let name = arg(args, "name")?;
    if name.contains(['&', '|', ';', '>', '<', '"', '\n']) {
        bail!("bad program name");
    }
    #[cfg(windows)]
    let status = std::process::Command::new("cmd")
        .args(["/C", "start", "", name])
        .status();
    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("open")
        .args(["-a", name])
        .status();
    // Linux has no launcher that reports a missing program: start it directly.
    #[cfg(all(unix, not(target_os = "macos")))]
    std::process::Command::new(name)
        .spawn()
        .with_context(|| format!("program {name} not found"))?;
    #[cfg(any(windows, target_os = "macos"))]
    {
        let status = status.context("failed to start the program")?;
        if !status.success() {
            bail!("program {name} not found");
        }
    }
    Ok(format!("запущено: {name}"))
}

fn set_volume(args: &Value) -> Result<String> {
    let percent = args["percent"]
        .as_u64()
        .or_else(|| {
            args["percent"]
                .as_str()
                .and_then(|s| s.trim_end_matches('%').parse().ok())
        })
        .ok_or_else(|| anyhow!("missing argument `percent`"))?
        .min(100);
    #[cfg(windows)]
    {
        // Windows moves the volume by 2% per key press: go to zero, then step up.
        for _ in 0..50 {
            press(Key::VolumeDown)?;
        }
        for _ in 0..percent.div_ceil(2) {
            press(Key::VolumeUp)?;
        }
    }
    #[cfg(target_os = "macos")]
    std::process::Command::new("osascript")
        .args(["-e", &format!("set volume output volume {percent}")])
        .status()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let wpctl = std::process::Command::new("wpctl")
            .args(["set-volume", "@DEFAULT_AUDIO_SINK@", &format!("{percent}%")])
            .status();
        if !wpctl.is_ok_and(|s| s.success()) {
            std::process::Command::new("pactl")
                .args(["set-sink-volume", "@DEFAULT_SINK@", &format!("{percent}%")])
                .status()?;
        }
    }
    Ok(format!("громкость {percent}%"))
}

/// Enough for the model's context; a huge clipboard is cut.
const CLIPBOARD_LIMIT: usize = 4000;

fn clipboard_read(_: &Value) -> Result<String> {
    let text = arboard::Clipboard::new()?
        .get_text()
        .context("в буфере нет текста")?;
    let mut cut: String = text.chars().take(CLIPBOARD_LIMIT).collect();
    if text.chars().count() > CLIPBOARD_LIMIT {
        cut.push_str("\n…(обрезано)");
    }
    Ok(cut)
}

fn clipboard_write(args: &Value) -> Result<String> {
    arboard::Clipboard::new()?.set_text(arg(args, "text")?.to_string())?;
    Ok("скопировано в буфер обмена".into())
}

fn system_info(_: &Value) -> Result<String> {
    let hw = crate::hardware::detect();
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs();
    Ok(format!(
        "unix-время {now} (UTC), ОС {}, память занята {} из {}",
        hw.os,
        crate::hardware::gib(sys.used_memory()),
        crate::hardware::gib(sys.total_memory()),
    ))
}
