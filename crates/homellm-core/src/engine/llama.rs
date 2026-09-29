//! Built-in inference: llama.cpp through `llama-cpp-2`, GGUF models.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result, anyhow};
use async_trait::async_trait;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::sampling::LlamaSampler;

use super::{Engine, Message, TokenSink};

const MAX_NEW_TOKENS: usize = 1024;
const CHUNK: usize = 512;

/// Generation speed of the last answer, tokens per second.
static LAST_SPEED: std::sync::Mutex<Option<f32>> = std::sync::Mutex::new(None);

pub fn last_speed() -> Option<f32> {
    *LAST_SPEED.lock().unwrap()
}

fn backend() -> &'static LlamaBackend {
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    BACKEND.get_or_init(|| LlamaBackend::init().expect("llama.cpp backend"))
}

/// The GPU with the most memory among llama.cpp's devices: (description, bytes).
pub fn best_gpu() -> Option<(String, u64)> {
    backend();
    llama_cpp_2::list_llama_ggml_backend_devices()
        .into_iter()
        .filter(|d| !d.backend.eq_ignore_ascii_case("CPU") && d.memory_total > 0)
        .max_by_key(|d| d.memory_total)
        .map(|d| {
            (
                format!("{} ({})", d.description, d.backend),
                d.memory_total as u64,
            )
        })
}

pub struct LlamaEngine {
    name: String,
    model: Arc<LlamaModel>,
    n_ctx: u32,
}

impl LlamaEngine {
    /// Loads a GGUF file, offloading every layer to the GPU when the build has one.
    /// Loads a catalog model, on the GPU when it fits there. Returns whether it went to the GPU.
    pub fn load_entry(model: &crate::catalog::ModelEntry) -> Result<(Self, bool)> {
        let hw = crate::hardware::detect();
        let on_gpu = crate::catalog::fit(model, &hw) == crate::catalog::Fit::Gpu;
        Ok((Self::load(&model.path(), model.context, on_gpu)?, on_gpu))
    }

    /// `on_gpu`: offload every layer to the video card; pass `catalog::fit(..) == Fit::Gpu`,
    /// since a model that does not fit in video memory fails to load there.
    pub fn load(path: &Path, n_ctx: u32, on_gpu: bool) -> Result<Self> {
        let layers = if on_gpu { 999 } else { 0 };
        let params = LlamaModelParams::default().with_n_gpu_layers(layers);
        let model = LlamaModel::load_from_file(backend(), path, &params)
            .with_context(|| format!("failed to load {}", path.display()))?;
        Ok(Self {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            n_ctx: n_ctx.min(model.n_ctx_train()),
            model: Arc::new(model),
        })
    }
}

#[async_trait]
impl Engine for LlamaEngine {
    fn name(&self) -> String {
        self.name.clone()
    }

    async fn complete(&self, messages: &[Message], tokens: TokenSink) -> Result<String> {
        let model = self.model.clone();
        let messages = messages.to_vec();
        let n_ctx = self.n_ctx;
        tokio::task::spawn_blocking(move || generate(&model, &messages, n_ctx, tokens)).await?
    }
}

fn generate(
    model: &LlamaModel,
    messages: &[Message],
    n_ctx: u32,
    tokens: TokenSink,
) -> Result<String> {
    // The chat template embedded in the GGUF (ChatML, Llama 3, Gemma...).
    let template = model
        .chat_template(None)
        .map_err(|e| anyhow!("no chat template: {e}"))?;
    let chat = messages
        .iter()
        .map(|m| LlamaChatMessage::new(m.role.clone(), m.content.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let prompt = model.apply_chat_template(&template, &chat, true)?;
    // The templates write BOS themselves where the model needs it.
    let prompt_tokens = model.str_to_token(&prompt, AddBos::Never)?;
    if prompt_tokens.len() + MAX_NEW_TOKENS > n_ctx as usize {
        anyhow::bail!(
            "conversation is too long for the context ({} tokens)",
            prompt_tokens.len()
        );
    }

    let threads = std::thread::available_parallelism().map_or(4, |n| n.get()) as i32;
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_threads(threads);
    let mut ctx = model.new_context(backend(), params)?;

    // Feed the prompt in chunks; logits only for its very last token.
    let mut batch = LlamaBatch::new(CHUNK, 1);
    let last = prompt_tokens.len() - 1;
    for (start, chunk) in prompt_tokens
        .chunks(CHUNK)
        .enumerate()
        .map(|(i, c)| (i * CHUNK, c))
    {
        batch.clear();
        for (i, &token) in chunk.iter().enumerate() {
            let pos = start + i;
            batch.add(token, pos as i32, &[0], pos == last)?;
        }
        ctx.decode(&mut batch)?;
    }

    let mut sampler = LlamaSampler::chain_simple([
        LlamaSampler::min_p(0.05, 1),
        LlamaSampler::temp(0.6),
        LlamaSampler::dist(rand_seed()),
    ]);
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut text = String::new();
    let mut pos = prompt_tokens.len() as i32;
    let started = std::time::Instant::now();
    let mut generated = 0usize;
    for _ in 0..MAX_NEW_TOKENS {
        generated += 1;
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }
        // Special tokens must be rendered: in Qwen3 `<tool_call>` and `<think>` are single special tokens.
        let piece = model.token_to_piece(token, &mut decoder, true, None)?;
        text.push_str(&piece);
        if let Some(tx) = &tokens {
            let _ = tx.send(piece);
        }
        // The agent only needs the call itself: stop right after it.
        if text.contains("</tool_call>") {
            break;
        }
        batch.clear();
        batch.add(token, pos, &[0], true)?;
        pos += 1;
        ctx.decode(&mut batch)?;
    }
    // Short replies (a tool call) say little about speed: count from 8 tokens up.
    let secs = started.elapsed().as_secs_f32();
    if generated >= 8 && secs > 0.0 {
        *LAST_SPEED.lock().unwrap() = Some(generated as f32 / secs);
    }
    Ok(text)
}

fn rand_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos())
}
