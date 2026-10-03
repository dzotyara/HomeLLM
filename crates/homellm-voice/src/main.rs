//! HomeLLM's voice: speech to text (whisper.cpp) and text to speech (Piper). A separate
//! program because whisper.cpp and llama.cpp each bring their own ggml, and the two do not
//! link into one binary. The app runs it per phrase:
//!
//! - `homellm-voice transcribe <whisper-model.bin> <audio.wav> [language]` prints the text;
//! - `homellm-voice speak <voice.onnx>` reads the text from stdin and plays it.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result, anyhow, bail};

fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["transcribe", model, wav] => transcribe(model, wav, "ru"),
        ["transcribe", model, wav, language] => transcribe(model, wav, language),
        ["speak", voice] => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            speak(voice, &text)
        }
        // Speech into a file instead of the speakers: for checks.
        ["synth", voice, out] => {
            let mut text = String::new();
            std::io::stdin().read_to_string(&mut text)?;
            synth(voice, &text, out)
        }
        _ => bail!(
            "usage: homellm-voice transcribe <model.bin> <audio.wav> [language] | speak <voice.onnx>"
        ),
    }
}

fn transcribe(model: &str, wav: &str, language: &str) -> Result<()> {
    let samples = read_wav(Path::new(wav))?;
    let mut ctx_params = whisper_rs::WhisperContextParameters::default();
    ctx_params.flash_attn(true);
    let ctx = whisper_rs::WhisperContext::new_with_params(model, ctx_params)
        .with_context(|| format!("failed to load {model}"))?;
    let mut state = ctx.create_state()?;
    let mut params =
        whisper_rs::FullParams::new(whisper_rs::SamplingStrategy::Greedy { best_of: 1 });
    params.set_language(Some(language));
    params.set_n_threads(threads());
    params.set_no_context(true);
    params.set_suppress_blank(true);
    params.set_print_progress(false);
    params.set_print_realtime(false);
    params.set_print_timestamps(false);
    params.set_no_timestamps(true);
    // The encoder always works on a 30-second window (1500 frames); a short phrase needs only
    // its own length plus a margin: 15 s -> 2.4 s for a 5-second phrase with whisper-small.
    params.set_audio_ctx(audio_ctx(samples.len()));
    state.full(params, &samples)?;
    if std::env::var_os("HOMELLM_VOICE_DEBUG").is_some() {
        ctx.print_timings();
    }
    let text: Vec<String> = state
        .as_iter()
        .filter_map(|s| s.to_str_lossy().ok().map(|t| t.trim().to_string()))
        .filter(|t| !t.is_empty())
        .collect();
    println!("{}", text.join(" "));
    Ok(())
}

/// Encoder frames for this many 16 kHz samples: 50 per second, a second of margin, at least
/// 3 s (shorter windows make whisper hallucinate), at most the full 1500.
fn audio_ctx(samples: usize) -> i32 {
    let seconds = samples as f32 / 16_000.0 + 1.0;
    ((seconds.max(3.0) * 50.0) as i32).min(1500)
}

/// Half the logical cores, at most 8: whisper gets slower with every hyper-thread on top.
fn threads() -> i32 {
    let logical = std::thread::available_parallelism().map_or(4, |n| n.get());
    (logical / 2).clamp(1, 8) as i32
}

/// 16-bit PCM, any rate and channel count (the app records 16 kHz mono), as whisper's 16 kHz mono.
fn read_wav(path: &Path) -> Result<Vec<f32>> {
    let bytes =
        std::fs::read(path).with_context(|| format!("failed to read {}", path.display()))?;
    if bytes.len() < 12 || &bytes[..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        bail!("not a WAV file");
    }
    // Walk the chunks to «data»: some writers put more before it.
    let (mut rate, mut channels) = (16_000u32, 1usize);
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32::from_le_bytes(bytes[at + 4..at + 8].try_into()?) as usize;
        let body = at + 8;
        if id == b"fmt " && body + 8 <= bytes.len() {
            channels = u16::from_le_bytes([bytes[body + 2], bytes[body + 3]]).max(1) as usize;
            rate = u32::from_le_bytes(bytes[body + 4..body + 8].try_into()?);
        }
        if id == b"data" {
            let end = (body + len).min(bytes.len());
            let mono: Vec<f32> = bytes[body..end]
                .chunks_exact(2 * channels)
                .map(|frame| {
                    frame
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|c| i16::from_le_bytes(*c) as f32 / 32768.0)
                        .sum::<f32>()
                        / channels as f32
                })
                .collect();
            return Ok(resample(&mono, rate, 16_000));
        }
        at = body + len + (len & 1);
    }
    Err(anyhow!("no audio in the WAV file"))
}

fn synth(voice: &str, text: &str, out: &str) -> Result<()> {
    let mut piper = load_voice(voice)?;
    let mut all = vec![];
    let mut rate = 22_050;
    for sentence in sentences(text) {
        let (samples, r) = piper
            .create(&sentence, false, None, None, None, None)
            .map_err(|e| anyhow!("speech synthesis failed: {e}"))?;
        all.extend(samples);
        rate = r;
    }
    std::fs::write(out, wav16(&all, rate))?;
    Ok(())
}

fn wav16(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    for part in [
        &b"RIFF"[..],
        &(36 + data_len).to_le_bytes(),
        b"WAVEfmt ",
        &16u32.to_le_bytes(),
        &1u16.to_le_bytes(),
        &1u16.to_le_bytes(),
        &rate.to_le_bytes(),
        &(rate * 2).to_le_bytes(),
        &2u16.to_le_bytes(),
        &16u16.to_le_bytes(),
        b"data",
        &data_len.to_le_bytes(),
    ] {
        out.extend(part);
    }
    for s in samples {
        out.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}

fn load_voice(voice: &str) -> Result<piper_rs::Piper> {
    if std::env::var_os("PIPER_ESPEAKNG_DATA_DIRECTORY").is_none()
        && let Some(dir) = espeak_data()
    {
        // SAFETY: nothing else runs yet: no other threads read the environment.
        unsafe { std::env::set_var("PIPER_ESPEAKNG_DATA_DIRECTORY", dir) };
    }
    let config = format!("{voice}.json");
    piper_rs::Piper::new(Path::new(voice), Path::new(&config))
        .map_err(|e| anyhow!("failed to load the voice {voice}: {e}"))
}

fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let len = (samples.len() as u64 * to as u64 / from as u64) as usize;
    let last = samples.len() - 1;
    (0..len)
        .map(|i| {
            let pos = i as f64 * from as f64 / to as f64;
            let at = (pos as usize).min(last);
            let frac = (pos - at as f64) as f32;
            samples[at] * (1.0 - frac) + samples[(at + 1).min(last)] * frac
        })
        .collect()
}

fn speak(voice: &str, text: &str) -> Result<()> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(());
    }
    let mut piper = load_voice(voice)?;
    let stream = rodio::OutputStreamBuilder::open_default_stream().context("no audio output")?;
    let sink = rodio::Sink::connect_new(stream.mixer());
    // Sentence by sentence: the first one plays while the next is synthesised.
    for sentence in sentences(text) {
        let (samples, rate) = piper
            .create(&sentence, false, None, None, None, None)
            .map_err(|e| anyhow!("speech synthesis failed: {e}"))?;
        sink.append(rodio::buffer::SamplesBuffer::new(1, rate, samples));
    }
    sink.sleep_until_end();
    Ok(())
}

/// The folder that holds `espeak-ng-data` (pronunciation rules): next to this program in an
/// installed app, else in cargo's build folder when run from `target/`.
fn espeak_data() -> Option<std::path::PathBuf> {
    let exe_dir = std::env::current_exe().ok()?.parent()?.to_path_buf();
    if exe_dir.join("espeak-ng-data").is_dir() {
        return Some(exe_dir);
    }
    std::fs::read_dir(exe_dir.join("build"))
        .ok()?
        .flatten()
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with("espeak-rs-sys-")
        })
        .map(|e| e.path().join("out").join("share"))
        .filter(|share| share.join("espeak-ng-data").join("phontab").is_file())
        .max_by_key(|share| share.metadata().and_then(|m| m.modified()).ok())
}

fn sentences(text: &str) -> Vec<String> {
    let mut out = vec![];
    let mut current = String::new();
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        current.push(c);
        // A sentence ends at punctuation followed by a space or the end: «...» and «3.5» stay whole.
        let ends =
            matches!(c, '.' | '!' | '?' | '\n') && chars.peek().is_none_or(|n| n.is_whitespace());
        if ends && !current.trim().is_empty() {
            out.push(std::mem::take(&mut current).trim().to_string());
        }
    }
    if !current.trim().is_empty() {
        out.push(current.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_encoder_window_follows_the_phrase() {
        assert_eq!(audio_ctx(16_000), 150, "short phrases get 3 s");
        assert_eq!(audio_ctx(5 * 16_000), 300, "5 s + 1 s margin");
        assert_eq!(audio_ctx(60 * 16_000), 1500, "never above the full window");
    }

    #[test]
    fn text_is_split_into_sentences() {
        assert_eq!(
            sentences("Привет! Как дела? Всё хорошо"),
            ["Привет!", "Как дела?", "Всё хорошо"]
        );
        assert_eq!(sentences("..."), ["..."]);
        assert_eq!(
            sentences("Версия 3.5 вышла. Ура"),
            ["Версия 3.5 вышла.", "Ура"]
        );
    }

    #[test]
    fn wav_samples_are_read_after_other_chunks() {
        let mut wav = b"RIFF\0\0\0\0WAVE".to_vec();
        wav.extend(b"LIST\x02\0\0\0ab");
        wav.extend(b"data\x04\0\0\0");
        wav.extend(16384i16.to_le_bytes());
        wav.extend((-32768i16).to_le_bytes());
        let path = std::env::temp_dir().join(format!("homellm-voice-{}.wav", std::process::id()));
        std::fs::write(&path, wav).unwrap();
        assert_eq!(read_wav(&path).unwrap(), [0.5, -1.0]);
        std::fs::remove_file(path).unwrap();
    }
}
