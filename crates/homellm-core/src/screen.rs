//! Screenshots for models that see: saved as PNG under the app's data folder, where the
//! chat keeps its pictures. Only the newest few are kept.

use std::path::PathBuf;

use anyhow::{Result, bail};

const KEEP: usize = 10;

/// The folder with the chat's pictures (screenshots and copies of attached images).
pub fn images_dir() -> PathBuf {
    crate::data_dir().join("images")
}

/// Takes a screenshot of the primary screen; returns the file.
pub fn capture() -> Result<PathBuf> {
    let dir = images_dir();
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!(
        "screen-{}.png",
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    ));
    shoot(&path)?;
    if !path.is_file() {
        bail!("не получилось сделать скриншот");
    }
    prune(&dir);
    Ok(path)
}

#[cfg(windows)]
fn shoot(path: &std::path::Path) -> Result<()> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    // The path goes through an environment variable: no quoting inside the script.
    // DPI-aware first, or a scaled display (150%) is captured only in part.
    let script = "Add-Type -Name Dpi -Namespace HomeLLM -MemberDefinition \
        '[DllImport(\"user32.dll\")] public static extern bool SetProcessDPIAware();'; \
        [void][HomeLLM.Dpi]::SetProcessDPIAware(); \
        Add-Type -AssemblyName System.Windows.Forms,System.Drawing; \
        $b =[System.Windows.Forms.Screen]::PrimaryScreen.Bounds; \
        $bmp = New-Object System.Drawing.Bitmap $b.Width, $b.Height; \
        $g = [System.Drawing.Graphics]::FromImage($bmp); \
        $g.CopyFromScreen($b.Location, [System.Drawing.Point]::Empty, $b.Size); \
        $bmp.Save($env:HOMELLM_SHOT, [System.Drawing.Imaging.ImageFormat]::Png)";
    let status = std::process::Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .env("HOMELLM_SHOT", path)
        .creation_flags(CREATE_NO_WINDOW)
        .status()?;
    if !status.success() {
        bail!("не получилось сделать скриншот");
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn shoot(path: &std::path::Path) -> Result<()> {
    std::process::Command::new("screencapture")
        .arg("-x")
        .arg(path)
        .status()?;
    Ok(())
}

#[cfg(all(unix, not(target_os = "macos")))]
fn shoot(path: &std::path::Path) -> Result<()> {
    // Wayland first (grim), then X11 (import from ImageMagick).
    let grim = std::process::Command::new("grim").arg(path).status();
    if !grim.is_ok_and(|s| s.success()) {
        std::process::Command::new("import")
            .args(["-window", "root"])
            .arg(path)
            .status()?;
    }
    Ok(())
}

/// Old screenshots go; attached pictures stay (the chats refer to them).
fn prune(dir: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut shots: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("screen-"))
        })
        .collect();
    shots.sort();
    let extra = shots.len().saturating_sub(KEEP);
    for old in &shots[..extra] {
        let _ = std::fs::remove_file(old);
    }
}
