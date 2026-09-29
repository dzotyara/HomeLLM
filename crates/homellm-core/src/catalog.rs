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
    /// For the user's own models: the file itself (not in the catalog).
    #[serde(skip)]
    pub local_path: Option<PathBuf>,
}

fn chat_kind() -> String {
    "chat".into()
}

impl ModelEntry {
    pub fn path(&self) -> PathBuf {
        self.local_path
            .clone()
            .unwrap_or_else(|| crate::models_dir().join(&self.file))
    }

    pub fn is_downloaded(&self) -> bool {
        if let Some(path) = &self.local_path {
            return path.is_file();
        }
        let has = |file: &str, size: u64| {
            std::fs::metadata(crate::models_dir().join(file)).is_ok_and(|m| m.len() == size)
        };
        has(&self.file, self.size) && self.extra.iter().all(|e| has(&e.file, e.size))
    }

    /// Bytes of the main file already downloaded by an interrupted download.
    pub fn partial(&self) -> u64 {
        std::fs::metadata(crate::models_dir().join(format!("{}.part", self.file)))
            .map_or(0, |m| m.len())
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
    all().into_iter().find(|m| m.id == id)
}

/// The catalog plus the user's own models.
pub fn all() -> Vec<ModelEntry> {
    let mut models = load();
    models.extend(local_models());
    models
}

/// Any .gguf in the models folder that the catalog does not know, plus files added by path.
pub fn local_models() -> Vec<ModelEntry> {
    let known: std::collections::HashSet<String> = load().into_iter().map(|m| m.file).collect();
    let mut paths: Vec<PathBuf> = std::fs::read_dir(crate::models_dir())
        .map(|dir| dir.flatten().map(|e| e.path()).collect())
        .unwrap_or_default();
    paths.retain(|p| {
        let name = p
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        name.ends_with(".gguf")
            && !name.contains("mmproj")
            && !known.iter().any(|k| k.to_lowercase() == name)
    });
    paths.extend(
        crate::settings::get()
            .custom_models
            .iter()
            .map(PathBuf::from),
    );
    paths.dedup();
    paths.into_iter().map(|path| local_entry(&path)).collect()
}

fn local_entry(path: &std::path::Path) -> ModelEntry {
    let file = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    ModelEntry {
        id: format!("local:{}", path.display()),
        kind: "local".into(),
        name: file.trim_end_matches(".gguf").to_string(),
        params_b: size as f32 / 1e9 * 1.7,
        url: String::new(),
        file,
        size,
        sha256: String::new(),
        context: 8192,
        tools: false,
        system_suffix: String::new(),
        about: format!("Своя модель: {}", path.display()),
        extra: vec![],
        local_path: Some(path.to_path_buf()),
    }
}

/// Opens the models folder in the file manager.
pub fn open_models_dir() -> anyhow::Result<()> {
    let dir = crate::models_dir();
    std::fs::create_dir_all(&dir)?;
    open::that_detached(dir)?;
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entries_are_complete_and_unique() {
        let models = load();
        let mut ids = std::collections::HashSet::new();
        for m in &models {
            assert!(ids.insert(m.id.clone()), "duplicate id {}", m.id);
            assert!(
                ["chat", "code", "voice"].contains(&m.kind.as_str()),
                "{}: kind {}",
                m.id,
                m.kind
            );
            assert!(
                m.url.starts_with("https://huggingface.co/"),
                "{}: url",
                m.id
            );
            assert!(
                m.url.ends_with(&m.file),
                "{}: url does not end with the file name",
                m.id
            );
            assert!(m.size > 0, "{}: size", m.id);
            let hex = |h: &str| h.len() == 64 && h.chars().all(|c| c.is_ascii_hexdigit());
            assert!(hex(&m.sha256), "{}: sha256", m.id);
            assert!(
                m.extra
                    .iter()
                    .all(|e| e.size > 0 && e.url.ends_with(&e.file)),
                "{}: extra",
                m.id
            );
        }
        assert!(models.len() >= 30);
    }

    #[test]
    fn a_stray_gguf_in_the_models_folder_becomes_a_local_model() {
        let dir = std::env::temp_dir().join(format!("homellm-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("my-model.gguf"), b"GGUF").unwrap();
        let entry = local_entry(&dir.join("my-model.gguf"));
        assert_eq!(entry.kind, "local");
        assert_eq!(entry.name, "my-model");
        assert!(entry.is_downloaded() && entry.is_llm());
        assert_eq!(entry.path(), dir.join("my-model.gguf"));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn recommends_by_parameters_not_file_size() {
        let hw = Hardware {
            os: String::new(),
            cpu_threads: 8,
            ram_bytes: 32 << 30,
            gpu_name: None,
            vram_bytes: Some(10 << 30),
        };
        assert_eq!(recommend(&hw).unwrap().id, "gemma4-12b");
        let cpu_only = Hardware {
            vram_bytes: None,
            ram_bytes: 8 << 30,
            ..hw
        };
        assert!(recommend(&cpu_only).unwrap().size < 5 << 30);
    }
}
