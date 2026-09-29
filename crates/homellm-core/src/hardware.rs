//! What the machine can run: RAM, CPU threads and video memory.

use std::process::Command;

use sysinfo::System;

#[derive(Debug, Clone)]
pub struct Hardware {
    pub os: String,
    pub cpu_threads: usize,
    pub ram_bytes: u64,
    pub gpu_name: Option<String>,
    /// Video memory usable for weights. Apple Silicon shares RAM, so it reports ~70% of RAM.
    pub vram_bytes: Option<u64>,
}

pub fn detect() -> Hardware {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu_all();
    let ram_bytes = sys.total_memory();
    let os = format!(
        "{} {}",
        System::name().unwrap_or_default(),
        System::os_version().unwrap_or_default()
    );
    let (gpu_name, vram_bytes) = match nvidia() {
        Some((name, vram)) => (Some(name), Some(vram)),
        None if cfg!(all(target_os = "macos", target_arch = "aarch64")) => {
            (Some("Apple Silicon".into()), Some(ram_bytes * 7 / 10))
        }
        None => (None, None),
    };
    Hardware {
        os,
        cpu_threads: sys.cpus().len(),
        ram_bytes,
        gpu_name,
        vram_bytes,
    }
}

/// First NVIDIA card through `nvidia-smi`. AMD/Intel detection is a TODO (Vulkan).
fn nvidia() -> Option<(String, u64)> {
    let out = Command::new("nvidia-smi")
        .args([
            "--query-gpu=name,memory.total",
            "--format=csv,noheader,nounits",
        ])
        .output()
        .ok()?;
    let text = String::from_utf8(out.stdout).ok()?;
    let (name, mib) = text.lines().next()?.split_once(',')?;
    let mib: u64 = mib.trim().parse().ok()?;
    Some((name.trim().to_string(), mib * 1024 * 1024))
}

pub fn gib(bytes: u64) -> String {
    format!("{:.1} ГБ", bytes as f64 / (1u64 << 30) as f64)
}
