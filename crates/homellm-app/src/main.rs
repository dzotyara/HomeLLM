//! HomeLLM desktop app: a Tauri shell around homellm-core. The window (`ui/`)
//! calls these commands and listens to events: `progress` (downloads), `token`
//! (streamed answer), `tool` / `tool-result` (PC actions) and `confirm` (asks the user).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use homellm_core::agent::{Agent, Confirm, Event};
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
    model_id: Mutex<Option<String>>,
    /// Risky tool calls waiting for the user's answer in the window.
    pending: Mutex<HashMap<u64, oneshot::Sender<bool>>>,
    next_id: AtomicU64,
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

fn err_text(e: anyhow::Error) -> String {
    format!("{e:#}")
}

#[tauri::command]
fn hw_info() -> Value {
    let hw = hardware::detect();
    json!({
        "os": hw.os,
        "cpu_threads": hw.cpu_threads,
        "ram": gib(hw.ram_bytes),
        "gpu": hw.gpu_name,
        "vram": hw.vram_bytes.map(gib),
    })
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
                "id": m.id,
                "name": m.name,
                "size": gib(m.size),
                "about": m.about,
                "tools": m.tools,
                "fit": format!("{fit:?}"),
                "fit_label": fit.label(),
                "downloaded": m.is_downloaded(),
                "recommended": recommended.as_deref() == Some(m.id.as_str()),
            })
        })
        .collect();
    json!({"models": models, "dir": homellm_core::models_dir()})
}

#[tauri::command]
async fn pull(app: AppHandle, id: String) -> Result<(), String> {
    let model = catalog::find(&id).ok_or("нет такой модели")?;
    let mut last = u64::MAX;
    download::download(&model, |done, total| {
        let permille = done * 1000 / total.max(1);
        if permille != last {
            last = permille;
            let _ = app.emit("progress", json!({"id": id, "done": done, "total": total}));
        }
    })
    .await
    .map_err(err_text)
}

/// Loads a downloaded model and starts a new conversation; says where it runs.
#[tauri::command]
async fn load(state: State<'_, App>, id: String) -> Result<String, String> {
    let model = catalog::find(&id).ok_or("нет такой модели")?;
    if !model.is_downloaded() {
        return Err("модель ещё не скачана".into());
    }
    let entry = model.clone();
    let (engine, on_gpu) = tokio::task::spawn_blocking(move || LlamaEngine::load_entry(&entry))
        .await
        .map_err(|e| e.to_string())?
        .map_err(err_text)?;
    *state.agent.lock().await = Some(Agent::new(Box::new(engine), &model.system_suffix));
    *state.model_id.lock().unwrap() = Some(id);
    Ok(if on_gpu {
        "видеокарта"
    } else {
        "процессор"
    }
    .into())
}

#[tauri::command]
fn current(state: State<'_, App>) -> Option<String> {
    state.model_id.lock().unwrap().clone()
}

#[tauri::command]
async fn send(app: AppHandle, state: State<'_, App>, text: String) -> Result<String, String> {
    let mut guard = state.agent.lock().await;
    let agent = guard
        .as_mut()
        .ok_or("Сначала выберите модель на вкладке «Модели»")?;
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
    answer.map_err(err_text)
}

#[tauri::command]
async fn reset(state: State<'_, App>) -> Result<(), String> {
    if let Some(agent) = state.agent.lock().await.as_mut() {
        agent.reset();
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
    settings::save(value).map_err(err_text)
}

fn main() {
    tauri::Builder::default()
        .manage(App::default())
        .invoke_handler(tauri::generate_handler![
            hw_info,
            list_models,
            pull,
            load,
            current,
            send,
            reset,
            answer,
            get_settings,
            save_settings
        ])
        .run(tauri::generate_context!())
        .expect("failed to start HomeLLM");
}
