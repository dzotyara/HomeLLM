//! Voice: the microphone is recorded here, speech is recognised and spoken by the
//! `homellm-voice` program next to the app (whisper.cpp and llama.cpp cannot share one binary).
//! Models come from the catalog: whisper-* for speech, piper-* for the voice.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::OnceLock;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use anyhow::{Context, Result, anyhow, bail};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use crate::catalog::{self, ModelEntry};

/// Whisper wants 16 kHz mono.
const RATE: u32 = 16_000;
/// Shorter than this is a slip of the finger, not speech.
const MIN_SECONDS: f32 = 0.4;
/// The first speech model and voice to download: good in Russian, small enough.
pub const DEFAULT_STT: &str = "whisper-small";
pub const DEFAULT_TTS: &str = "piper-ru-irina";

/// Records the default microphone until `stop`. The stream lives on its own thread:
/// cpal's streams may not move between threads.
pub struct Recorder {
    stop: mpsc::Sender<()>,
    thread: JoinHandle<Result<(Vec<f32>, u32)>>,
}

impl Recorder {
    pub fn start() -> Result<Self> {
        let (stop, stopped) = mpsc::channel();
        let (ready, started) = mpsc::channel();
        let thread = std::thread::spawn(move || record(stopped, ready));
        match started.recv() {
            Ok(Ok(())) => Ok(Self { stop, thread }),
            Ok(Err(e)) => Err(e),
            Err(_) => bail!("microphone thread failed"),
        }
    }

    /// Stops and saves the recording as a 16 kHz WAV.
    pub fn stop(self) -> Result<PathBuf> {
        let _ = self.stop.send(());
        let (samples, rate) = self
            .thread
            .join()
            .map_err(|_| anyhow!("microphone thread failed"))??;
        let samples = resample(&samples, rate, RATE);
        if (samples.len() as f32) < MIN_SECONDS * RATE as f32 {
            bail!("запись слишком короткая: говорите, удерживая клавишу");
        }
        let dir = crate::data_dir().join("voice");
        std::fs::create_dir_all(&dir)?;
        let path = dir.join("last.wav");
        std::fs::write(&path, wav(&samples, RATE))?;
        Ok(path)
    }
}

fn record(stopped: mpsc::Receiver<()>, ready: mpsc::Sender<Result<()>>) -> Result<(Vec<f32>, u32)> {
    let opened = open_stream();
    let (stream, samples, rate) = match opened {
        Ok(parts) => {
            let _ = ready.send(Ok(()));
            parts
        }
        Err(e) => {
            let _ = ready.send(Err(anyhow!("{e:#}")));
            return Err(e);
        }
    };
    let _ = stopped.recv();
    drop(stream);
    let samples = std::mem::take(&mut *samples.lock().unwrap());
    Ok((samples, rate))
}

/// Mono samples at the device's rate.
type Shared = Arc<Mutex<Vec<f32>>>;

fn open_stream() -> Result<(cpal::Stream, Shared, u32)> {
    let device = cpal::default_host()
        .default_input_device()
        .ok_or_else(|| anyhow!("микрофон не найден"))?;
    let config = device
        .default_input_config()
        .context("микрофон недоступен")?;
    let rate = config.sample_rate().0;
    let channels = config.channels() as usize;
    let samples: Shared = Arc::default();
    let stream = match config.sample_format() {
        cpal::SampleFormat::F32 => {
            input::<f32>(&device, &config.into(), channels, samples.clone())?
        }
        cpal::SampleFormat::I16 => {
            input::<i16>(&device, &config.into(), channels, samples.clone())?
        }
        cpal::SampleFormat::U16 => {
            input::<u16>(&device, &config.into(), channels, samples.clone())?
        }
        other => bail!("unsupported microphone format {other}"),
    };
    stream.play().context("микрофон не включился")?;
    Ok((stream, samples, rate))
}

fn input<T>(
    device: &cpal::Device,
    config: &cpal::StreamConfig,
    channels: usize,
    samples: Shared,
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    Ok(device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut out = samples.lock().unwrap();
            // Channels averaged into one.
            out.extend(data.chunks(channels.max(1)).map(|frame| {
                frame
                    .iter()
                    .map(|&s| <f32 as cpal::FromSample<T>>::from_sample_(s))
                    .sum::<f32>()
                    / frame.len() as f32
            }));
        },
        |e| eprintln!("microphone: {e}"),
        None,
    )?)
}

/// Linear resampling: enough for speech recognition.
fn resample(samples: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || samples.is_empty() {
        return samples.to_vec();
    }
    let len = (samples.len() as u64 * to as u64 / from as u64) as usize;
    (0..len)
        .map(|i| {
            let pos = i as f64 * from as f64 / to as f64;
            let at = pos as usize;
            let next = samples
                .get(at + 1)
                .copied()
                .unwrap_or(samples[at.min(samples.len() - 1)]);
            let frac = (pos - at as f64) as f32;
            samples[at.min(samples.len() - 1)] * (1.0 - frac) + next * frac
        })
        .collect()
}

/// 16-bit mono PCM WAV.
fn wav(samples: &[f32], rate: u32) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend(b"RIFF");
    out.extend((36 + data_len).to_le_bytes());
    out.extend(b"WAVEfmt ");
    out.extend(16u32.to_le_bytes());
    out.extend(1u16.to_le_bytes()); // PCM
    out.extend(1u16.to_le_bytes()); // mono
    out.extend(rate.to_le_bytes());
    out.extend((rate * 2).to_le_bytes());
    out.extend(2u16.to_le_bytes());
    out.extend(16u16.to_le_bytes());
    out.extend(b"data");
    out.extend(data_len.to_le_bytes());
    for s in samples {
        out.extend(((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    out
}

/// The installed app's resource folder: holds `espeak-ng-data` when it is not next to the
/// voice program (Linux and macOS packages keep resources apart from programs).
static RESOURCES: OnceLock<PathBuf> = OnceLock::new();

pub fn set_resource_dir(dir: PathBuf) {
    let _ = RESOURCES.set(dir);
}

/// The voice program: next to the app.
fn helper() -> Result<PathBuf> {
    let name = format!("homellm-voice{}", std::env::consts::EXE_SUFFIX);
    let path = std::env::current_exe()?
        .parent()
        .ok_or_else(|| anyhow!("no app folder"))?
        .join(name);
    if !path.is_file() {
        bail!(
            "нет программы голоса {}: переустановите HomeLLM",
            path.display()
        );
    }
    Ok(path)
}

/// The downloaded model of this kind: the one in the settings, else the default, else any.
fn pick(prefix: &str, chosen: &str, default: &str) -> Option<ModelEntry> {
    let models: Vec<ModelEntry> = catalog::all()
        .into_iter()
        .filter(|m| m.kind == "voice" && m.id.starts_with(prefix) && m.is_downloaded())
        .collect();
    [chosen, default]
        .iter()
        .find_map(|id| models.iter().find(|m| m.id == *id).cloned())
        .or_else(|| models.into_iter().next())
}

pub fn speech_model() -> Option<ModelEntry> {
    pick("whisper", &crate::settings::get().stt_model, DEFAULT_STT)
}

pub fn voice_model() -> Option<ModelEntry> {
    pick("piper", &crate::settings::get().tts_voice, DEFAULT_TTS)
}

/// Models the voice still needs: ids to download.
pub fn missing() -> Vec<&'static str> {
    let mut ids = vec![];
    if speech_model().is_none() {
        ids.push(DEFAULT_STT);
    }
    if voice_model().is_none() {
        ids.push(DEFAULT_TTS);
    }
    ids
}

/// Speech to text.
pub fn transcribe(wav: &std::path::Path) -> Result<String> {
    let model = speech_model().ok_or_else(|| anyhow!("модель распознавания речи не скачана"))?;
    let output = Command::new(helper()?)
        .arg("transcribe")
        .arg(model.path())
        .arg(wav)
        .arg("ru")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .no_window()
        .output()?;
    if !output.status.success() {
        bail!("не удалось распознать речь");
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if is_silence(&text) {
        bail!("ничего не расслышал");
    }
    Ok(text)
}

/// Whisper's usual output for silence or noise.
fn is_silence(text: &str) -> bool {
    let t = text
        .trim_matches(|c: char| !c.is_alphanumeric())
        .to_lowercase();
    t.is_empty()
        || ["продолжение следует", "субтитры", "спасибо за просмотр"]
            .iter()
            .any(|s| t.starts_with(s))
}

/// Starts speaking; kill the child to stop.
pub fn speak(text: &str) -> Result<Child> {
    let voice = voice_model().ok_or_else(|| anyhow!("голос не скачан"))?;
    let mut command = Command::new(helper()?);
    if let Some(dir) = RESOURCES
        .get()
        .filter(|d| d.join("espeak-ng-data").is_dir())
    {
        command.env("PIPER_ESPEAKNG_DATA_DIRECTORY", dir);
    }
    let mut child = command
        .arg("speak")
        .arg(voice.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .no_window()
        .spawn()?;
    use std::io::Write;
    child
        .stdin
        .take()
        .ok_or_else(|| anyhow!("no stdin"))?
        .write_all(speakable(text).as_bytes())?;
    Ok(child)
}

trait NoWindow {
    fn no_window(&mut self) -> &mut Self;
}

impl NoWindow for Command {
    /// No console window flashing up on Windows.
    fn no_window(&mut self) -> &mut Self {
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            self.creation_flags(0x0800_0000);
        }
        self
    }
}

/// What is worth saying aloud: no markdown, code or links.
pub fn speakable(text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            in_code = !in_code;
            if in_code {
                out.push_str("Код — в окне.\n");
            }
            continue;
        }
        if in_code {
            continue;
        }
        let line: String = line
            .split_whitespace()
            .map(|w| {
                if w.starts_with("http://") || w.starts_with("https://") {
                    "ссылка"
                } else {
                    w
                }
            })
            .collect::<Vec<_>>()
            .join(" ");
        let line = line
            .trim_start_matches(['#', '>', '-', '*', ' '])
            .replace(['*', '`', '_', '#', '|'], "");
        if !line.trim().is_empty() {
            out.push_str(line.trim());
            out.push('\n');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resampling_keeps_the_duration() {
        let one_second = vec![0.5; 48_000];
        let out = resample(&one_second, 48_000, 16_000);
        assert_eq!(out.len(), 16_000);
        assert!(out.iter().all(|s| (*s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn wav_header_is_pcm_16khz_mono() {
        let w = wav(&[0.0, 1.0, -1.0], RATE);
        assert_eq!(&w[..4], b"RIFF");
        assert_eq!(u32::from_le_bytes(w[24..28].try_into().unwrap()), RATE);
        assert_eq!(w.len(), 44 + 6);
        assert_eq!(i16::from_le_bytes([w[46], w[47]]), 32767);
    }

    #[test]
    fn markdown_code_and_links_are_not_read_aloud() {
        let text = "## Итог\n**Готово**: открыл https://example.com\n```\nlet x = 1;\n```\n- пункт";
        assert_eq!(
            speakable(text),
            "Итог\nГотово: открыл ссылка\nКод — в окне.\nпункт\n"
        );
    }

    #[test]
    fn whisper_hallucinations_on_silence_are_dropped() {
        assert!(is_silence(" Продолжение следует... "));
        assert!(is_silence("..."));
        assert!(!is_silence("включи музыку"));
    }
}
