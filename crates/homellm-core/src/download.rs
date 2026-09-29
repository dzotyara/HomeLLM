//! Model downloads: resumable (HTTP Range into a `.part` file) and checked by sha256.

use std::io::{Read, Seek, Write};

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};

use crate::catalog::ModelEntry;

/// Downloads `model` into `models_dir()`; `progress(done, total)` is called as bytes arrive.
pub async fn download(model: &ModelEntry, mut progress: impl FnMut(u64, u64)) -> Result<()> {
    let dest = model.path();
    if model.is_downloaded() {
        return Ok(());
    }
    std::fs::create_dir_all(dest.parent().unwrap())?;
    let part = dest.with_extension("gguf.part");
    let mut done = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);

    let client = reqwest::Client::builder().user_agent("HomeLLM").build()?;
    let mut request = client.get(&model.url);
    if done > 0 {
        request = request.header(reqwest::header::RANGE, format!("bytes={done}-"));
    }
    let response = request.send().await?.error_for_status()?;
    if done > 0 && response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
        done = 0; // the server ignored Range: start over
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(done == 0)
        .open(&part)?;
    file.seek(std::io::SeekFrom::Start(done))?;

    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("download interrupted; run it again to resume")?;
        file.write_all(&chunk)?;
        done += chunk.len() as u64;
        progress(done, model.size);
    }
    file.flush()?;
    drop(file);

    let actual = sha256_file(&part)?;
    if actual != model.sha256 {
        std::fs::remove_file(&part)?;
        bail!("checksum mismatch for {}: got {actual}", model.file);
    }
    std::fs::rename(&part, &dest)?;
    Ok(())
}

fn sha256_file(path: &std::path::Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}
