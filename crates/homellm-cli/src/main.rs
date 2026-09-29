//! `homellm`: the console shell around homellm-core until the Tauri app lands.

use std::io::{BufRead, Write};

use anyhow::{Result, bail};
use async_trait::async_trait;
use clap::{Parser, Subcommand};
use homellm_core::agent::{Agent, Confirm, Event};
use homellm_core::engine::Engine;
use homellm_core::engine::openai::OpenAiEngine;
use homellm_core::hardware::{self, gib};
use homellm_core::{catalog, download};
use serde_json::Value;

#[derive(Parser)]
#[command(
    name = "homellm",
    about = "Локальная LLM, которая управляет компьютером"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Железо и какую модель оно потянет
    Hw,
    /// Каталог моделей
    Models,
    /// Скачать модель из каталога
    Pull { id: String },
    /// Разговор с моделью (с управлением ПК)
    Chat {
        /// Модель из каталога; по умолчанию — рекомендованная и уже скачанная
        #[arg(long)]
        model: Option<String>,
        /// Внешний OpenAI-совместимый сервер, например http://localhost:11434 (Ollama)
        #[arg(long)]
        server: Option<String>,
        /// Имя модели на внешнем сервере, например qwen3:8b
        #[arg(long, requires = "server")]
        server_model: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Hw => hw(),
        Command::Models => models(),
        Command::Pull { id } => pull(&id).await,
        Command::Chat {
            model,
            server,
            server_model,
        } => chat(model, server, server_model).await,
    }
}

fn hw() -> Result<()> {
    let hw = hardware::detect();
    println!("ОС: {}", hw.os);
    println!(
        "Процессор: {} потоков, память: {}",
        hw.cpu_threads,
        gib(hw.ram_bytes)
    );
    match (&hw.gpu_name, hw.vram_bytes) {
        (Some(name), Some(vram)) => println!("Видеокарта: {name}, {}", gib(vram)),
        _ => println!("Видеокарта: не найдена (модели пойдут на процессоре)"),
    }
    if let Some(m) = catalog::recommend(&hw) {
        println!("\nРекомендую: {} — `homellm pull {}`", m.name, m.id);
    }
    Ok(())
}

fn models() -> Result<()> {
    let hw = hardware::detect();
    for m in catalog::load() {
        let mark = if m.is_downloaded() { "✓" } else { " " };
        let tools = if m.tools {
            ""
        } else {
            " [без инструментов]"
        };
        println!(
            "{mark} {:<12} {:<32} {:>8}  {}{tools}\n               {}",
            m.id,
            m.name,
            gib(m.size),
            catalog::fit(&m, &hw).label(),
            m.about
        );
    }
    println!("\nПапка моделей: {}", homellm_core::models_dir().display());
    Ok(())
}

async fn pull(id: &str) -> Result<()> {
    let Some(model) = catalog::find(id) else {
        bail!("нет модели {id}, см. `homellm models`")
    };
    println!(
        "Скачиваю {} ({}) в {}",
        model.name,
        gib(model.size),
        model.path().display()
    );
    let mut shown = 0;
    download::download(&model, |done, total| {
        let percent = done * 100 / total.max(1);
        if percent != shown {
            shown = percent;
            print!("\r{percent:>3}%  {} / {}", gib(done), gib(total));
            let _ = std::io::stdout().flush();
        }
    })
    .await?;
    println!("\nГотово, контрольная сумма совпала.");
    Ok(())
}

struct AskInTerminal;

#[async_trait]
impl Confirm for AskInTerminal {
    async fn confirm(&self, tool: &str, args: &Value) -> bool {
        print!("\n⚠ Разрешить {tool} {args}? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        matches!(
            answer.trim().to_lowercase().as_str(),
            "y" | "yes" | "д" | "да"
        )
    }
}

async fn chat(
    model: Option<String>,
    server: Option<String>,
    server_model: Option<String>,
) -> Result<()> {
    let (engine, suffix): (Box<dyn Engine>, String) = match server {
        Some(url) => {
            let name = server_model.unwrap_or_else(|| "qwen3:8b".into());
            (Box::new(OpenAiEngine::new(&url, &name)), String::new())
        }
        None => local_engine(model)?,
    };
    let mut agent = Agent::new(engine, &suffix);
    println!("Модель: {}. Пустая строка — выход.", agent.engine_name());

    let stdin = std::io::stdin();
    loop {
        print!("\n> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        if stdin.lock().read_line(&mut line)? == 0 || line.trim().is_empty() {
            return Ok(());
        }
        let answer = agent
            .send(line.trim(), &AskInTerminal, None, |event| match event {
                Event::ToolCall { name, args } => println!("  → {name} {args}"),
                Event::ToolResult { result, .. } => println!("  ← {result}"),
            })
            .await;
        match answer {
            Ok(text) => println!("{text}"),
            Err(e) => println!("Ошибка: {e:#}"),
        }
    }
}

#[cfg(feature = "llama-cpu")]
fn local_engine(id: Option<String>) -> Result<(Box<dyn Engine>, String)> {
    use homellm_core::engine::llama::LlamaEngine;
    let model = match id {
        Some(id) => catalog::find(&id).ok_or_else(|| anyhow::anyhow!("нет модели {id}"))?,
        None => catalog::default_local().ok_or_else(|| {
            anyhow::anyhow!("нет скачанных моделей: `homellm hw`, потом `homellm pull <id>`")
        })?,
    };
    if !model.is_downloaded() {
        bail!("модель не скачана: `homellm pull {}`", model.id);
    }
    println!("Загружаю {}…", model.name);
    let (engine, on_gpu) = LlamaEngine::load_entry(&model)?;
    println!(
        "{}",
        if on_gpu {
            "Считаю на видеокарте."
        } else {
            "Считаю на процессоре."
        }
    );
    Ok((Box::new(engine), model.system_suffix))
}

#[cfg(not(feature = "llama-cpu"))]
fn local_engine(_: Option<String>) -> Result<(Box<dyn Engine>, String)> {
    bail!("собрано без llama.cpp: используйте --server (Ollama, LM Studio)")
}
