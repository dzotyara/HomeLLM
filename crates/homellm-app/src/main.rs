//! HomeLLM desktop app: a Tauri shell around homellm-core. The window (`ui/`)
//! calls these commands and listens to events: `progress` / `download-done`,
//! `token` (streamed answer), `tool` / `tool-result` (actions), `confirm` (asks
//! the user), `model-changed`, `chats-changed`, `settings-changed`.
//! Everything the window can do is also a tool, so it can be asked for in the chat.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chats;
mod memory;
mod reminders;
mod scenarios;

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chats::Chat;
use homellm_core::agent::{Agent, Confirm, Event, ToolHost};
use homellm_core::engine::Message;
use homellm_core::engine::llama::LlamaEngine;
use homellm_core::hardware::{self, gib};
use homellm_core::settings::{self, Settings};
use homellm_core::{catalog, download};
use serde_json::{Value, json};
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_global_shortcut::ShortcutState;
use tokio::sync::oneshot;

#[derive(Default)]
struct App {
    agent: tokio::sync::Mutex<Option<Agent>>,
    /// `{"id", "name", "where"}` of the running model.
    model: Mutex<Option<Value>>,
    chats: Mutex<Vec<Chat>>,
    current_chat: Mutex<Option<u64>>,
    /// Risky tool calls waiting for the user's answer in the window.
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    next_id: AtomicU64,
    /// Models being downloaded right now.
    downloading: Mutex<HashSet<String>>,
    /// A model switch asked for in the chat: done once the answer is out.
    switch_to: Mutex<Option<String>>,
}

fn err_text(e: anyhow::Error) -> String {
    format!("{e:#}")
}

/// Asks through a dialog in the window; no answer = no.
struct AskUser {
    app: AppHandle,
}

#[async_trait]
impl Confirm for AskUser {
    async fn confirm(&self, tool: &str, args: &Value) -> bool {
        let state = self.app.state::<App>();
        let id = state.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        state.pending.lock().unwrap().insert(id, tx);
        let _ = self
            .app
            .emit("confirm", json!({"id": id, "tool": tool, "args": args}));
        rx.await.unwrap_or(false)
    }
}

/// The app's own controls as tools.
struct AppTools {
    app: AppHandle,
}

#[async_trait]
impl ToolHost for AppTools {
    fn specs(&self) -> Vec<Value> {
        let id =
            json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]});
        vec![
            json!({"name": "list_models", "description": "Каталог моделей: какие есть, какие скачаны, пойдут ли на этом ПК.",
                   "parameters": {"type": "object", "properties": {}}}),
            json!({"name": "hardware_info", "description": "Железо компьютера: память, видеокарта.",
                   "parameters": {"type": "object", "properties": {}}}),
            json!({"name": "download_model", "description": "Скачать модель из каталога по id (например qwen3-8b).", "parameters": id}),
            json!({"name": "switch_model", "description": "Переключиться на скачанную модель по id.", "parameters": id}),
            json!({"name": "remind", "description": "Напомнить через N минут (системное уведомление). «Через час» = 60.",
                   "parameters": {"type": "object", "properties": {"minutes": {"type": "number"}, "text": {"type": "string"}},
                   "required": ["minutes", "text"]}}),
            json!({"name": "save_scenario", "description": "Запомнить сценарий — несколько действий под одним именем. steps: [{\"tool\": имя инструмента, \"args\": {...}}].",
                   "parameters": {"type": "object", "properties": {"name": {"type": "string"}, "steps": {"type": "array", "items": {"type": "object"}}},
                   "required": ["name", "steps"]}}),
            json!({"name": "run_scenario", "description": "Выполнить сохранённый сценарий по имени («режим кино»).",
                   "parameters": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}}),
            json!({"name": "list_scenarios", "description": "Какие сценарии сохранены.", "parameters": {"type": "object", "properties": {}}}),
            json!({"name": "delete_scenario", "description": "Удалить сценарий по имени.",
                   "parameters": {"type": "object", "properties": {"name": {"type": "string"}}, "required": ["name"]}}),
            json!({"name": "remember", "description": "Запомнить факт о пользователе надолго, во всех чатах («запомни, что я люблю Кино»).",
                   "parameters": {"type": "object", "properties": {"fact": {"type": "string"}}, "required": ["fact"]}}),
            json!({"name": "forget", "description": "Забыть факты о пользователе, в которых есть эти слова.",
                   "parameters": {"type": "object", "properties": {"about": {"type": "string"}}, "required": ["about"]}}),
            json!({"name": "list_reminders", "description": "Какие напоминания стоят.", "parameters": {"type": "object", "properties": {}}}),
            json!({"name": "set_setting", "description": "Изменить настройку приложения. theme: mint (ночь и мята), lime (графит и лайм), violet (полночь и фиалка), amber (тёплый янтарь).",
                   "parameters": {"type": "object", "properties": {
                       "key": {"type": "string", "enum": ["music_dir", "music_search", "models_dir", "theme"]},
                       "value": {"type": "string"}}, "required": ["key", "value"]}}),
        ]
    }

    fn context(&self) -> String {
        memory::context()
    }

    async fn call(&self, name: &str, args: &Value) -> Option<String> {
        let id = args["id"].as_str().unwrap_or_default();
        Some(match name {
            "list_models" => {
                let hw = hardware::detect();
                catalog::all()
                    .iter()
                    .map(|m| {
                        let state = if m.is_downloaded() {
                            "скачана"
                        } else {
                            "не скачана"
                        };
                        format!(
                            "{} — {}, {}, {}, {state}",
                            m.id,
                            m.name,
                            gib(m.size),
                            catalog::fit(m, &hw).label()
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            }
            "hardware_info" => {
                let hw = hardware::detect();
                let gpu = match (hw.gpu_name, hw.vram_bytes) {
                    (Some(name), Some(vram)) => format!("{name}, {}", gib(vram)),
                    _ => "нет".into(),
                };
                format!(
                    "{}; память {}; процессор {} потоков; видеокарта: {gpu}",
                    hw.os,
                    gib(hw.ram_bytes),
                    hw.cpu_threads
                )
            }
            "download_model" => match catalog::find(id) {
                None => format!("нет модели {id}, посмотри list_models"),
                Some(m) if m.is_downloaded() => format!("{} уже скачана", m.name),
                Some(m) => {
                    let app = self.app.clone();
                    let model_id = m.id.clone();
                    tauri::async_runtime::spawn(async move {
                        let _ = pull_model(&app, &model_id).await;
                    });
                    format!(
                        "начал скачивать {} ({}), прогресс видно в окне",
                        m.name,
                        gib(m.size)
                    )
                }
            },
            "switch_model" => match catalog::find(id) {
                None => format!("нет модели {id}, посмотри list_models"),
                Some(m) if !m.is_downloaded() => format!("{} ещё не скачана", m.name),
                Some(m) if !m.is_llm() => {
                    format!("{} — голосовая модель, в чат её не поставить", m.name)
                }
                Some(m) => {
                    *self.app.state::<App>().switch_to.lock().unwrap() = Some(m.id.clone());
                    format!("переключусь на {} сразу после этого ответа", m.name)
                }
            },
            "remind" => {
                let minutes = args["minutes"]
                    .as_f64()
                    .or_else(|| args["minutes"].as_str().and_then(|s| s.parse().ok()));
                match minutes {
                    Some(m) => {
                        let r = reminders::add(m, args["text"].as_str().unwrap_or("напоминание"));
                        format!("напомню {}: {}", reminders::left(r.due), r.text)
                    }
                    None => "ошибка: нужно число минут".into(),
                }
            }
            "save_scenario" => {
                let name = args["name"].as_str().unwrap_or_default().trim().to_string();
                let steps: Vec<scenarios::Step> =
                    serde_json::from_value(args["steps"].clone()).unwrap_or_default();
                let unknown: Vec<&str> = steps
                    .iter()
                    .map(|s| s.tool.as_str())
                    .filter(|t| homellm_core::tools::find(t).is_none() && !HOST_STEPS.contains(t))
                    .collect();
                if name.is_empty() || steps.is_empty() {
                    "ошибка: нужны имя и хотя бы одно действие".into()
                } else if !unknown.is_empty() {
                    format!("ошибка: нет инструментов {}", unknown.join(", "))
                } else {
                    let count = steps.len();
                    scenarios::save(scenarios::Scenario {
                        name: name.clone(),
                        steps,
                    });
                    format!("сценарий «{name}» сохранён: {count} действий")
                }
            }
            "run_scenario" => match scenarios::find(args["name"].as_str().unwrap_or_default()) {
                None => {
                    let names: Vec<String> =
                        scenarios::load().into_iter().map(|s| s.name).collect();
                    format!(
                        "нет такого сценария; есть: {}",
                        if names.is_empty() {
                            "никаких".into()
                        } else {
                            names.join(", ")
                        }
                    )
                }
                Some(scenario) => {
                    let mut report = vec![];
                    for step in &scenario.steps {
                        report.push(format!("{}: {}", step.tool, self.run_step(step).await));
                    }
                    format!(
                        "сценарий «{}» выполнен:\n{}",
                        scenario.name,
                        report.join("\n")
                    )
                }
            },
            "list_scenarios" => {
                let all = scenarios::load();
                if all.is_empty() {
                    "сценариев нет".into()
                } else {
                    all.iter()
                        .map(|s| {
                            format!(
                                "{}: {}",
                                s.name,
                                s.steps
                                    .iter()
                                    .map(|x| x.tool.as_str())
                                    .collect::<Vec<_>>()
                                    .join(" → ")
                            )
                        })
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "delete_scenario" => {
                let name = args["name"].as_str().unwrap_or_default();
                if scenarios::delete(name) {
                    format!("сценарий «{name}» удалён")
                } else {
                    "нет такого сценария".into()
                }
            }
            "remember" => {
                let fact = args["fact"].as_str().unwrap_or_default();
                if memory::remember(fact) {
                    format!("запомнил: {fact}")
                } else {
                    "это я уже помню".into()
                }
            }
            "forget" => {
                let gone = memory::forget(args["about"].as_str().unwrap_or_default());
                if gone.is_empty() {
                    "ничего такого не помню".into()
                } else {
                    format!("забыл: {}", gone.join("; "))
                }
            }
            "list_reminders" => {
                let all = reminders::load();
                if all.is_empty() {
                    "напоминаний нет".into()
                } else {
                    all.iter()
                        .map(|r| format!("{} — {}", reminders::left(r.due), r.text))
                        .collect::<Vec<_>>()
                        .join("\n")
                }
            }
            "set_setting" => {
                let value = args["value"].as_str().unwrap_or_default().to_string();
                let mut s = settings::get();
                match args["key"].as_str().unwrap_or_default() {
                    "music_dir" => s.music_dir = value.clone(),
                    "music_search" => s.music_search = value.clone(),
                    "models_dir" => s.models_dir = value.clone(),
                    "theme" => s.theme = value.clone(),
                    other => return Some(format!("нет настройки {other}")),
                }
                match settings::save(s) {
                    Ok(()) => {
                        let _ = self.app.emit("settings-changed", ());
                        format!("сохранено: {value}")
                    }
                    Err(e) => format!("ошибка: {e}"),
                }
            }
            _ => return None,
        })
    }
}

/// Downloads a model; remembered in the settings until it finishes, so a download cut by
/// closing the app resumes on the next start.
/// App tools a scenario may include (not the ones that manage scenarios themselves).
const HOST_STEPS: &[&str] = &["remind", "set_setting", "switch_model"];

impl AppTools {
    /// One scenario step: a PC tool (risky ones still ask) or one of `HOST_STEPS`.
    async fn run_step(&self, step: &scenarios::Step) -> String {
        if let Some(tool) = homellm_core::tools::find(&step.tool) {
            if tool.risk != homellm_core::tools::Risk::Safe
                && !(AskUser {
                    app: self.app.clone(),
                })
                .confirm(&step.tool, &step.args)
                .await
            {
                return "пользователь запретил".into();
            }
            return (tool.run)(&step.args).unwrap_or_else(|e| format!("ошибка: {e}"));
        }
        if HOST_STEPS.contains(&step.tool.as_str()) {
            return self.call(&step.tool, &step.args).await.unwrap_or_default();
        }
        format!("нет инструмента {}", step.tool)
    }
}

async fn pull_model(app: &AppHandle, id: &str) -> Result<(), String> {
    let model = catalog::find(id).ok_or("нет такой модели")?;
    let state = app.state::<App>();
    if !state.downloading.lock().unwrap().insert(id.to_string()) {
        return Ok(()); // already running
    }
    let mut s = settings::get();
    if !s.downloads.iter().any(|d| d == id) {
        s.downloads.push(id.to_string());
        let _ = settings::save(s);
    }
    let mut last = u64::MAX;
    let result = download::download(&model, |done, total| {
        let permille = done * 1000 / total.max(1);
        if permille != last {
            last = permille;
            let _ = app.emit("progress", json!({"id": id, "done": done, "total": total}));
        }
    })
    .await
    .map_err(err_text);
    state.downloading.lock().unwrap().remove(id);
    let mut s = settings::get();
    s.downloads.retain(|d| d != id);
    let _ = settings::save(s);
    let _ = app.emit(
        "download-done",
        json!({"id": id, "error": result.as_ref().err()}),
    );
    result
}

/// Loads a model and continues the open chat with it.
async fn load_model(app: &AppHandle, id: &str) -> Result<Value, String> {
    let state = app.state::<App>();
    let model = catalog::find(id).ok_or("нет такой модели")?;
    if !model.is_llm() {
        return Err("голосовые модели заработают вместе с голосовым режимом".into());
    }
    if !model.is_downloaded() {
        return Err("модель ещё не скачана".into());
    }
    let entry = model.clone();
    let (engine, on_gpu) = tokio::task::spawn_blocking(move || LlamaEngine::load_entry(&entry))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err_text)?;
    let host: Arc<dyn ToolHost> = Arc::new(AppTools { app: app.clone() });
    let mut agent = Agent::with_host(Box::new(engine), &model.system_suffix, Some(host));
    if let Some(chat) = current_chat(&state) {
        agent.set_history(chat.history);
    }
    *state.agent.lock().await = Some(agent);
    let info = json!({"id": model.id, "name": model.name, "where": if on_gpu { "видеокарта" } else { "процессор" }});
    *state.model.lock().unwrap() = Some(info.clone());
    let mut s = settings::get();
    s.last_model = model.id.clone();
    let _ = settings::save(s);
    let _ = app.emit("model-changed", &info);
    Ok(info)
}

fn current_chat(state: &App) -> Option<Chat> {
    let id = (*state.current_chat.lock().unwrap())?;
    state
        .chats
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.id == id)
        .cloned()
}

#[tauri::command]
fn hw_info() -> Value {
    let hw = hardware::detect();
    json!({"os": hw.os, "cpu_threads": hw.cpu_threads, "ram": gib(hw.ram_bytes), "gpu": hw.gpu_name, "vram": hw.vram_bytes.map(gib)})
}

#[tauri::command]
fn list_models(state: State<'_, App>) -> Value {
    let downloading = state.downloading.lock().unwrap().clone();
    let hw = hardware::detect();
    let speeds = settings::get().speeds;
    let recommended = catalog::recommend(&hw).map(|m| m.id);
    let models: Vec<Value> = catalog::all()
        .into_iter()
        .map(|m| {
            let fit = catalog::fit(&m, &hw);
            json!({
                "id": m.id, "kind": m.kind, "name": m.name, "size": gib(m.size), "about": m.about, "tools": m.tools,
                "fit": format!("{fit:?}"), "fit_label": fit.label(), "downloaded": m.is_downloaded(),
                "speed": speeds.get(&m.id).map(|s| s.round()),
                "new": catalog::is_new(&m.id),
                "bytes": m.size, "partial": m.partial(), "downloading": downloading.contains(&m.id),
                "recommended": recommended.as_deref() == Some(m.id.as_str()),
            })
        })
        .collect();
    json!({"models": models, "dir": homellm_core::models_dir()})
}

#[tauri::command]
async fn pull(app: AppHandle, id: String) -> Result<(), String> {
    pull_model(&app, &id).await
}

#[tauri::command]
async fn load(app: AppHandle, id: String) -> Result<Value, String> {
    load_model(&app, &id).await
}

/// On start: the last used model, else the biggest downloaded one.
#[tauri::command]
async fn auto_load(app: AppHandle) -> Result<Option<Value>, String> {
    let last = settings::get().last_model;
    let pick = catalog::find(&last)
        .filter(|m| m.is_downloaded())
        .or_else(catalog::default_local);
    match pick {
        Some(m) => load_model(&app, &m.id).await.map(Some),
        None => Ok(None),
    }
}

#[tauri::command]
fn current(state: State<'_, App>) -> Option<Value> {
    state.model.lock().unwrap().clone()
}

#[tauri::command]
async fn send(app: AppHandle, state: State<'_, App>, text: String) -> Result<String, String> {
    let mut guard = state.agent.lock().await;
    let agent = guard
        .as_mut()
        .ok_or("Сначала скачайте и запустите модель — кнопка с её именем вверху")?;
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let forward = {
        let app = app.clone();
        tokio::spawn(async move {
            while let Some(piece) = rx.recv().await {
                let _ = app.emit("token", piece);
            }
        })
    };
    let confirm = AskUser { app: app.clone() };
    let answer = agent
        .send(&text, &confirm, Some(tx), |event| {
            let _ = match event {
                Event::ToolCall { name, args } => {
                    app.emit("tool", json!({"name": name, "args": args}))
                }
                Event::ToolResult { name, result } => {
                    app.emit("tool-result", json!({"name": name, "result": result}))
                }
            };
        })
        .await;
    let _ = forward.await;
    save_chat(&state, &text, agent.history().to_vec());
    record_speed(&app, &state);
    drop(guard);
    let _ = app.emit("chats-changed", ());

    let switch = state.switch_to.lock().unwrap().take();
    if let Some(id) = switch {
        load_model(&app, &id).await?;
    }
    answer.map_err(err_text)
}

/// Remembers how fast the running model answered on this PC (a running average).
fn record_speed(app: &AppHandle, state: &App) {
    let Some(speed) = homellm_core::engine::llama::last_speed() else {
        return;
    };
    let Some(id) = state
        .model
        .lock()
        .unwrap()
        .as_ref()
        .and_then(|m| m["id"].as_str().map(String::from))
    else {
        return;
    };
    let mut s = settings::get();
    let avg = s
        .speeds
        .get(&id)
        .map_or(speed, |old| old * 0.7 + speed * 0.3);
    s.speeds.insert(id.clone(), avg);
    let _ = settings::save(s);
    let _ = app.emit("speed", json!({"id": id, "speed": avg.round()}));
}

/// Stores the conversation in the open chat, creating one on the first message.
fn save_chat(state: &App, first_text: &str, history: Vec<Message>) {
    let mut chats = state.chats.lock().unwrap();
    let mut current = state.current_chat.lock().unwrap();
    let id = match *current {
        Some(id) if chats.iter().any(|c| c.id == id) => id,
        _ => {
            let id = chats.iter().map(|c| c.id).max().unwrap_or(0) + 1;
            chats.push(Chat {
                id,
                title: chats::title_from(first_text),
                updated: 0,
                history: vec![],
            });
            *current = Some(id);
            id
        }
    };
    if let Some(chat) = chats.iter_mut().find(|c| c.id == id) {
        chat.history = history;
        chat.updated = chats::now();
    }
    let _ = chats::save(&chats);
}

#[tauri::command]
fn list_chats(state: State<'_, App>, query: Option<String>) -> Value {
    let mut chats = state.chats.lock().unwrap().clone();
    // Search by title and by what was said (not by tool results).
    if let Some(q) = query
        .map(|q| q.trim().to_lowercase())
        .filter(|q| !q.is_empty())
    {
        chats.retain(|c| {
            c.title.to_lowercase().contains(&q)
                || c.history.iter().any(|m| {
                    !m.content.starts_with("<tool_response>")
                        && m.content.to_lowercase().contains(&q)
                })
        });
    }
    chats.sort_by(|a, b| b.updated.cmp(&a.updated));
    let current = *state.current_chat.lock().unwrap();
    let list: Vec<Value> = chats
        .iter()
        .map(|c| json!({"id": c.id, "title": c.title}))
        .collect();
    json!({"chats": list, "current": current})
}

#[tauri::command]
async fn open_chat(state: State<'_, App>, id: u64) -> Result<Vec<Message>, String> {
    let chat = state
        .chats
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.id == id)
        .cloned()
        .ok_or("чат не найден")?;
    *state.current_chat.lock().unwrap() = Some(id);
    if let Some(agent) = state.agent.lock().await.as_mut() {
        agent.set_history(chat.history.clone());
    }
    Ok(chat.history)
}

#[tauri::command]
async fn new_chat(state: State<'_, App>) -> Result<(), String> {
    *state.current_chat.lock().unwrap() = None;
    if let Some(agent) = state.agent.lock().await.as_mut() {
        agent.reset();
    }
    Ok(())
}

#[tauri::command]
fn rename_chat(state: State<'_, App>, id: u64, title: String) -> Result<(), String> {
    let mut chats = state.chats.lock().unwrap();
    if let Some(chat) = chats.iter_mut().find(|c| c.id == id) {
        chat.title = title.trim().chars().take(60).collect();
    }
    chats::save(&chats).map_err(err_text)
}

#[tauri::command]
async fn delete_chat(state: State<'_, App>, id: u64) -> Result<(), String> {
    {
        let mut chats = state.chats.lock().unwrap();
        chats.retain(|c| c.id != id);
        chats::save(&chats).map_err(err_text)?;
    }
    let was_open = *state.current_chat.lock().unwrap() == Some(id);
    if was_open {
        *state.current_chat.lock().unwrap() = None;
        if let Some(agent) = state.agent.lock().await.as_mut() {
            agent.reset();
        }
    }
    Ok(())
}

/// The context is ~8k tokens: a file gets about a third of it.
const ATTACHMENT_LIMIT: usize = 6000;

/// Reads a file dropped into the chat: text-like files as is, PDF through a text extractor.
#[tauri::command]
fn read_attachment(path: String) -> Result<Value, String> {
    let file = std::path::Path::new(&path);
    let name = file
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let ext = file
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let text = match ext.as_str() {
        "pdf" => {
            pdf_extract::extract_text(file).map_err(|e| format!("не удалось прочитать PDF: {e}"))?
        }
        "gguf" | "exe" | "dll" | "zip" | "7z" | "rar" | "png" | "jpg" | "jpeg" | "gif" | "webp"
        | "mp3" | "mp4" | "docx" | "xlsx" => {
            return Err(format!(
                "{name}: такие файлы пока не читаю — только текст и PDF"
            ));
        }
        _ => {
            let bytes = std::fs::read(file).map_err(|e| e.to_string())?;
            if bytes.iter().take(4096).any(|b| *b == 0) {
                return Err(format!("{name}: это не текстовый файл"));
            }
            String::from_utf8_lossy(&bytes).into_owned()
        }
    };
    let text = text.trim();
    let mut cut: String = text.chars().take(ATTACHMENT_LIMIT).collect();
    let truncated = text.chars().count() > ATTACHMENT_LIMIT;
    if truncated {
        cut.push_str("\n…(дальше обрезано)");
    }
    Ok(json!({"name": name, "text": cut, "truncated": truncated}))
}

/// Adds a .gguf file from anywhere on disk as the user's own model.
#[tauri::command]
fn add_model(path: String) -> Result<(), String> {
    let path = path.trim().trim_matches('"').to_string();
    if !std::path::Path::new(&path).is_file() || !path.to_lowercase().ends_with(".gguf") {
        return Err("нужен путь к существующему файлу .gguf".into());
    }
    let mut s = settings::get();
    if !s.custom_models.contains(&path) {
        s.custom_models.push(path);
    }
    settings::save(s).map_err(err_text)
}

/// Forgets a model added by path (the file stays on disk).
#[tauri::command]
fn forget_model(id: String) -> Result<(), String> {
    let mut s = settings::get();
    s.custom_models.retain(|p| format!("local:{p}") != id);
    settings::save(s).map_err(err_text)
}

#[tauri::command]
fn open_models_dir() -> Result<(), String> {
    catalog::open_models_dir().map_err(err_text)
}

/// Saves a chat as Markdown into Downloads and returns the file path.
#[tauri::command]
fn export_chat(state: State<'_, App>, id: u64) -> Result<String, String> {
    let chat = state
        .chats
        .lock()
        .unwrap()
        .iter()
        .find(|c| c.id == id)
        .cloned()
        .ok_or("чат не найден")?;
    let mut md = format!(
        "# {}

",
        chat.title
    );
    for m in &chat.history {
        let text = m.content.trim();
        if m.role == "user" && text.starts_with("<tool_response>") {
            md.push_str(&format!(
                "> ✓ {}

",
                text.trim_start_matches("<tool_response>")
                    .trim_end_matches("</tool_response>")
                    .trim()
            ));
        } else if m.role == "user" && !text.starts_with("Ты не вызвал инструмент")
        {
            md.push_str(&format!(
                "**Я:** {text}

"
            ));
        } else if m.role == "assistant" && !text.starts_with('{') && !text.contains("<tool_call>") {
            md.push_str(&format!(
                "**HomeLLM:** {text}

"
            ));
        }
    }
    let dir = directories::UserDirs::new()
        .and_then(|d| d.download_dir().map(|p| p.to_path_buf()))
        .unwrap_or_else(homellm_core::data_dir);
    let safe: String = chat
        .title
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == ' ' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let path = dir.join(format!("HomeLLM — {}.md", safe.trim()));
    std::fs::write(&path, md).map_err(|e| e.to_string())?;
    Ok(path.display().to_string())
}

#[tauri::command]
fn answer(app: AppHandle, state: State<'_, App>, id: u64, allow: bool) {
    if let Some(tx) = state.pending.lock().unwrap().remove(&id) {
        let _ = tx.send(allow);
    }
    // Both windows may show the question: the other one closes it.
    let _ = app.emit("confirm-done", json!({"id": id}));
}

/// Brings the main window up from the tray or from behind other windows.
fn show_main(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

/// The global hotkey: the quick-ask bar above all windows, like Spotlight.
fn toggle_quick(app: &AppHandle) {
    let window = match app.get_webview_window("quick") {
        Some(window) => window,
        None => match tauri::WebviewWindowBuilder::new(
            app,
            "quick",
            tauri::WebviewUrl::App("quick.html".into()),
        )
        .title("HomeLLM — быстрый вопрос")
        .inner_size(680.0, 90.0)
        .decorations(false)
        .transparent(true)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .visible(false)
        .build()
        {
            Ok(window) => window,
            Err(_) => return show_main(app),
        },
    };
    if window.is_visible().unwrap_or(false) && window.is_focused().unwrap_or(false) {
        let _ = window.hide();
        return;
    }
    // Upper third of the screen, centred.
    if let Ok(Some(m)) = window.current_monitor() {
        let size = m.size().to_logical::<f64>(m.scale_factor());
        let _ = window.set_position(tauri::LogicalPosition::new(
            (size.width - 680.0) / 2.0,
            size.height * 0.22,
        ));
    }
    let _ = window.show();
    let _ = window.set_focus();
    let _ = app.emit_to("quick", "quick-open", ());
}

/// The desktop pet: a small transparent window above the others, bottom right.
fn set_pet(app: &AppHandle, on: bool) {
    let existing = app.get_webview_window("pet");
    if !on {
        if let Some(window) = existing {
            let _ = window.close();
        }
        return;
    }
    if existing.is_some() {
        return;
    }
    let (x, y) = app
        .primary_monitor()
        .ok()
        .flatten()
        .map(|m| {
            let size = m.size().to_logical::<f64>(m.scale_factor());
            (size.width - 150.0, size.height - 190.0)
        })
        .unwrap_or((40.0, 40.0));
    let _ = tauri::WebviewWindowBuilder::new(app, "pet", tauri::WebviewUrl::App("pet.html".into()))
        .title("HomeLLM — питомец")
        .inner_size(120.0, 120.0)
        .position(x, y)
        .transparent(true)
        .decorations(false)
        .shadow(false)
        .always_on_top(true)
        .skip_taskbar(true)
        .resizable(false)
        .build();
}

#[tauri::command]
fn show_main_window(app: AppHandle) {
    show_main(&app);
}

/// Keeps the Windows autostart entry in line with the setting.
fn apply_autostart(app: &AppHandle, on: bool) {
    let launch = app.autolaunch();
    let _ = if on {
        launch.enable()
    } else {
        launch.disable()
    };
}

fn build_tray(app: &tauri::App) -> tauri::Result<()> {
    let show = MenuItem::with_id(app, "show", "Открыть HomeLLM", true, None::<&str>)?;
    let new = MenuItem::with_id(app, "new", "Новый чат", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Выход", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&show, &new, &quit])?;
    let mut tray = TrayIconBuilder::with_id("main")
        .tooltip("HomeLLM — Alt+Space")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "show" => show_main(app),
            "new" => {
                show_main(app);
                let _ = app.emit("new-chat", ());
            }
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } = event
            {
                show_main(tray.app_handle());
            }
        });
    if let Some(icon) = app.default_window_icon() {
        tray = tray.icon(icon.clone());
    }
    tray.build(app)?;
    Ok(())
}

#[tauri::command]
fn get_settings() -> Settings {
    settings::get()
}

#[tauri::command]
fn save_settings(app: AppHandle, value: Settings) -> Result<(), String> {
    if value.autostart != settings::get().autostart {
        apply_autostart(&app, value.autostart);
    }
    set_pet(&app, !value.hide_pet);
    let mut s = value;
    let saved = settings::get();
    s.last_model = saved.last_model;
    s.downloads = saved.downloads;
    s.custom_models = saved.custom_models;
    s.speeds = saved.speeds;
    settings::save(s).map_err(err_text)
}

fn main() {
    let app = App {
        chats: Mutex::new(chats::load()),
        ..Default::default()
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_autostart::init(
            MacosLauncher::LaunchAgent,
            Some(vec!["--minimized"]),
        ))
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, _shortcut, event| {
                    if event.state == ShortcutState::Pressed {
                        toggle_quick(app);
                    }
                })
                .build(),
        )
        .manage(app)
        .on_window_event(|window, event| {
            // Closing hides into the tray: the model stays loaded and answers at once.
            if let WindowEvent::CloseRequested { api, .. } = event
                && window.label() == "main"
                && !settings::get().quit_on_close
            {
                let _ = window.hide();
                api.prevent_close();
            }
        })
        .setup(|app| {
            build_tray(app)?;
            {
                use tauri_plugin_global_shortcut::GlobalShortcutExt;
                // Alt+Space may be taken (PowerToys Run): the app works without it.
                if let Err(e) = app.global_shortcut().register("alt+space") {
                    eprintln!("hotkey Alt+Space is not available: {e}");
                }
            }
            apply_autostart(app.handle(), settings::get().autostart);
            set_pet(app.handle(), !settings::get().hide_pet);
            // A fresher model catalog from GitHub, once a day; offline is fine.
            tauri::async_runtime::spawn(async {
                if let Err(e) = catalog::refresh().await {
                    eprintln!("catalog refresh skipped: {e:#}");
                }
            });
            // Fire due reminders: a system notification plus a line in the chat.
            let handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tauri_plugin_notification::NotificationExt;
                loop {
                    for r in reminders::take_due() {
                        let _ = handle
                            .notification()
                            .builder()
                            .title("HomeLLM — напоминание")
                            .body(&r.text)
                            .show();
                        let _ = handle.emit("reminder", &r.text);
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            });
            if std::env::args().any(|a| a == "--minimized")
                && let Some(window) = app.get_webview_window("main")
            {
                let _ = window.hide();
            }
            // Resume the downloads the last run did not finish.
            for id in settings::get().downloads {
                let handle = app.handle().clone();
                tauri::async_runtime::spawn(async move {
                    let _ = pull_model(&handle, &id).await;
                });
            }
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            hw_info,
            list_models,
            pull,
            load,
            auto_load,
            current,
            send,
            list_chats,
            open_chat,
            new_chat,
            rename_chat,
            delete_chat,
            export_chat,
            add_model,
            read_attachment,
            show_main_window,
            forget_model,
            open_models_dir,
            answer,
            get_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("failed to start HomeLLM");
}
