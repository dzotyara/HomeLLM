//! The curated list of models the user picks from (`catalog/models.json`).

use std::path::PathBuf;

use serde::Deserialize;

use crate::hardware::Hardware;

const BUILTIN: &str = include_str!("../../../catalog/models.json");

/// A file a model needs besides the main one (e.g. a Piper voice's config).
#[derive(Debug, Clone, Deserialize)]
pub struct ExtraFile {
    pub url: String,
    pub file: String,
    pub size: u64,
    #[serde(default)]
    pub sha256: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ModelEntry {
    pub id: String,
    /// `chat` (talks and controls the PC), `code` (programming) or `voice` (speech in/out,
    /// used by the voice mode, not by the chat engine).
    #[serde(default = "chat_kind")]
    pub kind: String,
    pub name: String,
    pub params_b: f32,
    pub url: String,
    pub file: String,
    pub size: u64,
    pub sha256: String,
    pub context: u32,
    /// Reliable at calling tools (the PC-control protocol).
    pub tools: bool,
    /// Appended to the system prompt, e.g. `/no_think` for Qwen3.
    #[serde(default)]
    pub system_suffix: String,
    pub about: String,
    #[serde(default)]
    pub extra: Vec<ExtraFile>,
}

fn chat_kind() -> String {
    "chat".into()
}

impl ModelEntry {
    pub fn path(&self) -> PathBuf {
        crate::models_dir().join(&self.file)
    }

    pub fn is_downloaded(&self) -> bool {
        let has = |file: &str, size: u64| {
            std::fs::metadata(crate::models_dir().join(file)).is_ok_and(|m| m.len() == size)
        };
        has(&self.file, self.size) && self.extra.iter().all(|e| has(&e.file, e.size))
    }

    /// Runs in the chat engine (not a voice model).
    pub fn is_llm(&self) -> bool {
        self.kind != "voice"
    }
}

/// How well a model fits the machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Fit {
    /// Fits in video memory: fast.
    Gpu,
    /// Fits in RAM only: works, but slowly.
    Cpu,
    TooBig,
}

impl Fit {
    pub fn label(self) -> &'static str {
        match self {
            Fit::Gpu => "быстро (видеокарта)",
            Fit::Cpu => "медленно (процессор)",
            Fit::TooBig => "не влезет",
        }
    }
}

/// Weights plus ~20% for the KV cache and buffers.
pub fn fit(model: &ModelEntry, hw: &Hardware) -> Fit {
    let need = model.size + model.size / 5;
    if hw.vram_bytes.is_some_and(|v| need <= v) {
        Fit::Gpu
    } else if need <= hw.ram_bytes * 7 / 10 {
        Fit::Cpu
    } else {
        Fit::TooBig
    }
}

pub fn load() -> Vec<ModelEntry> {
    serde_json::from_str(BUILTIN).expect("catalog/models.json is valid")
}

pub fn find(id: &str) -> Option<ModelEntry> {
    load().into_iter().find(|m| m.id == id)
}

/// The largest downloaded tool-capable model: what to start when the user named none.
pub fn default_local() -> Option<ModelEntry> {
    load()
        .into_iter()
        .filter(|m| m.kind == "chat" && m.tools && m.is_downloaded())
        .max_by(|a, b| a.params_b.total_cmp(&b.params_b))
}

/// The biggest tool-capable model that runs on the GPU, else the biggest that runs at all.
pub fn recommend(hw: &Hardware) -> Option<ModelEntry> {
    let mut models: Vec<_> = load()
        .into_iter()
        .filter(|m| m.kind == "chat" && m.tools)
        .collect();
    // The smartest first: more parameters beat a bigger file of a smaller model.
    models.sort_by(|a, b| b.params_b.total_cmp(&a.params_b).then(b.size.cmp(&a.size)));
    let pick = |want: Fit| models.iter().find(|m| fit(m, hw) == want).cloned();
    pick(Fit::Gpu).or_else(|| pick(Fit::Cpu))
}
