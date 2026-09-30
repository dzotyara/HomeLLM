//! Finding the user's files by name or by the text inside, and opening them.
//! Programs and scripts are never opened from here: that is `open_app`, with a confirmation.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Result, anyhow, bail};
use serde_json::{Value, json};

use super::{Risk, Tool};

const MAX_RESULTS: usize = 15;
const TIME_BUDGET: Duration = Duration::from_secs(6);
/// Text files bigger than this are not read for a content search.
const MAX_TEXT_SIZE: u64 = 2 << 20;
const TEXT_EXTENSIONS: &[&str] = &[
    "txt", "md", "csv", "json", "log", "ini", "cfg", "toml", "yaml", "yml", "xml", "html", "rs",
    "py", "js", "ts",
];
const SKIP_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "appdata",
    "$recycle.bin",
    "__pycache__",
];
const RUNNABLE: &[&str] = &[
    "exe", "bat", "cmd", "com", "ps1", "vbs", "vbe", "js", "jse", "wsf", "wsh", "msi", "msp",
    "scr", "lnk", "reg", "hta", "cpl", "jar", "sh", "app", "command",
];

pub fn tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "find_files",
            description: "Найти файлы пользователя по словам из имени («найди отчёт за март»). in_content=true — искать слова и внутри текстовых файлов. folder — где искать; по умолчанию рабочий стол, документы, загрузки, картинки, музыка, видео.",
            parameters: || {
                json!({"type": "object", "properties": {
                    "query": {"type": "string"},
                    "folder": {"type": "string"},
                    "in_content": {"type": "boolean"}},
                    "required": ["query"]})
            },
            risk: Risk::Safe,
            run: find_files,
        },
        Tool {
            name: "open_file",
            description: "Открыть файл или папку по полному пути (например из find_files). reveal=true — показать файл в проводнике.",
            parameters: || {
                json!({"type": "object", "properties": {"path": {"type": "string"}, "reveal": {"type": "boolean"}},
                    "required": ["path"]})
            },
            risk: Risk::Safe,
            run: open_file,
        },
    ]
}

fn find_files(args: &Value) -> Result<String> {
    let words = words(args["query"].as_str().unwrap_or_default());
    if words.is_empty() {
        bail!("missing argument `query`");
    }
    let in_content = args["in_content"].as_bool().unwrap_or(false);
    let roots = match args["folder"].as_str().filter(|f| !f.trim().is_empty()) {
        Some(folder) => {
            let folder = PathBuf::from(folder.trim());
            if !folder.is_dir() {
                bail!("папки {} нет", folder.display());
            }
            vec![folder]
        }
        None => default_roots(),
    };
    let (mut found, complete) = search(&roots, &words, in_content);
    if found.is_empty() {
        return Ok(if complete {
            "ничего не нашлось".into()
        } else {
            "за 6 секунд ничего не нашлось: назовите папку поточнее".into()
        });
    }
    // The newest first: «тот файл» is usually the recent one.
    found.sort_by_key(|f| std::cmp::Reverse(f.1));
    let total = found.len();
    let mut lines: Vec<String> = found
        .iter()
        .take(MAX_RESULTS)
        .map(|(path, modified, size)| {
            let when = chrono::DateTime::<chrono::Local>::from(*modified).format("%d.%m.%Y");
            let size = if path.is_dir() {
                "папка".to_string()
            } else {
                human_size(*size)
            };
            format!("{} ({size}, изменён {when})", path.display())
        })
        .collect();
    if total > MAX_RESULTS {
        lines.push(format!("…и ещё {}", total - MAX_RESULTS));
    }
    if !complete {
        lines.push("(искал 6 секунд, просмотрел не всё)".into());
    }
    Ok(lines.join("\n"))
}

fn default_roots() -> Vec<PathBuf> {
    let mut roots = vec![];
    if let Some(dirs) = directories::UserDirs::new() {
        roots.extend(
            [
                dirs.desktop_dir(),
                dirs.document_dir(),
                dirs.download_dir(),
                dirs.picture_dir(),
                dirs.audio_dir(),
                dirs.video_dir(),
            ]
            .into_iter()
            .flatten()
            .map(Path::to_path_buf),
        );
    }
    if let Some(music) = crate::settings::value("HOMELLM_MUSIC_DIR", |s| s.music_dir.clone()) {
        roots.push(PathBuf::from(music));
    }
    // OneDrive may redirect several of them to one folder.
    roots.sort();
    roots.dedup();
    let copy = roots.clone();
    roots.retain(|r| !copy.iter().any(|other| other != r && r.starts_with(other)));
    roots
}

/// Matches with their modification time and size; `false` when the time ran out.
fn search(
    roots: &[PathBuf],
    words: &[String],
    in_content: bool,
) -> (Vec<(PathBuf, SystemTime, u64)>, bool) {
    let started = Instant::now();
    let mut found = vec![];
    let mut stack: Vec<PathBuf> = roots.to_vec();
    while let Some(dir) = stack.pop() {
        if started.elapsed() > TIME_BUDGET {
            return (found, false);
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = normalize(&entry.file_name().to_string_lossy());
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let modified = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            if meta.is_dir() {
                if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                    stack.push(path.clone());
                }
                if matches(&name, words) {
                    found.push((path, modified, 0));
                }
                continue;
            }
            if matches(&name, words) || (in_content && content_matches(&path, meta.len(), words)) {
                found.push((path, modified, meta.len()));
            }
        }
    }
    (found, true)
}

fn content_matches(path: &Path, size: u64, words: &[String]) -> bool {
    let text_file = extension(path).is_some_and(|e| TEXT_EXTENSIONS.contains(&e.as_str()));
    if !text_file || size > MAX_TEXT_SIZE {
        return false;
    }
    std::fs::read(path)
        .is_ok_and(|bytes| matches(&normalize(&String::from_utf8_lossy(&bytes)), words))
}

/// Every word is in the text: «отчёт март» finds «Отчет_за_март_2026.xlsx».
fn matches(text: &str, words: &[String]) -> bool {
    words.iter().all(|w| text.contains(w.as_str()))
}

fn words(query: &str) -> Vec<String> {
    normalize(query)
        .split(|c: char| !c.is_alphanumeric() && c != '.')
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn normalize(text: &str) -> String {
    text.to_lowercase().replace('ё', "е")
}

fn extension(path: &Path) -> Option<String> {
    path.extension().map(|e| e.to_string_lossy().to_lowercase())
}

fn human_size(bytes: u64) -> String {
    match bytes {
        b if b >= 1 << 30 => format!("{:.1} ГБ", b as f64 / (1u64 << 30) as f64),
        b if b >= 1 << 20 => format!("{:.1} МБ", b as f64 / (1u64 << 20) as f64),
        b if b >= 1 << 10 => format!("{} КБ", b >> 10),
        b => format!("{b} Б"),
    }
}

fn open_file(args: &Value) -> Result<String> {
    let path = PathBuf::from(
        args["path"]
            .as_str()
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .ok_or_else(|| anyhow!("missing argument `path`"))?,
    );
    if !path.exists() {
        bail!("файла {} нет", path.display());
    }
    if args["reveal"].as_bool().unwrap_or(false) {
        reveal(&path)?;
        return Ok(format!("показан в проводнике: {}", path.display()));
    }
    if path.is_file() && extension(&path).is_some_and(|e| RUNNABLE.contains(&e.as_str())) {
        bail!(
            "это программа или скрипт: запускать их можно только через open_app, с подтверждением"
        );
    }
    open::that_detached(&path)?;
    Ok(format!("открыто: {}", path.display()))
}

fn reveal(path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // explorer wants «/select,"path"» as one raw argument.
        std::process::Command::new("explorer")
            .raw_arg(format!("/select,\"{}\"", path.display()))
            .spawn()?;
    }
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg("-R")
        .arg(path)
        .spawn()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    open::that_detached(path.parent().unwrap_or(path))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_words_must_match_and_yo_is_e() {
        let name = normalize("Отчет_за_Март_2026.xlsx");
        assert!(matches(&name, &words("отчёт март")));
        assert!(matches(&name, &words("2026 .xlsx")));
        assert!(!matches(&name, &words("отчёт апрель")));
    }

    #[test]
    fn finds_by_name_and_by_content() {
        let dir = std::env::temp_dir().join(format!("homellm-files-{}", std::process::id()));
        let nested = dir.join("работа").join(".git");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(dir.join("работа").join("План отпуска.txt"), "Сочи, июль").unwrap();
        std::fs::write(dir.join("заметки.md"), "купить билеты в Сочи").unwrap();
        std::fs::write(nested.join("сочи.txt"), "скрытая папка").unwrap();

        let roots = [dir.clone()];
        let (by_name, complete) = search(&roots, &words("план отпуска"), false);
        assert!(complete);
        assert_eq!(by_name.len(), 1);
        let (by_text, _) = search(&roots, &words("сочи"), true);
        let names: Vec<String> = by_text
            .iter()
            .map(|(p, ..)| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(
            !names.contains(&"сочи.txt".to_string()),
            "hidden folders are skipped"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn programs_are_not_opened() {
        let exe = std::env::temp_dir().join(format!("homellm-{}.bat", std::process::id()));
        std::fs::write(&exe, "echo hi").unwrap();
        let err = open_file(&json!({"path": exe.to_string_lossy()})).unwrap_err();
        assert!(err.to_string().contains("open_app"));
        std::fs::remove_file(exe).unwrap();
    }

    #[test]
    fn sizes_are_human() {
        assert_eq!(human_size(512), "512 Б");
        assert_eq!(human_size(3 << 20), "3.0 МБ");
    }
}
