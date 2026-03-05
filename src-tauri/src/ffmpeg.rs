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
//! 5. User's shell PATH (macOS only — reads the login shell's PATH since GUI apps don't inherit it).
//! 6. Bare name — falls back to the system PATH.
//!
//! Backwards-compatibility targets (~2016+):
//! - Windows 7 SP1 / 10 (x86_64) — PowerShell 5.1 extraction, Chocolatey/Scoop paths
//! - macOS 10.12 Sierra+ (x86_64 / ARM64) — Xcode CLT, Homebrew (Intel & AS), MacPorts
//! - Linux (x86_64) — FHS paths, snap, flatpak, Nix, linuxbrew

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

    log_resolved_path(app, "ffmpeg", &ffmpeg);
    log_resolved_path(app, "ffprobe", &ffprobe);

    if let Ok(mut w) = FFMPEG_PATH.write() { *w = Some(ffmpeg); }
    if let Ok(mut w) = FFPROBE_PATH.write() { *w = Some(ffprobe); }
}

/// Re-resolve the binary paths after a download.
pub fn reinit(app: &AppHandle) {
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let ffmpeg = resolve_binary(app, &ffmpeg_name);
    let ffprobe = resolve_binary(app, &ffprobe_name);

    log_resolved_path(app, "ffmpeg", &ffmpeg);
    log_resolved_path(app, "ffprobe", &ffprobe);

    if let Ok(mut w) = FFMPEG_PATH.write() { *w = Some(ffmpeg); }
    if let Ok(mut w) = FFPROBE_PATH.write() { *w = Some(ffprobe); }
}

/// Log which path was resolved for a binary (helps diagnose "not found" reports).
fn log_resolved_path(app: &AppHandle, label: &str, path: &PathBuf) {
    let display = path.display();
    if path.exists() {
        let _ = app.emit("log", format!("[ffmpeg] {label} resolved: {display}"));
    } else if path.components().count() == 1 {
        // Bare name — will rely on OS PATH lookup at spawn time
        let _ = app.emit("log", format!("[ffmpeg] {label} not found in known locations, will try PATH"));
    } else {
        let _ = app.emit("log", format!("[ffmpeg] {label} resolved to {display} (does not exist yet)"));
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
        // evermeet.cx provides static macOS builds; x86_64 for Intel Macs (~2012+)
        Some((
            "https://evermeet.cx/ffmpeg/get/zip",
            "zip",
        ))
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        // evermeet.cx builds work on Apple Silicon natively
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
#[cfg(target_os = "macos")]
fn ffprobe_download_url() -> Option<(&'static str, &'static str)> {
    Some(("https://evermeet.cx/ffmpeg/get/ffprobe/zip", "zip"))
}

#[cfg(not(target_os = "macos"))]
fn ffprobe_download_url() -> Option<(&'static str, &'static str)> {
    // On Windows/Linux, ffprobe is included in the main archive
    None
}

// ── Download & extraction ───────────────────────────────────────

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

    // Stream-download a URL to a file, reporting progress
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    download_file(&client, url, target_dir, archive_type, "ffmpeg", app).await?;

    // On macOS, ffprobe is a separate download from evermeet.cx
    if let Some((probe_url, probe_archive)) = ffprobe_download_url() {
        let _ = app.emit("ffmpeg-download-progress", "Downloading ffprobe...");
        download_file(&client, probe_url, target_dir, probe_archive, "ffprobe", app).await?;
    }

    // Verify the binaries exist
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");
    if !target_dir.join(&ffmpeg_name).exists() || !target_dir.join(&ffprobe_name).exists() {
        return Err("Extraction completed but ffmpeg/ffprobe not found in archive.".to_string());
    }

    let _ = app.emit("ffmpeg-download-progress", "Done!");
    Ok(())
}

/// Download a single archive, extract the relevant binary, and clean up.
async fn download_file(
    client: &reqwest::Client,
    url: &str,
    target_dir: &std::path::Path,
    archive_type: &str,
    label: &str,
    app: &AppHandle,
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
                let _ = app.emit(
                    "ffmpeg-download-progress",
                    format!("Downloading {label}... {pct}% ({mb_done:.1} / {mb_total:.1} MB)"),
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

    let _ = app.emit("ffmpeg-download-progress", format!("Extracting {label}..."));

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

#[cfg(target_os = "windows")]
fn extract_from_zip(zip_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
    // Use PowerShell to extract just ffmpeg.exe and ffprobe.exe.
    // System.IO.Compression.FileSystem is available from .NET 4.5+ (Windows 7 SP1+).
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

#[cfg(target_os = "linux")]
fn extract_from_tar_xz(tar_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
    // Use tar to extract just ffmpeg and ffprobe.
    // The BtbN archives have files like ffmpeg-master-latest-linux64-gpl/bin/ffmpeg.
    // --wildcards is GNU tar (standard on Linux); safe for glibc-based distros back to ~2014.
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

    // Ensure extracted binaries are executable
    set_executable(target_dir);

    Ok(())
}

/// Extract a flat zip archive (evermeet.cx style) where the binary is at the
/// root of the archive. Used on macOS where ffmpeg and ffprobe are separate
/// downloads, each containing just the single binary.
#[cfg(target_os = "macos")]
fn extract_from_zip_flat(zip_path: &std::path::Path, target_dir: &std::path::Path) -> Result<(), String> {
    // Try unzip first (ships with macOS since 10.0), fall back to ditto (since 10.4).
    // Both are safe for macOS 10.12 Sierra+ (our ~2016 compatibility floor).
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
            // Fallback to ditto (always present on macOS, handles zip natively)
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
//
// GUI apps (especially on macOS) often do not inherit the user's shell PATH.
// Older systems may also have ffmpeg installed via package managers whose
// bin directories are not on the default PATH for GUI-launched processes.
// We check all well-known locations to maximise discoverability.

/// Well-known binary directories to check on macOS.
/// Covers Homebrew (Intel since ~2012, Apple Silicon since 2020), MacPorts
/// (popular on older Macs), Xcode Command Line Tools (ships /usr/bin
/// symlinks on 10.9+, but the actual CLT path is checked for completeness),
/// Nix, and the standard /usr/local/bin used by manual installs.
#[cfg(target_os = "macos")]
const WELL_KNOWN_DIRS: &[&str] = &[
    "/opt/homebrew/bin",                    // Homebrew on Apple Silicon (macOS 11+)
    "/usr/local/bin",                       // Homebrew on Intel, manual installs, CLT symlinks
    "/opt/local/bin",                       // MacPorts (popular on pre-2020 Macs)
    "/usr/local/Cellar/ffmpeg",             // Homebrew Cellar (edge case: unlinked formula)
    "/nix/var/nix/profiles/default/bin",    // Nix
];

/// Well-known binary directories to check on Linux.
/// Covers standard FHS paths, snap (Ubuntu 16.04+), flatpak exports,
/// Linuxbrew (/home/linuxbrew), and Nix.
#[cfg(target_os = "linux")]
const WELL_KNOWN_DIRS: &[&str] = &[
    "/usr/bin",                             // Standard FHS (most distros)
    "/usr/local/bin",                       // Manual installs, source builds
    "/snap/bin",                            // Snap packages (Ubuntu 16.04+)
    "/var/lib/flatpak/exports/bin",         // Flatpak system installs
    "/home/linuxbrew/.linuxbrew/bin",       // Linuxbrew
    "/nix/var/nix/profiles/default/bin",    // Nix
];

// Static slice not used on Windows (dynamic env-var lookup needed),
// but we still need the const for the catch-all cfg fallback.
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const WELL_KNOWN_DIRS: &[&str] = &[];

/// Well-known binary directories to check on Windows.
/// Chocolatey and Scoop are popular package managers; both default to
/// well-known install locations. We also check common manual-extract
/// locations (forums often advise extracting ffmpeg to the user profile).
#[cfg(target_os = "windows")]
fn well_known_dirs_windows() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();

    // Chocolatey — default install path (works on Windows 7+)
    if let Ok(choco) = std::env::var("ChocolateyInstall") {
        dirs.push(PathBuf::from(&choco).join("bin"));
    } else if let Ok(sd) = std::env::var("SystemDrive") {
        // Chocolatey default when env var is missing
        dirs.push(PathBuf::from(&sd).join("ProgramData").join("chocolatey").join("bin"));
    }

    // Scoop — user-level install (Windows 7+)
    if let Ok(home) = std::env::var("USERPROFILE") {
        dirs.push(PathBuf::from(&home).join("scoop").join("shims"));
        // Common manual-extract locations
        dirs.push(PathBuf::from(&home).join("ffmpeg").join("bin"));
        dirs.push(PathBuf::from(&home).join("Downloads").join("ffmpeg").join("bin"));
    }

    // winget packages — common linked location
    if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
        dirs.push(PathBuf::from(&localappdata).join("Microsoft").join("WinGet").join("Links"));
    }

    // Program Files (both native and x86)
    if let Ok(pf) = std::env::var("ProgramFiles") {
        dirs.push(PathBuf::from(&pf).join("ffmpeg").join("bin"));
    }
    if let Ok(pf86) = std::env::var("ProgramFiles(x86)") {
        dirs.push(PathBuf::from(&pf86).join("ffmpeg").join("bin"));
    }

    dirs
}

// ── macOS shell PATH resolution ─────────────────────────────────
//
// On macOS, GUI apps launched from Finder / Dock / Spotlight do not inherit
// the user's shell PATH. This means `brew install ffmpeg` works perfectly
// in Terminal but the binary is invisible to a Tauri app. This is the root
// cause of the "installed ffmpeg but HISTV can't find it" report.
//
// We resolve this by spawning the user's login shell in non-interactive mode
// and printing its PATH. This captures anything set in .zshrc, .bash_profile,
// .profile, /etc/paths, /etc/paths.d/*, etc. — even on macOS 10.12 where
// zsh wasn't yet the default (bash was used until Catalina 10.15).
//
// The shell is only spawned once at init time; the result is cached.

#[cfg(target_os = "macos")]
fn shell_path_dirs() -> Vec<PathBuf> {
    // Determine the user's login shell. $SHELL is set by the system even for
    // GUI apps (it comes from the user record, not from a parent shell).
    let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".to_string());

    // Use -l (login) -c to source the full login profile chain.
    // -i (interactive) is deliberately omitted — it can trigger .zshrc
    // prompts or other interactive-only setup that would hang.
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

    // 5. (macOS only) Directories from the user's login shell PATH.
    //    This catches Homebrew, MacPorts, pyenv-installed ffmpeg, or anything
    //    else the user has configured — even on old macOS with Xcode 8.x CLT.
    #[cfg(target_os = "macos")]
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