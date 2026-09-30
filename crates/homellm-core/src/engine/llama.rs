//! Built-in inference: llama.cpp through `llama-cpp-2`, GGUF models.

use std::num::NonZeroU32;
use std::path::Path;
use std::sync::{Arc, OnceLock};

use anyhow::{Context, Result};
use async_trait::async_trait;
use llama_cpp_2::context::params::LlamaContextParams;
use llama_cpp_2::llama_backend::LlamaBackend;
use llama_cpp_2::llama_batch::LlamaBatch;
use llama_cpp_2::model::params::LlamaModelParams;
use llama_cpp_2::model::{AddBos, LlamaChatMessage, LlamaModel};
use llama_cpp_2::mtmd::{
    MtmdBitmap, MtmdContext, MtmdContextParams, MtmdInputText, mtmd_default_marker,
};
use llama_cpp_2::sampling::LlamaSampler;

use super::{Engine, Message, TokenSink, keep_last_images};

const MAX_NEW_TOKENS: usize = 1024;
const CHUNK: usize = 512;
/// End-of-turn markers, in case the model does not report them as the end of generation.
const END_MARKERS: &[&str] = &[
    "<end_of_turn>",
    "<|im_end|>",
    "<|eot_id|>",
    "<|end|>",
    "<|endoftext|>",
];

/// Generation speed of the last answer, tokens per second.
static LAST_SPEED: std::sync::Mutex<Option<f32>> = std::sync::Mutex::new(None);

pub fn last_speed() -> Option<f32> {
    *LAST_SPEED.lock().unwrap()
}

fn backend() -> &'static LlamaBackend {
    static BACKEND: OnceLock<LlamaBackend> = OnceLock::new();
    BACKEND.get_or_init(|| LlamaBackend::init().expect("llama.cpp backend"))
}

/// Video memory for llama.cpp's compute buffers, on top of the weights and the KV cache.
const GPU_COMPUTE: u64 = 900_000_000;

/// How many layers go to the GPU, and the context size. Planned from free video memory
/// up front: a failed allocation on Vulkan does not give its memory back, so trial and
/// error only makes things worse. Full context if it fits, else half; then fewer layers.
/// `reserved`: video memory taken by something else first (the vision projector).
fn plan_gpu(path: &Path, n_ctx: u32, reserved: u64) -> (u32, u32) {
    let size = std::fs::metadata(path).map_or(0, |m| m.len());
    let Some(free) = gpu_free() else {
        return (999, n_ctx);
    };
    let free = free.saturating_sub(reserved);
    // Shapes from the metadata alone (no weights loaded).
    let params = LlamaModelParams::default().with_vocab_only(true);
    let meta = LlamaModel::load_from_file(backend(), path, &params).ok();
    let num = |key: &str| -> Option<u64> {
        let m = meta.as_ref()?;
        let arch = m.meta_val_str("general.architecture").ok()?;
        m.meta_val_str(&format!("{arch}.{key}")).ok()?.parse().ok()
    };
    let blocks = num("block_count").unwrap_or(40);
    let heads = num("attention.head_count").unwrap_or(32);
    let kv_heads = num("attention.head_count_kv").unwrap_or(heads);
    let head_dim = num("attention.key_length")
        .or_else(|| Some(num("embedding_length")? / heads))
        .unwrap_or(128);
    // K and V, f16, per token of context.
    let kv_per_token = 2 * blocks * kv_heads * head_dim * 2;
    let need = |ctx: u32, layers: u64| {
        size * layers / blocks + kv_per_token * ctx as u64 * layers / blocks + GPU_COMPUTE
    };
    for ctx in [n_ctx, (n_ctx / 2).max(2048)] {
        if need(ctx, blocks) <= free {
            return (999, ctx);
        }
    }
    let ctx = (n_ctx / 2).max(2048);
    let per_layer = (size + kv_per_token * ctx as u64) / blocks;
    let layers = free.saturating_sub(GPU_COMPUTE) / per_layer.max(1);
    (layers.min(blocks) as u32, ctx)
}

/// Free memory of the biggest GPU llama.cpp can use.
fn gpu_free() -> Option<u64> {
    backend();
    llama_cpp_2::list_llama_ggml_backend_devices()
        .into_iter()
        .filter(|d| !d.backend.eq_ignore_ascii_case("CPU") && d.memory_total > 0)
        .max_by_key(|d| d.memory_total)
        .map(|d| d.memory_free as u64)
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
    /// The vision projector: set when the model's mmproj file is downloaded.
    vision: Option<Arc<MtmdContext>>,
    n_ctx: u32,
}

impl LlamaEngine {
    /// Loads a GGUF file, offloading every layer to the GPU when the build has one.
    /// Loads a catalog model, on the GPU when it fits there. Returns whether it went to the GPU.
    pub fn load_entry(model: &crate::catalog::ModelEntry) -> Result<(Self, bool)> {
        let hw = crate::hardware::detect();
        let on_gpu = crate::catalog::fit(model, &hw) == crate::catalog::Fit::Gpu;
        let vision = model.vision_path();
        Ok((
            Self::load(&model.path(), model.context, on_gpu, vision.as_deref())?,
            on_gpu,
        ))
    }

    /// `on_gpu`: offload every layer to the video card; pass `catalog::fit(..) == Fit::Gpu`,
    /// since a model that does not fit in video memory fails to load there.
    /// `mmproj`: the vision projector, so the model sees pictures.
    pub fn load(path: &Path, n_ctx: u32, on_gpu: bool, mmproj: Option<&Path>) -> Result<Self> {
        // Plan the split from free video memory up front: a failed context allocation on
        // Vulkan does not give its memory back, so trial and error only makes things worse.
        let (layers, n_ctx) = if on_gpu {
            let reserved = mmproj
                .and_then(|p| std::fs::metadata(p).ok())
                .map_or(0, |m| m.len());
            plan_gpu(path, n_ctx, reserved)
        } else {
            (0, n_ctx)
        };
        let params = LlamaModelParams::default().with_n_gpu_layers(layers);
        let model = LlamaModel::load_from_file(backend(), path, &params)
            .with_context(|| format!("failed to load {}", path.display()))?;
        let ctx_size = n_ctx.min(model.n_ctx_train());
        let vision = match mmproj {
            Some(mmproj) => {
                let params = MtmdContextParams {
                    use_gpu: on_gpu,
                    print_timings: false,
                    n_threads: threads(),
                    ..MtmdContextParams::default()
                };
                let vision =
                    MtmdContext::init_from_file(&mmproj.to_string_lossy(), &model, &params)
                        .with_context(|| format!("failed to load {}", mmproj.display()))?;
                vision.support_vision().then(|| Arc::new(vision))
            }
            None => None,
        };
        Ok(Self {
            vision,
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            n_ctx: ctx_size,
            model: Arc::new(model),
        })
    }
}

#[async_trait]
impl Engine for LlamaEngine {
    fn name(&self) -> String {
        self.name.clone()
    }

    fn sees_images(&self) -> bool {
        self.vision.is_some()
    }

    async fn complete(&self, messages: &[Message], tokens: TokenSink) -> Result<String> {
        let model = self.model.clone();
        let vision = self.vision.clone();
        let messages = messages.to_vec();
        let n_ctx = self.n_ctx;
        tokio::task::spawn_blocking(move || {
            generate(&model, vision.as_deref(), &messages, n_ctx, tokens)
        })
        .await?
    }
}

fn threads() -> i32 {
    std::thread::available_parallelism().map_or(4, |n| n.get()) as i32
}

/// A media marker per picture in front of the text; without a projector, a note instead.
fn with_markers(messages: &[Message], sees: bool) -> (Vec<Message>, Vec<String>) {
    let mut images = vec![];
    let messages = keep_last_images(messages)
        .into_iter()
        .map(|mut m| {
            if m.images.is_empty() {
                return m;
            }
            if sees {
                let markers = vec![mtmd_default_marker(); m.images.len()].join("\n");
                m.content = format!("{markers}\n{}", m.content);
                images.append(&mut m.images);
            } else {
                m.content = format!(
                    "{}\n[приложена картинка, но эта модель не видит изображений]",
                    m.content
                );
                m.images.clear();
            }
            m
        })
        .collect();
    (messages, images)
}

fn generate(
    model: &LlamaModel,
    vision: Option<&MtmdContext>,
    messages: &[Message],
    n_ctx: u32,
    tokens: TokenSink,
) -> Result<String> {
    let (messages, images) = with_markers(messages, vision.is_some());
    let messages = &messages[..];
    // The chat template embedded in the GGUF (ChatML, Llama 3, Gemma...). llama.cpp only
    // knows the templates it recognises (Gemma 4's gave "ffi error -1"): then build it by hand.
    let chat = messages
        .iter()
        .map(|m| LlamaChatMessage::new(m.role.clone(), m.content.clone()))
        .collect::<Result<Vec<_>, _>>()?;
    let prompt = match model
        .chat_template(None)
        .ok()
        .and_then(|t| model.apply_chat_template(&t, &chat, true).ok())
    {
        Some(prompt) => prompt,
        None => manual_prompt(
            &model
                .meta_val_str("general.architecture")
                .unwrap_or_default(),
            messages,
        ),
    };
    let params = LlamaContextParams::default()
        .with_n_ctx(NonZeroU32::new(n_ctx))
        .with_n_threads(threads())
        .with_n_batch(CHUNK as u32);
    let too_long =
        |n: usize| anyhow::anyhow!("conversation is too long for the context ({n} tokens)");
    let mut batch = LlamaBatch::new(CHUNK, 1);
    let (mut ctx, prompt_len) = match vision.filter(|_| !images.is_empty()) {
        // Pictures: the projector turns text and pictures into chunks and evaluates them.
        Some(vision) => {
            let bitmaps = images
                .iter()
                .map(|path| {
                    MtmdBitmap::from_file(vision, path, false)
                        .with_context(|| format!("не удалось открыть картинку {path}"))
                })
                .collect::<Result<Vec<_>>>()?;
            let refs: Vec<&MtmdBitmap> = bitmaps.iter().collect();
            let text = MtmdInputText {
                text: prompt,
                add_special: false,
                parse_special: true,
            };
            let chunks = vision.tokenize(text, &refs)?;
            if chunks.total_tokens() + MAX_NEW_TOKENS > n_ctx as usize {
                return Err(too_long(chunks.total_tokens()));
            }
            let ctx = new_context(model, params)?;
            let n_past = chunks.eval_chunks(vision, &ctx, 0, 0, CHUNK as i32, true)?;
            (ctx, n_past as usize)
        }
        None => {
            // The templates write BOS themselves where the model needs it.
            let prompt_tokens = model.str_to_token(&prompt, AddBos::Never)?;
            if prompt_tokens.len() + MAX_NEW_TOKENS > n_ctx as usize {
                return Err(too_long(prompt_tokens.len()));
            }
            let mut ctx = new_context(model, params)?;
            // Feed the prompt in chunks; logits only for its very last token.
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
            (ctx, prompt_tokens.len())
        }
    };

    let mut sampler = LlamaSampler::chain_simple([
        LlamaSampler::min_p(0.05, 1),
        LlamaSampler::temp(0.6),
        LlamaSampler::dist(rand_seed()),
    ]);
    let mut decoder = encoding_rs::UTF_8.new_decoder();
    let mut text = String::new();
    let mut pos = prompt_len as i32;
    let started = std::time::Instant::now();
    let mut generated = 0usize;
    for _ in 0..MAX_NEW_TOKENS {
        generated += 1;
        // After the projector the batch is still empty: -1 is the last evaluated token.
        let token = sampler.sample(&ctx, batch.n_tokens() - 1);
        sampler.accept(token);
        if model.is_eog_token(token) {
            break;
        }
        // Special tokens must be rendered: in Qwen3 `<tool_call>` and `<think>` are single special tokens.
        let piece = model.token_to_piece(token, &mut decoder, true, None)?;
        text.push_str(&piece);
        // Some GGUFs do not mark their end-of-turn token as the end: stop on it by text.
        if let Some(at) = END_MARKERS.iter().find_map(|m| text.find(m)) {
            text.truncate(at);
            break;
        }
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

fn new_context(
    model: &LlamaModel,
    params: LlamaContextParams,
) -> Result<llama_cpp_2::context::LlamaContext<'_>> {
    model.new_context(backend(), params).context(
        "не хватает видеопамяти: закройте игры и тяжёлые программы или выберите модель поменьше",
    )
}

/// A prompt for models whose template llama.cpp does not know: Gemma's turn format
/// (no system role: the rules open the first user turn) or ChatML for the rest.
fn manual_prompt(arch: &str, messages: &[Message]) -> String {
    if arch.starts_with("gemma") {
        let mut prompt = String::from("<bos>");
        let mut system = String::new();
        for m in messages {
            match m.role.as_str() {
                "system" => system = m.content.clone(),
                "assistant" => prompt.push_str(&format!(
                    "<start_of_turn>model
{}<end_of_turn>
",
                    m.content
                )),
                _ => {
                    let text = if system.is_empty() {
                        m.content.clone()
                    } else {
                        format!(
                            "{}

{}",
                            std::mem::take(&mut system),
                            m.content
                        )
                    };
                    prompt.push_str(&format!(
                        "<start_of_turn>user
{text}<end_of_turn>
"
                    ));
                }
            }
        }
        prompt
            + "<start_of_turn>model
"
    } else {
        let mut prompt: String = messages
            .iter()
            .map(|m| {
                format!(
                    "<|im_start|>{}
{}<|im_end|>
",
                    m.role, m.content
                )
            })
            .collect();
        prompt.push_str(
            "<|im_start|>assistant
",
        );
        prompt
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pictures_become_markers_or_a_note() {
        let msgs = [
            Message::with_images("user", "что тут?", vec!["a.png".into()]),
            Message::new("assistant", "кот"),
            Message::with_images("user", "а тут?", vec!["b.png".into(), "c.png".into()]),
        ];
        let (seen, images) = with_markers(&msgs, true);
        assert_eq!(images, ["b.png", "c.png"], "only the latest pictures");
        assert!(seen[0].content.contains("уже обсуждали"));
        assert_eq!(
            seen[2].content,
            format!("{0}\n{0}\nа тут?", mtmd_default_marker())
        );
        let (blind, images) = with_markers(&msgs, false);
        assert!(images.is_empty());
        assert!(blind[2].content.contains("не видит изображений"));
    }

    #[test]
    fn gemma_prompt_folds_the_system_rules_into_the_first_turn() {
        let msgs = [
            Message::new("system", "правила"),
            Message::new("user", "привет"),
            Message::new("assistant", "здравствуй"),
        ];
        let p = manual_prompt("gemma4", &msgs);
        assert_eq!(
            p,
            "<bos><start_of_turn>user
правила

привет<end_of_turn>
<start_of_turn>model
здравствуй<end_of_turn>
<start_of_turn>model
"
        );
        assert!(manual_prompt("qwen3", &msgs).ends_with(
            "<|im_start|>assistant
"
        ));
    }
}

fn rand_seed() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos())
}
