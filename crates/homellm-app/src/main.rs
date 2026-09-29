//! HomeLLM desktop app: a Tauri shell around homellm-core. The window (`ui/`)
//! calls these commands and listens to events: `progress` / `download-done`,
//! `token` (streamed answer), `tool` / `tool-result` (actions), `confirm` (asks
//! the user), `model-changed`, `chats-changed`, `settings-changed`.
//! Everything the window can do is also a tool, so it can be asked for in the chat.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod chats;

use std::collections::HashMap;
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
use tauri::{AppHandle, Emitter, Manager, State};
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
            json!({"name": "set_setting", "description": "Изменить настройку приложения. theme: mint (ночь и мята), lime (графит и лайм), violet (полночь и фиалка), amber (тёплый янтарь).",
                   "parameters": {"type": "object", "properties": {
                       "key": {"type": "string", "enum": ["music_dir", "music_search", "models_dir", "theme"]},
                       "value": {"type": "string"}}, "required": ["key", "value"]}}),
        ]
    }

    async fn call(&self, name: &str, args: &Value) -> Option<String> {
        let id = args["id"].as_str().unwrap_or_default();
        Some(match name {
            "list_models" => {
                let hw = hardware::detect();
                catalog::load()
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
                Some(m) => {
                    *self.app.state::<App>().switch_to.lock().unwrap() = Some(m.id.clone());
                    format!("переключусь на {} сразу после этого ответа", m.name)
                }
            },
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

async fn pull_model(app: &AppHandle, id: &str) -> Result<(), String> {
    let model = catalog::find(id).ok_or("нет такой модели")?;
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
fn list_models() -> Value {
    let hw = hardware::detect();
    let recommended = catalog::recommend(&hw).map(|m| m.id);
    let models: Vec<Value> = catalog::load()
        .into_iter()
        .map(|m| {
            let fit = catalog::fit(&m, &hw);
            json!({
                "id": m.id, "name": m.name, "size": gib(m.size), "about": m.about, "tools": m.tools,
                "fit": format!("{fit:?}"), "fit_label": fit.label(), "downloaded": m.is_downloaded(),
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
    drop(guard);
    let _ = app.emit("chats-changed", ());

    let switch = state.switch_to.lock().unwrap().take();
    if let Some(id) = switch {
        load_model(&app, &id).await?;
    }
    answer.map_err(err_text)
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
fn list_chats(state: State<'_, App>) -> Value {
    let mut chats = state.chats.lock().unwrap().clone();
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

#[tauri::command]
fn answer(state: State<'_, App>, id: u64, allow: bool) {
    if let Some(tx) = state.pending.lock().unwrap().remove(&id) {
        let _ = tx.send(allow);
    }
}

#[tauri::command]
fn get_settings() -> Settings {
    settings::get()
}

#[tauri::command]
fn save_settings(value: Settings) -> Result<(), String> {
    let mut s = value;
    s.last_model = settings::get().last_model;
    settings::save(s).map_err(err_text)
}

fn main() {
    let app = App {
        chats: Mutex::new(chats::load()),
        ..Default::default()
    };
    tauri::Builder::default()
        .manage(app)
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
            answer,
            get_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("failed to start HomeLLM");
}
