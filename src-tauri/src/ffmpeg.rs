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
use std::sync::RwLock;
use tauri::AppHandle;
use tauri::Emitter;
use tauri::Manager;
use tokio::process::Command;

static FFMPEG_PATH: RwLock<Option<PathBuf>> = RwLock::new(None);
static FFPROBE_PATH: RwLock<Option<PathBuf>> = RwLock::new(None);

/// Platform-specific executable extension.
#[cfg(target_os = "windows")]
const EXE_EXT: &str = ".exe";
#[cfg(not(target_os = "windows"))]
const EXE_EXT: &str = "";

/// Apply CREATE_NO_WINDOW on Windows to suppress console flashes.
#[cfg(target_os = "windows")]
pub fn hide_window(cmd: &mut Command) {
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

/// Return the app-data binary directory for storing downloaded ffmpeg/ffprobe.
/// - Windows: %APPDATA%\com.histv.encoder\bin
/// - macOS:   ~/Library/Application Support/com.histv.encoder/bin
/// - Linux:   ~/.local/share/com.histv.encoder/bin
pub fn app_data_bin_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        std::env::var("APPDATA").ok().map(|d| PathBuf::from(d).join("com.histv.encoder").join("bin"))
    }
    #[cfg(target_os = "macos")]
    {
        dirs_next().map(|d| d.join("com.histv.encoder").join("bin"))
    }
    #[cfg(target_os = "linux")]
    {
        std::env::var("XDG_DATA_HOME")
            .ok()
            .map(PathBuf::from)
            .or_else(|| std::env::var("HOME").ok().map(|h| PathBuf::from(h).join(".local").join("share")))
            .map(|d| d.join("com.histv.encoder").join("bin"))
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn dirs_next() -> Option<PathBuf> {
    std::env::var("HOME")
        .ok()
        .map(|h| PathBuf::from(h).join("Library").join("Application Support"))
}

/// Call once during app setup to resolve and cache the binary paths.
pub fn init(app: &AppHandle) {
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let ffmpeg = resolve_binary(app, &ffmpeg_name);
    let ffprobe = resolve_binary(app, &ffprobe_name);

    if let Ok(mut w) = FFMPEG_PATH.write() { *w = Some(ffmpeg); }
    if let Ok(mut w) = FFPROBE_PATH.write() { *w = Some(ffprobe); }
}

/// Re-resolve the binary paths after a download.
pub fn reinit(app: &AppHandle) {
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let ffmpeg = resolve_binary(app, &ffmpeg_name);
    let ffprobe = resolve_binary(app, &ffprobe_name);

    if let Ok(mut w) = FFMPEG_PATH.write() { *w = Some(ffmpeg); }
    if let Ok(mut w) = FFPROBE_PATH.write() { *w = Some(ffprobe); }
}

/// Return a `Command` that will invoke ffmpeg (console window hidden on Windows).
pub fn ffmpeg_command() -> Command {
    let path = FFMPEG_PATH
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|p| p.as_os_str().to_os_string()))
        .unwrap_or_else(|| "ffmpeg".into());
    let mut cmd = Command::new(path);
    hide_window(&mut cmd);
    cmd
}

/// Return a `Command` that will invoke ffprobe (console window hidden on Windows).
pub fn ffprobe_command() -> Command {
    let path = FFPROBE_PATH
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|p| p.as_os_str().to_os_string()))
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
/// Uses reqwest for cross-platform HTTP with progress reporting.
/// Returns Ok(()) on success.
pub async fn download_to_dir(
    target_dir: &std::path::Path,
    app: &AppHandle,
) -> Result<(), String> {
    let (url, archive_type) = download_url()
        .ok_or_else(|| "Automatic download is not available for this platform. Please install ffmpeg manually (e.g. via Homebrew on macOS).".to_string())?;

    let _ = app.emit("ffmpeg-download-progress", "Downloading ffmpeg... 0%");

    // Download to a temp file
    let tmp_path = target_dir.join(format!("_ffmpeg_download.{}", archive_type.replace('.', "_")));

    // Stream the download with progress
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let response = client.get(url).send().await
        .map_err(|e| format!("Download request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Download failed with HTTP {}", response.status()));
    }

    let total_size = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut last_pct: u64 = 0;

    let mut file = tokio::fs::File::create(&tmp_path).await
        .map_err(|e| format!("Failed to create temp file: {e}"))?;

    let mut stream = response.bytes_stream();
    use tokio::io::AsyncWriteExt;
    use futures_util::StreamExt;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download interrupted: {e}"))?;
        file.write_all(&chunk).await
            .map_err(|e| format!("Failed to write to temp file: {e}"))?;

        downloaded += chunk.len() as u64;

        if total_size > 0 {
            let pct = (downloaded * 100) / total_size;
            if pct != last_pct {
                last_pct = pct;
                let mb_done = downloaded as f64 / 1_048_576.0;
                let mb_total = total_size as f64 / 1_048_576.0;
                let _ = app.emit(
                    "ffmpeg-download-progress",
                    format!("Downloading ffmpeg... {pct}% ({mb_done:.1} / {mb_total:.1} MB)"),
                );
            }
        }
    }

    file.flush().await
        .map_err(|e| format!("Failed to flush temp file: {e}"))?;
    drop(file);

    // Verify download size
    let file_size = std::fs::metadata(&tmp_path)
        .map(|m| m.len())
        .unwrap_or(0);
    if file_size < 1_000_000 {
        let _ = std::fs::remove_file(&tmp_path);
        return Err("Download appears incomplete. Check your internet connection.".to_string());
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

/// Resolve a binary name to a full path. Checks multiple locations
/// in priority order, falling back to a bare name for PATH lookup.
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

    // 3. App-data binary directory (where we download ffmpeg to)
    if let Some(bin_dir) = app_data_bin_dir() {
        let candidate = bin_dir.join(name);
        if candidate.exists() {
            return candidate;
        }
    }

    // 4. Bare name — let the OS find it on PATH
    PathBuf::from(name)
}