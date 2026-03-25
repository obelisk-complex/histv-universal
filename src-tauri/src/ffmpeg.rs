//! Centralised ffmpeg/ffprobe path resolution and spawning.
//!
//! At startup, `init()` is called to resolve the paths once. Every subsequent
//! call to `ffmpeg_command()` or `ffprobe_command()` returns a pre-configured
//! `tokio::process::Command` pointing at the correct binary.
//!
//! Resolution order:
//! 1. Tauri sidecar / resource directory (bundled binaries next to the app).
//! 2. Same directory as the running executable.
//! 3. App-data binary directory (where auto-downloaded ffmpeg lives).
//! 4. Well-known platform directories (Homebrew, MacPorts, Xcode CLT, snap, Chocolatey, etc.).
//!    This is critical on macOS where GUI apps do not inherit the shell PATH,
//!    and on older Windows/Linux installs where PATH may be incomplete.
//! 5. User's shell PATH (macOS/Linux — reads the login shell's PATH to catch
//!    directories not in the desktop session's environment).
//! 6. Bare name — falls back to the system PATH.
//!
//! Backwards-compatibility targets (~2016+):
//! - Windows 7 SP1 / 10 (x86_64) — PowerShell 5.1 extraction, Chocolatey/Scoop paths
//! - macOS 10.12 Sierra+ (x86_64 / ARM64) — Xcode CLT, Homebrew (Intel & AS), MacPorts
//! - Linux (x86_64) — FHS paths, snap, flatpak, Nix, linuxbrew

use std::path::{Path, PathBuf};
use std::sync::RwLock;
use tokio::process::Command;

use crate::events::EventSink;

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
///
/// `resource_dir` — Tauri resource/sidecar directory (pass `None` for CLI).
/// `_app_data_dir` — reserved for future use; app-data bin dir is resolved
///                   internally via `app_data_bin_dir()`.
/// `sink` — event output for logging which paths were resolved.
pub fn init(
    resource_dir: Option<&Path>,
    _app_data_dir: Option<&Path>,
    sink: &dyn EventSink,
) {
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let ffmpeg = resolve_binary(resource_dir, &ffmpeg_name);
    let ffprobe = resolve_binary(resource_dir, &ffprobe_name);

    log_resolved_path(sink, "ffmpeg", &ffmpeg);
    log_resolved_path(sink, "ffprobe", &ffprobe);

    if let Ok(mut w) = FFMPEG_PATH.write() { *w = Some(ffmpeg); }
    if let Ok(mut w) = FFPROBE_PATH.write() { *w = Some(ffprobe); }
}

/// Re-resolve the binary paths after a download.
pub fn reinit(
    resource_dir: Option<&Path>,
    sink: &dyn EventSink,
) {
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let ffmpeg = resolve_binary(resource_dir, &ffmpeg_name);
    let ffprobe = resolve_binary(resource_dir, &ffprobe_name);

    log_resolved_path(sink, "ffmpeg", &ffmpeg);
    log_resolved_path(sink, "ffprobe", &ffprobe);

    if let Ok(mut w) = FFMPEG_PATH.write() { *w = Some(ffmpeg); }
    if let Ok(mut w) = FFPROBE_PATH.write() { *w = Some(ffprobe); }
}

/// Log which path was resolved for a binary (helps diagnose "not found" reports).
fn log_resolved_path(sink: &dyn EventSink, label: &str, path: &PathBuf) {
    let display = path.display();
    if path.exists() {
        sink.log(&format!("[ffmpeg] {label} resolved: {display}"));
    } else if path.components().count() == 1 {
        // Bare name — will rely on OS PATH lookup at spawn time
        sink.log(&format!("[ffmpeg] {label} not found in known locations, will try PATH"));
    } else {
        sink.log(&format!("[ffmpeg] {label} resolved to {display} (does not exist yet)"));
    }
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

// ── Download URLs ───────────────────────────────────────────────

/// Download URL for the platform-appropriate ffmpeg static build.
#[cfg(feature = "downloader")]
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
        Some((
            "https://evermeet.cx/ffmpeg/get/zip",
            "zip",
        ))
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        Some((
            "https://evermeet.cx/ffmpeg/get/zip",
            "zip",
        ))
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

/// Download URL for ffprobe on platforms where it is bundled separately.
/// On macOS (evermeet.cx), ffmpeg and ffprobe are separate downloads.
#[cfg(all(feature = "downloader", target_os = "macos"))]
fn ffprobe_download_url() -> Option<(&'static str, &'static str)> {
    Some(("https://evermeet.cx/ffmpeg/get/ffprobe/zip", "zip"))
}

#[cfg(all(feature = "downloader", not(target_os = "macos")))]
fn ffprobe_download_url() -> Option<(&'static str, &'static str)> {
    // On Windows/Linux, ffprobe is included in the main archive
    None
}

// ── Download & extraction ───────────────────────────────────────

/// Download ffmpeg and ffprobe to the given directory.
/// Uses reqwest for cross-platform HTTP with progress reporting.
/// Returns Ok(()) on success.
#[cfg(feature = "downloader")]
pub async fn download_to_dir(
    target_dir: &std::path::Path,
    sink: &dyn EventSink,
) -> Result<(), String> {
    let (url, archive_type) = download_url()
        .ok_or_else(|| "Automatic download is not available for this platform. Please install ffmpeg manually (e.g. via Homebrew on macOS).".to_string())?;

    sink.ffmpeg_download_progress("Downloading ffmpeg... 0%");

    // Stream-download a URL to a file, reporting progress
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    download_file(&client, url, target_dir, archive_type, "ffmpeg", sink).await?;

    // On macOS, ffprobe is a separate download from evermeet.cx
    if let Some((probe_url, probe_archive)) = ffprobe_download_url() {
        sink.ffmpeg_download_progress("Downloading ffprobe...");
        download_file(&client, probe_url, target_dir, probe_archive, "ffprobe", sink).await?;
    }

    // Verify the binaries exist
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");
    if !target_dir.join(&ffmpeg_name).exists() || !target_dir.join(&ffprobe_name).exists() {
        return Err("Extraction completed but ffmpeg/ffprobe not found in archive.".to_string());
    }

    sink.ffmpeg_download_progress("Done!");
    Ok(())
}

/// Download a single archive, extract the relevant binary, and clean up.
#[cfg(feature = "downloader")]
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    target_dir: &std::path::Path,
    archive_type: &str,
    label: &str,
    sink: &dyn EventSink,
) -> Result<(), String> {
    let tmp_path = target_dir.join(format!("_histv_dl_{label}.{}", archive_type.replace('.', "_")));

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
                sink.ffmpeg_download_progress(
                    &format!("Downloading {label}... {pct}% ({mb_done:.1} / {mb_total:.1} MB)"),
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
    if file_size < 500_000 {
        let _ = std::fs::remove_file(&tmp_path);
        return Err(format!("{label} download appears incomplete. Check your internet connection."));
    }

    sink.ffmpeg_download_progress(&format!("Extracting {label}..."));

    // Extract the binary from the archive
    #[cfg(target_os = "windows")]
    {
        extract_from_zip(&tmp_path, target_dir)?;
    }

    #[cfg(target_os = "macos")]
    {
        extract_from_zip_flat(&tmp_path, target_dir)?;
    }

    #[cfg(target_os = "linux")]
    {
        extract_from_tar_xz(&tmp_path, target_dir)?;
    }

    // Clean up the archive
    let _ = std::fs::remove_file(&tmp_path);

    Ok(())
}

#[cfg(all(feature = "downloader", target_os = "windows"))]
fn extract_from_zip(zip_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
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

#[cfg(all(feature = "downloader", target_os = "linux"))]
fn extract_from_tar_xz(tar_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
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

    set_executable(target_dir);

    Ok(())
}

#[cfg(all(feature = "downloader", target_os = "macos"))]
fn extract_from_zip_flat(zip_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
    let mut extract_cmd = std::process::Command::new("unzip");
    extract_cmd.args([
        "-o",
        &zip_path.to_string_lossy(),
        "-d",
        &target_dir.to_string_lossy(),
    ]);
    hide_window_std(&mut extract_cmd);
    let output = extract_cmd.output();

    match output {
        Ok(o) if o.status.success() => {
            set_executable(target_dir);
            Ok(())
        }
        _ => {
            let mut ditto_cmd = std::process::Command::new("ditto");
            ditto_cmd.args([
                "-xk",
                &zip_path.to_string_lossy(),
                &target_dir.to_string_lossy(),
            ]);
            hide_window_std(&mut ditto_cmd);
            let ditto_out = ditto_cmd.output()
                .map_err(|e| format!("Failed to run ditto extraction: {e}"))?;
            if !ditto_out.status.success() {
                let stderr = String::from_utf8_lossy(&ditto_out.stderr);
                return Err(format!("Zip extraction failed: {stderr}"));
            }
            set_executable(target_dir);
            Ok(())
        }
    }
}

/// Ensure ffmpeg/ffprobe binaries are executable after extraction.
#[cfg(not(target_os = "windows"))]
fn set_executable(dir: &std::path::Path) {
    use std::os::unix::fs::PermissionsExt;
    for name in &["ffmpeg", "ffprobe"] {
        let path = dir.join(name);
        if path.exists() {
            if let Ok(meta) = std::fs::metadata(&path) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&path, perms);
            }
        }
    }
}

// ── Well-known directories ──────────────────────────────────────

#[cfg(target_os = "macos")]
const WELL_KNOWN_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/opt/local/bin",
    "/usr/local/Cellar/ffmpeg",
    "/nix/var/nix/profiles/default/bin",
];

#[cfg(target_os = "linux")]
const WELL_KNOWN_DIRS: &[&str] = &[
    "/usr/bin",
    "/usr/local/bin",
    "/snap/bin",
    "/var/lib/flatpak/exports/bin",
    "/home/linuxbrew/.linuxbrew/bin",
    "/nix/var/nix/profiles/default/bin",
];

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const WELL_KNOWN_DIRS: &[&str] = &[];

#[cfg(target_os = "windows")]
fn well_known_dirs_windows() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    if let Ok(choco) = std::env::var("ChocolateyInstall") {
        dirs.push(PathBuf::from(&choco).join("bin"));
    } else if let Ok(sd) = std::env::var("SystemDrive") {
        dirs.push(PathBuf::from(&sd).join("ProgramData").join("chocolatey").join("bin"));
    }

    if let Ok(home) = std::env::var("USERPROFILE") {
        dirs.push(PathBuf::from(&home).join("scoop").join("shims"));
        dirs.push(PathBuf::from(&home).join("ffmpeg").join("bin"));
        dirs.push(PathBuf::from(&home).join("Downloads").join("ffmpeg").join("bin"));
    }

    if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(&localappdata).join("Microsoft").join("WinGet").join("Links"));
    }

    if let Ok(pf) = std::env::var("ProgramFiles") {
        dirs.push(PathBuf::from(&pf).join("ffmpeg").join("bin"));
    }
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        dirs.push(PathBuf::from(&pf86).join("ffmpeg").join("bin"));
    }

    dirs
}

// ── Shell PATH resolution (macOS + Linux) ──────────────────────

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn shell_path_dirs() -> Vec<PathBuf> {
    #[cfg(target_os = "macos")]
    let default_shell = "/bin/zsh";
    #[cfg(target_os = "linux")]
    let default_shell = "/bin/bash";

    let shell = std::env::var("SHELL").unwrap_or_else(|_| default_shell.to_string());

    let output = std::process::Command::new(&shell)
        .args(["-l", "-c", "echo $PATH"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let path_str = String::from_utf8_lossy(&o.stdout);
            path_str
                .trim()
                .split(':')
                .filter(|s| !s.is_empty())
                .map(PathBuf::from)
                .collect()
        }
        _ => Vec::new(),
    }
}

// ── Binary resolution ───────────────────────────────────────────

/// Resolve a binary name to a full path. Checks multiple locations
/// in priority order, falling back to a bare name for PATH lookup.
fn resolve_binary(resource_dir: Option<&Path>, name: &str) -> PathBuf {
    // 1. Tauri resource directory (where sidecars are placed by the bundler)
    if let Some(res_dir) = resource_dir {
        let candidate: PathBuf = res_dir.join(name);
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

    // 4. Well-known platform directories
    #[cfg(target_os = "windows")]
    {
        for dir in well_known_dirs_windows() {
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        for dir_str in WELL_KNOWN_DIRS {
            let candidate = PathBuf::from(dir_str).join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // 4b. (Linux) ~/.local/bin — common user-local install path that
    //     can't go in the static const because it needs $HOME expansion.
    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            let candidate = PathBuf::from(home).join(".local").join("bin").join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // 5. (macOS/Linux) Directories from the user's login shell PATH.
    //    On macOS, GUI apps launched from Finder/Dock don't inherit the
    //    shell PATH at all.  On Linux, desktop sessions usually do inherit
    //    PATH, but some environments (Wayland compositors, snaps, AppImage
    //    launchers) may present a reduced PATH — this catches those cases.
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        for dir in shell_path_dirs() {
            let candidate = dir.join(name);
            if candidate.exists() {
                return candidate;
            }
        }
    }

    // 6. Bare name — let the OS find it on PATH
    PathBuf::from(name)
}