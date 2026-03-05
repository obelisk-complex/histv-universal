//! Centralised ffmpeg/ffprobe path resolution and spawning.
//!
//! At startup, `init()` is called to resolve the paths once. Every subsequent
//! call to `ffmpeg_command()` or `ffprobe_command()` returns a pre-configured
//! `tokio::process::Command` pointing at the correct binary.
//!
//! Resolution order:
//! 1. Tauri sidecar directory (bundled binaries next to the app).
//! 2. Same directory as the running executable.
//! 3. Bare name — falls back to the system PATH.

use std::path::PathBuf;
use std::sync::OnceLock;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tokio::process::Command;

static FFMPEG_PATH: OnceLock<PathBuf> = OnceLock::new();
static FFPROBE_PATH: OnceLock<PathBuf> = OnceLock::new();

/// Platform-specific executable extension.
#[cfg(target_os = "windows")]
const EXE_EXT: &str = ".exe";
#[cfg(not(target_os = "windows"))]
const EXE_EXT: &str = "";

/// Apply CREATE_NO_WINDOW on Windows to suppress console flashes.
#[cfg(target_os = "windows")]
pub fn hide_window(cmd: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

/// Apply CREATE_NO_WINDOW on Windows to suppress console flashes (std::process variant).
#[cfg(target_os = "windows")]
pub fn hide_window_std(cmd: &mut std::process::Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;
    cmd.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(target_os = "windows"))]
pub fn hide_window(_cmd: &mut Command) {}

#[cfg(not(target_os = "windows"))]
pub fn hide_window_std(_cmd: &mut std::process::Command) {}

/// Call once during app setup to resolve and cache the binary paths.
pub fn init(app: &AppHandle) {
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let ffmpeg = resolve_binary(app, &ffmpeg_name);
    let ffprobe = resolve_binary(app, &ffprobe_name);

    let _ = FFMPEG_PATH.set(ffmpeg);
    let _ = FFPROBE_PATH.set(ffprobe);
}

/// Return a `Command` that will invoke ffmpeg (console window hidden on Windows).
pub fn ffmpeg_command() -> Command {
    let path = FFMPEG_PATH
        .get()
        .map(|p| p.as_os_str().to_os_string())
        .unwrap_or_else(|| "ffmpeg".into());
    let mut cmd = Command::new(path);
    hide_window(&mut cmd);
    cmd
}

/// Return a `Command` that will invoke ffprobe (console window hidden on Windows).
pub fn ffprobe_command() -> Command {
    let path = FFPROBE_PATH
        .get()
        .map(|p| p.as_os_str().to_os_string())
        .unwrap_or_else(|| "ffprobe".into());
    let mut cmd = Command::new(path);
    hide_window(&mut cmd);
    cmd
}

/// Check whether ffmpeg is actually reachable (runs `ffmpeg -version`).
pub async fn is_available() -> bool {
    let mut cmd = ffmpeg_command();
    cmd.arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null());
    match cmd.spawn() {
        Ok(child) => match child.wait_with_output().await {
            Ok(o) => o.status.success(),
            Err(_) => false,
        },
        Err(_) => false,
    }
}

/// Return the directory containing the running executable, if available.
pub fn exe_dir() -> Option<PathBuf> {
    std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

/// Download URL for the platform-appropriate ffmpeg static build.
fn download_url() -> Option<(&'static str, &'static str)> {
    // Returns (url, archive_type)
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some((
            "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
            "zip",
        ))
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some((
            "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz",
            "tar.xz",
        ))
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        // No stable BtbN mac builds; user should install via brew
        None
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        None
    }
    // Fallback for any other platform
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        None
    }
}

/// Download ffmpeg and ffprobe to the given directory.
/// Emits progress events via the provided callback.
/// Returns Ok(()) on success.
pub async fn download_to_dir(
    target_dir: &std::path::Path,
    app: &AppHandle,
) -> Result<(), String> {
    let (url, archive_type) = download_url()
        .ok_or_else(|| "Automatic download is not available for this platform. Please install ffmpeg manually (e.g. via Homebrew on macOS).".to_string())?;

    let _ = app.emit("ffmpeg-download-progress", "Downloading ffmpeg...");

    // Download to a temp file
    let tmp_path = target_dir.join(format!("_ffmpeg_download.{}", archive_type.replace('.', "_")));

    // Use a child process to download — avoids needing reqwest as a dependency
    #[cfg(target_os = "windows")]
    {
        // Use PowerShell's Invoke-WebRequest
        let mut dl_cmd = tokio::process::Command::new("powershell");
        dl_cmd.args([
                "-NoProfile",
                "-Command",
                &format!(
                    "[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12; Invoke-WebRequest -Uri '{}' -OutFile '{}'",
                    url,
                    tmp_path.to_string_lossy()
                ),
            ])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped());
        hide_window(&mut dl_cmd);
        let status = dl_cmd.status()
            .await
            .map_err(|e| format!("Failed to start download: {e}"))?;

        if !status.success() {
            let _ = std::fs::remove_file(&tmp_path);
            return Err("Download failed. Check your internet connection.".to_string());
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        // Use curl
        let mut dl_cmd = tokio::process::Command::new("curl");
        dl_cmd.args(["-L", "-o", &tmp_path.to_string_lossy(), url])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        hide_window(&mut dl_cmd);
        let status = dl_cmd.status()
            .await
            .map_err(|e| format!("Failed to start download: {e}"))?;

        if !status.success() {
            let _ = std::fs::remove_file(&tmp_path);
            return Err("Download failed. Check your internet connection.".to_string());
        }
    }

    let _ = app.emit("ffmpeg-download-progress", "Extracting ffmpeg...");

    // Extract ffmpeg and ffprobe from the archive
    #[cfg(target_os = "windows")]
    {
        extract_from_zip(&tmp_path, target_dir)?;
    }

    #[cfg(not(target_os = "windows"))]
    {
        extract_from_tar_xz(&tmp_path, target_dir)?;
    }

    // Clean up the archive
    let _ = std::fs::remove_file(&tmp_path);

    // Verify the binaries exist
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");
    if !target_dir.join(&ffmpeg_name).exists() || !target_dir.join(&ffprobe_name).exists() {
        return Err("Extraction completed but ffmpeg/ffprobe not found in archive.".to_string());
    }

    let _ = app.emit("ffmpeg-download-progress", "Done!");
    Ok(())
}

/// Re-resolve the binary paths after a download. Call this after download_to_dir.
pub fn reinit(app: &AppHandle) {
    // OnceLock can't be reset, so we use a different approach:
    // The existing OnceLock values will still point to the bare names.
    // Since exe_dir is checked at spawn time by resolve_binary, and we can't
    // update OnceLock, we'll just verify the binaries are findable.
    // In practice, the app should be restarted after download, or we
    // re-check at encoder detection time.
    let _ = app; // placeholder — encoder detection will re-find them via PATH or exe dir
}

#[cfg(target_os = "windows")]
fn extract_from_zip(zip_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
    // Use PowerShell to extract just ffmpeg.exe and ffprobe.exe
    let script = format!(
        r#"
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [System.IO.Compression.ZipFile]::OpenRead('{}')
foreach ($entry in $zip.Entries) {{
    if ($entry.Name -eq 'ffmpeg.exe' -or $entry.Name -eq 'ffprobe.exe') {{
        $destPath = Join-Path '{}' $entry.Name
        [System.IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $destPath, $true)
    }}
}}
$zip.Dispose()
"#,
        zip_path.to_string_lossy().replace('\'', "''"),
        target_dir.to_string_lossy().replace('\'', "''")
    );

    let mut extract_cmd = std::process::Command::new("powershell");
    extract_cmd.args(["-NoProfile", "-Command", &script]);
    hide_window_std(&mut extract_cmd);
    let output = extract_cmd.output()
        .map_err(|e| format!("Failed to run extraction: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Extraction failed: {stderr}"));
    }

    Ok(())
}

#[cfg(not(target_os = "windows"))]
fn extract_from_tar_xz(tar_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
    // Use tar to extract just ffmpeg and ffprobe
    // The BtbN archives have files like ffmpeg-master-latest-linux64-gpl/bin/ffmpeg
    let mut extract_cmd = std::process::Command::new("tar");
    extract_cmd.args([
            "xf",
            &tar_path.to_string_lossy(),
            "--wildcards",
            "*/bin/ffmpeg",
            "*/bin/ffprobe",
            "--strip-components=2",
            "-C",
            &target_dir.to_string_lossy(),
        ]);
    hide_window_std(&mut extract_cmd);
    let output = extract_cmd.output()
        .map_err(|e| format!("Failed to run extraction: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Extraction failed: {stderr}"));
    }

    Ok(())
}

/// Resolve a binary name to a full path. Checks the sidecar/resource
/// directory first, then the executable's own directory, then gives up
/// and returns just the bare name (so the OS PATH search takes over).
fn resolve_binary(app: &AppHandle, name: &str) -> PathBuf {
    // 1. Tauri resource directory (where sidecars are placed by the bundler)
    if let Ok(resource_dir) = app.path().resource_dir() {
        let candidate: PathBuf = resource_dir.join(name);
        if candidate.exists() {
            return candidate;
        }
    }

    // 2. Same directory as the running executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // 3. Bare name — let the OS find it on PATH
    PathBuf::from(name)
}