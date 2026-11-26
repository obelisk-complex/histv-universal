//! Centralised ffmpeg/ffprobe path resolution and spawning.
//!
//! At startup, `init()` is called to resolve the paths once. Every subsequent
//! call to `ffmpeg_command()` or `ffprobe_command()` returns a pre-configured
//! `tokio::process::Command` pointing at the correct binary.
//!
//! Resolution order:
//! 1. Tauri sidecar / resource directory (bundled binaries next to the app).
//! 2. Same directory as the running executable, skipped when that is a
//!    system directory such as `/usr/bin` (a packaged install), so a system
//!    ffmpeg beside the app does not shadow the downloaded one in step 3.
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

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{OnceLock, RwLock};
use tokio::process::Command;

use crate::events::EventSink;

// Fast path: OnceLock is set at init() and provides zero-overhead reads
// (no lock acquisition, no PathBuf clone) for the entire lifetime of the
// process. The rare reinit() case (after ffmpeg download) sets the
// override RwLock and flips the AtomicBool so subsequent reads fall back
// to the RwLock instead.
static FFMPEG_INIT: OnceLock<OsString> = OnceLock::new();
static FFPROBE_INIT: OnceLock<OsString> = OnceLock::new();
static FFMPEG_OVERRIDE: RwLock<Option<OsString>> = RwLock::new(None);
static FFPROBE_OVERRIDE: RwLock<Option<OsString>> = RwLock::new(None);
static HAS_OVERRIDE: AtomicBool = AtomicBool::new(false);

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
        std::env::var("APPDATA")
            .ok()
            .map(|d| PathBuf::from(d).join("com.histv.encoder").join("bin"))
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
            .or_else(|| {
                std::env::var("HOME")
                    .ok()
                    .map(|h| PathBuf::from(h).join(".local").join("share"))
            })
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
pub fn init(resource_dir: Option<&Path>, _app_data_dir: Option<&Path>, sink: &dyn EventSink) {
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let ffmpeg = resolve_binary(resource_dir, &ffmpeg_name);
    let ffprobe = resolve_binary(resource_dir, &ffprobe_name);

    log_resolved_path(sink, "ffmpeg", &ffmpeg);
    log_resolved_path(sink, "ffprobe", &ffprobe);

    // OnceLock::set returns Err if already set (idempotent; no panic).
    let _ = FFMPEG_INIT.set(ffmpeg.into_os_string());
    let _ = FFPROBE_INIT.set(ffprobe.into_os_string());
}

/// Re-resolve the binary paths after a download.
/// This is a rare operation (only after auto-download). It stores the new
/// paths in the override RwLock and flips the AtomicBool flag so that
/// subsequent reads use the override instead of the OnceLock fast path.
pub fn reinit(resource_dir: Option<&Path>, sink: &dyn EventSink) {
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");

    let ffmpeg = resolve_binary(resource_dir, &ffmpeg_name);
    let ffprobe = resolve_binary(resource_dir, &ffprobe_name);

    log_resolved_path(sink, "ffmpeg", &ffmpeg);
    log_resolved_path(sink, "ffprobe", &ffprobe);

    if let Ok(mut w) = FFMPEG_OVERRIDE.write() {
        *w = Some(ffmpeg.into_os_string());
    }
    if let Ok(mut w) = FFPROBE_OVERRIDE.write() {
        *w = Some(ffprobe.into_os_string());
    }
    HAS_OVERRIDE.store(true, Ordering::Release);
}

/// Log which path was resolved for a binary (helps diagnose "not found" reports).
fn log_resolved_path(sink: &dyn EventSink, label: &str, path: &Path) {
    let display = path.display();
    if path.exists() {
        sink.log(&format!("[ffmpeg] {label} resolved: {display}"));
    } else if path.components().count() == 1 {
        // Bare name — will rely on OS PATH lookup at spawn time
        sink.log(&format!(
            "[ffmpeg] {label} not found in known locations, will try PATH"
        ));
    } else {
        sink.log(&format!(
            "[ffmpeg] {label} resolved to {display} (does not exist yet)"
        ));
    }
}

/// Resolve the current ffmpeg path. Fast path (no lock) unless reinit has
/// been called; fallback to the override RwLock otherwise.
fn resolve_ffmpeg_os() -> OsString {
    if !HAS_OVERRIDE.load(Ordering::Acquire) {
        if let Some(p) = FFMPEG_INIT.get() {
            return p.clone();
        }
    } else if let Ok(r) = FFMPEG_OVERRIDE.read() {
        if let Some(ref p) = *r {
            return p.clone();
        }
    }
    // Fallback: OnceLock may have been set even if override is active
    FFMPEG_INIT
        .get()
        .cloned()
        .unwrap_or_else(|| "ffmpeg".into())
}

/// Resolve the current ffprobe path. Same pattern as `resolve_ffmpeg_os`.
fn resolve_ffprobe_os() -> OsString {
    if !HAS_OVERRIDE.load(Ordering::Acquire) {
        if let Some(p) = FFPROBE_INIT.get() {
            return p.clone();
        }
    } else if let Ok(r) = FFPROBE_OVERRIDE.read() {
        if let Some(ref p) = *r {
            return p.clone();
        }
    }
    FFPROBE_INIT
        .get()
        .cloned()
        .unwrap_or_else(|| "ffprobe".into())
}

/// Return a `Command` that will invoke ffmpeg (console window hidden on Windows).
pub fn ffmpeg_command() -> Command {
    let mut cmd = Command::new(resolve_ffmpeg_os());
    hide_window(&mut cmd);
    cmd
}

/// Return a `Command` that will invoke ffprobe (console window hidden on Windows).
pub fn ffprobe_command() -> Command {
    let mut cmd = Command::new(resolve_ffprobe_os());
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
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.to_path_buf()))
}

// ── Download URLs ───────────────────────────────────────────────

/// Pinned histv ffmpeg release. Fill in the four SHA-256 values below once
/// the release is built (one edit); an all-zero value means "not pinned yet"
/// and the candidate is skipped in favour of the fallbacks.
#[cfg(feature = "downloader")]
#[allow(dead_code)] // each platform uses only its own pin
mod histv_release {
    pub const TAG: &str = "ffmpeg-n8.1.3-histv.2";
    pub const SHA256_LINUX_X64: &str =
        "3b6e92edbd1944f4816d1de31e507b82beaf6358692f32651ef08b228c34407a";
    pub const SHA256_WINDOWS_X64: &str =
        "32f0ef2378efb7db546cd76e9b7d32de3175b6df0af0b84fe1ac713a477edb94";
    pub const SHA256_MACOS_ARM64: &str =
        "6e8d74820cefbbc3b997cb972b13f1dbca9b60da013b2e80a899d249c7e9f80e";
    pub const SHA256_MACOS_X64: &str =
        "50052f6f189bb75c8f592d6eb207ba47b8071d1229fdeed96a0672dceac83be4";
}

/// One place to fetch an ffmpeg archive from, with an optional pinned digest.
#[cfg(feature = "downloader")]
#[derive(Debug, Clone, PartialEq, Eq)]
struct Candidate {
    url: String,
    archive_type: &'static str,
    /// Lowercase hex SHA-256 of the archive. `None` means unverified.
    sha256: Option<&'static str>,
}

#[cfg(feature = "downloader")]
impl Candidate {
    fn unverified(url: &str, archive_type: &'static str) -> Self {
        Self {
            url: url.to_string(),
            archive_type,
            sha256: None,
        }
    }

    /// An asset of the histv release, verified against `sha256`.
    #[allow(dead_code)] // unused on platforms without a histv build
    fn histv(asset: &str, archive_type: &'static str, sha256: &'static str) -> Self {
        Self {
            url: format!(
                "https://github.com/obelisk-complex/histv-universal/releases/download/{}/{asset}",
                histv_release::TAG
            ),
            archive_type,
            sha256: Some(sha256),
        }
    }
}

/// Download candidates for the platform-appropriate ffmpeg static build,
/// tried in order. The histv release comes first and is verified; the
/// third-party builds after it are unverified fallbacks.
#[cfg(feature = "downloader")]
fn download_urls() -> Vec<Candidate> {
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        vec![
            Candidate::histv(
                "histv-ffmpeg-windows-x64.zip",
                "zip",
                histv_release::SHA256_WINDOWS_X64,
            ),
            Candidate::unverified(
                "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
                "zip",
            ),
            Candidate::unverified(
                "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-n7.1-latest-win64-gpl-7.1.zip",
                "zip",
            ),
        ]
    }
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        vec![
            Candidate::histv(
                "histv-ffmpeg-linux-x64.tar.gz",
                "tar.gz",
                histv_release::SHA256_LINUX_X64,
            ),
            Candidate::unverified(
                "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz",
                "tar.xz",
            ),
            Candidate::unverified(
                "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-n7.1-latest-linux64-gpl-7.1.tar.xz",
                "tar.xz",
            ),
            Candidate::unverified(
                "https://johnvansickle.com/ffmpeg/releases/ffmpeg-release-amd64-static.tar.xz",
                "tar.xz",
            ),
        ]
    }
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    {
        // BtbN does not publish macOS x86_64 builds; only the evermeet
        // universal/x64 archive is available as a fallback. If evermeet is
        // down, users should install via Homebrew (`brew install ffmpeg`).
        vec![
            Candidate::histv(
                "histv-ffmpeg-macos-x64.zip",
                "zip",
                histv_release::SHA256_MACOS_X64,
            ),
            Candidate::unverified("https://evermeet.cx/ffmpeg/get/zip", "zip"),
        ]
    }
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    {
        vec![
            Candidate::histv(
                "histv-ffmpeg-macos-arm64.zip",
                "zip",
                histv_release::SHA256_MACOS_ARM64,
            ),
            Candidate::unverified("https://evermeet.cx/ffmpeg/get/zip", "zip"),
            Candidate::unverified(
                "https://github.com/BtbN/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-macosarm64-gpl.zip",
                "zip",
            ),
        ]
    }
    // No known source for other platforms
    #[cfg(not(any(
        all(target_os = "windows", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "macos", target_arch = "aarch64"),
    )))]
    {
        vec![]
    }
}

/// Download URL candidates for ffprobe on platforms where it is bundled separately.
/// On macOS (evermeet.cx), ffmpeg and ffprobe are separate downloads.
/// The BtbN builds include ffprobe in the main archive so only evermeet needs this.
#[cfg(all(feature = "downloader", target_os = "macos"))]
fn ffprobe_download_urls() -> Vec<(&'static str, &'static str)> {
    vec![("https://evermeet.cx/ffmpeg/get/ffprobe/zip", "zip")]
}

#[cfg(all(feature = "downloader", not(target_os = "macos")))]
fn ffprobe_download_urls() -> Vec<(&'static str, &'static str)> {
    // On Windows/Linux, ffprobe is included in the main archive
    vec![]
}

// ── Download & extraction ───────────────────────────────────────

/// Download ffmpeg and ffprobe to the given directory.
/// Uses reqwest for cross-platform HTTP with progress reporting.
/// Tries each URL in the fallback chain until one succeeds.
/// Returns Ok(()) on success.
#[cfg(feature = "downloader")]
pub async fn download_to_dir(
    target_dir: &std::path::Path,
    sink: &dyn EventSink,
) -> Result<(), String> {
    let urls = download_urls();
    if urls.is_empty() {
        return Err("Automatic download is not available on this platform \
                     (install ffmpeg manually, e.g. via Homebrew on macOS)"
            .to_string());
    }

    sink.ffmpeg_download_progress("Downloading ffmpeg 0%");

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    download_from_candidates(&client, &urls, target_dir, "ffmpeg", sink)
        .await
        .map_err(|e| format!("All download mirrors failed: {e}"))?;

    // On macOS, the evermeet fallback ships ffprobe separately. Skip it when
    // the primary archive already provided one.
    let probe_urls = ffprobe_download_urls();
    if !probe_urls.is_empty() && !target_dir.join(format!("ffprobe{EXE_EXT}")).exists() {
        sink.ffmpeg_download_progress("Downloading ffprobe");
        let mut probe_ok = false;
        let mut probe_err = String::new();
        for (i, (url, archive_type)) in probe_urls.iter().enumerate() {
            let candidate = Candidate::unverified(url, archive_type);
            match download_file(&client, &candidate, target_dir, "ffprobe", sink).await {
                Ok(()) => {
                    probe_ok = true;
                    break;
                }
                Err(e) => {
                    probe_err = e;
                    if i + 1 < probe_urls.len() {
                        sink.log(&format!(
                            "[ffmpeg] ffprobe mirror {url} failed ({probe_err}), trying next..."
                        ));
                    }
                }
            }
        }
        if !probe_ok {
            return Err(format!("All ffprobe download mirrors failed: {probe_err}"));
        }
    }

    // Verify the binaries exist
    let ffmpeg_name = format!("ffmpeg{EXE_EXT}");
    let ffprobe_name = format!("ffprobe{EXE_EXT}");
    if !target_dir.join(&ffmpeg_name).exists() || !target_dir.join(&ffprobe_name).exists() {
        return Err("ffmpeg/ffprobe not found in archive".to_string());
    }

    sink.ffmpeg_download_progress("ffmpeg downloaded");
    Ok(())
}

/// Try each candidate in order and stop at the first that downloads,
/// verifies and extracts. Candidates whose pinned digest is still the
/// all-zero placeholder are skipped. On failure returns the last error.
#[cfg(feature = "downloader")]
async fn download_from_candidates(
    client: &reqwest::Client,
    candidates: &[Candidate],
    target_dir: &std::path::Path,
    label: &str,
    sink: &dyn EventSink,
) -> Result<(), String> {
    let mut last_err = String::new();
    for (i, candidate) in candidates.iter().enumerate() {
        let url = &candidate.url;
        if candidate.sha256.is_some_and(is_placeholder_sha256) {
            sink.log(&format!("[ffmpeg] Skipping {url}: checksum not pinned yet"));
            continue;
        }
        match download_file(client, candidate, target_dir, label, sink).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = e;
                if i + 1 < candidates.len() {
                    sink.log(&format!(
                        "[ffmpeg] Mirror {url} failed ({last_err}), trying next..."
                    ));
                }
            }
        }
    }
    Err(last_err)
}

/// True for the all-zero digest used before a release's real hash is known.
#[cfg(feature = "downloader")]
fn is_placeholder_sha256(expected: &str) -> bool {
    !expected.is_empty() && expected.bytes().all(|b| b == b'0')
}

/// Stream a file through SHA-256 and return the lowercase hex digest.
#[cfg(feature = "downloader")]
fn sha256_file(path: &std::path::Path) -> std::io::Result<String> {
    use sha2::{Digest, Sha256};
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

/// Check a file against an expected hex SHA-256 (case-insensitive).
#[cfg(feature = "downloader")]
fn verify_sha256(path: &std::path::Path, expected: &str) -> Result<(), String> {
    let actual = sha256_file(path).map_err(|e| format!("Failed to read download: {e}"))?;
    if actual.eq_ignore_ascii_case(expected) {
        Ok(())
    } else {
        Err(format!(
            "Checksum mismatch: expected {expected}, got {actual}"
        ))
    }
}

/// Removes the downloaded archive when dropped, so every exit path of
/// `download_file` (success, failed check, failed extraction) cleans up.
#[cfg(feature = "downloader")]
struct RemoveOnDrop(std::path::PathBuf);

#[cfg(feature = "downloader")]
impl Drop for RemoveOnDrop {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Download a single archive, verify it when a digest is pinned, extract the
/// relevant binaries, and clean up.
#[cfg(feature = "downloader")]
async fn download_file(
    client: &reqwest::Client,
    candidate: &Candidate,
    target_dir: &std::path::Path,
    label: &str,
    sink: &dyn EventSink,
) -> Result<(), String> {
    let url = candidate.url.as_str();
    let tmp_path = target_dir.join(format!(
        "_histv_dl_{label}.{}",
        candidate.archive_type.replace('.', "_")
    ));
    let _archive_guard = RemoveOnDrop(tmp_path.clone());

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download request failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status()));
    }

    let total_size = response.content_length().unwrap_or(0);
    let mut downloaded: u64 = 0;
    let mut last_pct: u64 = 0;

    let mut file = tokio::fs::File::create(&tmp_path)
        .await
        .map_err(|e| format!("Failed to create temp file: {e}"))?;

    let mut stream = response.bytes_stream();
    use futures_util::StreamExt;
    use tokio::io::AsyncWriteExt;

    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| format!("Download interrupted: {e}"))?;
        file.write_all(&chunk)
            .await
            .map_err(|e| format!("Failed to write to temp file: {e}"))?;

        downloaded += chunk.len() as u64;

        if total_size > 0 {
            let pct = (downloaded * 100) / total_size;
            if pct != last_pct {
                last_pct = pct;
                let mb_done = downloaded as f64 / 1_048_576.0;
                let mb_total = total_size as f64 / 1_048_576.0;
                sink.ffmpeg_download_progress(&format!(
                    "Downloading {label} {pct}% ({mb_done:.1} / {mb_total:.1} MB)"
                ));
            }
        }
    }

    file.flush()
        .await
        .map_err(|e| format!("Failed to flush temp file: {e}"))?;
    drop(file);

    // Verify download size
    let file_size = std::fs::metadata(&tmp_path).map(|m| m.len()).unwrap_or(0);
    if file_size < 500_000 {
        return Err(format!(
            "{label} download appears incomplete. Check your internet connection."
        ));
    }

    if let Some(expected) = candidate.sha256 {
        sink.ffmpeg_download_progress(&format!("Verifying {label}"));
        verify_sha256(&tmp_path, expected)?;
    }

    sink.ffmpeg_download_progress(&format!("Extracting {label}"));

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
        extract_from_tar(&tmp_path, target_dir)?;
    }

    Ok(())
}

#[cfg(all(feature = "downloader", target_os = "windows"))]
fn extract_from_zip(
    zip_path: &std::path::Path,
    target_dir: &std::path::Path,
) -> Result<(), String> {
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
    let output = extract_cmd
        .output()
        .map_err(|e| format!("Failed to run extraction: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Extraction failed: {stderr}"));
    }

    Ok(())
}

/// Extract `ffmpeg` and `ffprobe` from a tar archive (gzip or xz; GNU tar
/// detects the compression). Handles both the flat histv layout (binaries at
/// the archive root) and the BtbN layout (`<dir>/bin/`).
#[cfg(all(feature = "downloader", target_os = "linux"))]
fn extract_from_tar(
    tar_path: &std::path::Path,
    target_dir: &std::path::Path,
) -> Result<(), String> {
    // GNU tar's --wildcards + --strip-components silently drops files on
    // some versions (observed on GNU tar 1.35 / Ubuntu 24.04). Instead,
    // extract the full archive to a temp directory, then move just the
    // binaries we need. Using tempfile::TempDir avoids race conditions
    // when multiple instances download concurrently — each gets a unique
    // directory that is automatically cleaned up on drop.
    let tmp_extract =
        tempfile::tempdir_in(target_dir).map_err(|e| format!("Failed to create temp dir: {e}"))?;

    let mut extract_cmd = std::process::Command::new("tar");
    extract_cmd.args([
        "xf",
        tar_path.to_string_lossy().as_ref(),
        "-C",
        tmp_extract.path().to_string_lossy().as_ref(),
    ]);
    hide_window_std(&mut extract_cmd);
    let output = extract_cmd
        .output()
        .map_err(|e| format!("Failed to run extraction: {e}"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("Extraction failed: {stderr}"));
    }

    // Look at the archive root first, then at <top-level dir>/bin.
    let mut search_dirs = vec![tmp_extract.path().to_path_buf()];
    if let Ok(entries) = std::fs::read_dir(tmp_extract.path()) {
        for entry in entries.flatten() {
            let bin_dir = entry.path().join("bin");
            if bin_dir.is_dir() {
                search_dirs.push(bin_dir);
            }
        }
    }

    let mut found = false;
    for dir in &search_dirs {
        for name in &["ffmpeg", "ffprobe"] {
            let src = dir.join(name);
            if !src.is_file() {
                continue;
            }
            let dst = target_dir.join(name);
            if let Err(e) = std::fs::rename(&src, &dst) {
                // rename may fail across filesystems; fall back to copy
                if let Err(e2) = std::fs::copy(&src, &dst) {
                    return Err(format!(
                        "Could not move {name} to target: rename={e}, copy={e2}"
                    ));
                }
            }
            found = true;
        }
    }

    // TempDir auto-cleans on drop; no manual removal needed.

    if !found {
        return Err("Extracted archive but could not find ffmpeg or ffprobe".to_string());
    }

    set_executable(target_dir);

    Ok(())
}

#[cfg(all(feature = "downloader", target_os = "macos"))]
fn extract_from_zip_flat(
    zip_path: &std::path::Path,
    target_dir: &std::path::Path,
) -> Result<(), String> {
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
            let ditto_out = ditto_cmd
                .output()
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
#[cfg(all(feature = "downloader", not(target_os = "windows")))]
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
        dirs.push(
            PathBuf::from(&sd)
                .join("ProgramData")
                .join("chocolatey")
                .join("bin"),
        );
    }

    if let Ok(home) = std::env::var("USERPROFILE") {
        dirs.push(PathBuf::from(&home).join("scoop").join("shims"));
        dirs.push(PathBuf::from(&home).join("ffmpeg").join("bin"));
        dirs.push(
            PathBuf::from(&home)
                .join("Downloads")
                .join("ffmpeg")
                .join("bin"),
        );
    }

    if let Ok(localappdata) = std::env::var("LOCALAPPDATA") {
        dirs.push(
            PathBuf::from(&localappdata)
                .join("Microsoft")
                .join("WinGet")
                .join("Links"),
        );
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
        .args(["-l", "-c", "printenv PATH"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let raw = String::from_utf8_lossy(&o.stdout);
            // Take only the last non-empty line as a safety measure
            // against chatty shell profiles that emit extra output.
            let path_str = raw
                .lines()
                .rev()
                .find(|l| !l.trim().is_empty())
                .unwrap_or("");
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

/// Directories where a distro package or package manager installs
/// executables. `WELL_KNOWN_DIRS` does not fit here: it omits `/bin` and the
/// `sbin` directories and includes locations that only hold ffmpeg itself.
const SYSTEM_EXE_DIRS: &[&str] = &[
    "/usr/bin",
    "/usr/local/bin",
    "/bin",
    "/usr/sbin",
    "/usr/local/sbin",
    "/opt/homebrew/bin",
];

/// True when `dir` is a shared system directory. An ffmpeg found there
/// belongs to the OS, not to this app, so it must not shadow the pinned
/// ffmpeg that HISTV downloads. Compared by path component, so a trailing
/// slash does not matter.
fn exe_dir_is_system_dir(dir: &Path) -> bool {
    SYSTEM_EXE_DIRS.iter().any(|d| Path::new(d) == dir)
}

/// `name` inside `exe_dir`, when that file exists and `exe_dir` is not a
/// system directory according to `is_system_dir`.
fn sibling_binary(
    exe_dir: &Path,
    name: &str,
    is_system_dir: impl Fn(&Path) -> bool,
) -> Option<PathBuf> {
    if is_system_dir(exe_dir) {
        return None;
    }
    let candidate = exe_dir.join(name);
    candidate.exists().then_some(candidate)
}

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

    // 2. Same directory as the running executable, unless that is a system
    //    directory (distro package in /usr/bin): the ffmpeg there is the
    //    OS's, and step 3 must get the chance to supply the pinned fork.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(candidate) = exe
            .parent()
            .and_then(|dir| sibling_binary(dir, name, exe_dir_is_system_dir))
        {
            return candidate;
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use std::fs;

    // ── app_data_bin_dir ────────────────────────────────────────

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn test_app_data_bin_dir_xdg() {
        let orig = std::env::var("XDG_DATA_HOME").ok();
        std::env::set_var("XDG_DATA_HOME", "/tmp/test-xdg-data");
        let result = app_data_bin_dir();
        // Restore
        match orig {
            Some(v) => std::env::set_var("XDG_DATA_HOME", v),
            None => std::env::remove_var("XDG_DATA_HOME"),
        }
        assert_eq!(
            result,
            Some(PathBuf::from("/tmp/test-xdg-data/com.histv.encoder/bin"))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn test_app_data_bin_dir_home_fallback() {
        let orig_xdg = std::env::var("XDG_DATA_HOME").ok();
        let orig_home = std::env::var("HOME").ok();
        std::env::remove_var("XDG_DATA_HOME");
        std::env::set_var("HOME", "/tmp/test-home");
        let result = app_data_bin_dir();
        // Restore
        if let Some(v) = orig_xdg {
            std::env::set_var("XDG_DATA_HOME", v);
        }
        match orig_home {
            Some(v) => std::env::set_var("HOME", v),
            None => std::env::remove_var("HOME"),
        }
        assert_eq!(
            result,
            Some(PathBuf::from(
                "/tmp/test-home/.local/share/com.histv.encoder/bin"
            ))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    #[serial]
    fn test_app_data_bin_dir_no_env() {
        let orig_xdg = std::env::var("XDG_DATA_HOME").ok();
        let orig_home = std::env::var("HOME").ok();
        std::env::remove_var("XDG_DATA_HOME");
        std::env::remove_var("HOME");
        let result = app_data_bin_dir();
        // Restore
        if let Some(v) = orig_xdg {
            std::env::set_var("XDG_DATA_HOME", v);
        }
        if let Some(v) = orig_home {
            std::env::set_var("HOME", v);
        }
        assert_eq!(result, None);
    }

    // ── resolve_binary ──────────────────────────────────────────

    #[test]
    fn test_resolve_binary_resource_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_ffmpeg = tmp.path().join("ffmpeg");
        fs::write(&fake_ffmpeg, b"fake").unwrap();

        let result = resolve_binary(Some(tmp.path()), "ffmpeg");
        assert_eq!(result, fake_ffmpeg);
    }

    #[test]
    fn test_sibling_binary_found_in_non_system_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let fake_ffmpeg = tmp.path().join("ffmpeg");
        fs::write(&fake_ffmpeg, b"fake").unwrap();

        let result = sibling_binary(tmp.path(), "ffmpeg", |_| false);
        assert_eq!(result, Some(fake_ffmpeg));
    }

    #[test]
    fn test_sibling_binary_skipped_in_system_dir() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("ffmpeg"), b"fake").unwrap();

        let result = sibling_binary(tmp.path(), "ffmpeg", |dir| dir == tmp.path());
        assert_eq!(result, None);
    }

    #[test]
    fn test_sibling_binary_missing_file() {
        let tmp = tempfile::tempdir().unwrap();

        let result = sibling_binary(tmp.path(), "ffmpeg", |_| false);
        assert_eq!(result, None);
    }

    #[test]
    fn test_resolve_binary_bare_name_fallback() {
        // When no candidate exists in any known location, should return
        // the bare name for PATH lookup.
        let result = resolve_binary(None, "totally_nonexistent_binary_xyz");
        assert_eq!(result, PathBuf::from("totally_nonexistent_binary_xyz"));
    }

    #[test]
    fn test_resolve_binary_resource_dir_none() {
        // With no resource dir and a nonexistent binary, falls through
        // to bare name.
        let result = resolve_binary(None, "ffmpeg_test_missing");
        assert_eq!(result, PathBuf::from("ffmpeg_test_missing"));
    }

    #[test]
    fn test_resolve_binary_resource_dir_missing_file() {
        let tmp = tempfile::tempdir().unwrap();
        // Resource dir exists but doesn't contain the binary
        let result = resolve_binary(Some(tmp.path()), "ffmpeg");
        // Should fall through to other locations or bare name
        // (won't be the resource dir path)
        assert!(!result.starts_with(tmp.path()));
    }

    // ── exe_dir_is_system_dir ───────────────────────────────────

    #[test]
    fn test_exe_dir_is_system_dir_true_for_system_dirs() {
        for dir in [
            "/usr/bin",
            "/usr/local/bin",
            "/bin",
            "/usr/sbin",
            "/usr/local/sbin",
            "/opt/homebrew/bin",
        ] {
            assert!(exe_dir_is_system_dir(Path::new(dir)), "{dir}");
        }
    }

    #[test]
    fn test_exe_dir_is_system_dir_ignores_trailing_slash() {
        assert!(exe_dir_is_system_dir(Path::new("/usr/bin/")));
        assert!(exe_dir_is_system_dir(Path::new("/usr/local/bin/")));
    }

    #[test]
    fn test_exe_dir_is_system_dir_false_for_bundled_layouts() {
        for dir in [
            "/tmp/.mount_HISTV-abc123/usr/bin",
            "/opt/histv",
            "/opt/histv/bin",
            "/usr/lib/histv",
            "/usr/local/lib/histv",
            "/usr/bin/histv-portable",
            "/usr/binx",
            "/home/user/Apps/histv",
            "/Applications/HISTV.app/Contents/MacOS",
            "/usr",
            "/",
            "usr/bin",
            "",
        ] {
            assert!(!exe_dir_is_system_dir(Path::new(dir)), "{dir}");
        }
    }

    // ── log_resolved_path ───────────────────────────────────────

    #[test]
    fn test_log_resolved_path_exists() {
        let sink = crate::test_helpers::RecordingSink::new();
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("ffmpeg");
        fs::write(&path, b"fake").unwrap();

        log_resolved_path(&sink, "ffmpeg", &path);
        let logs = sink.take_logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains("resolved:"));
    }

    #[test]
    fn test_log_resolved_path_bare_name() {
        let sink = crate::test_helpers::RecordingSink::new();
        log_resolved_path(&sink, "ffmpeg", Path::new("ffmpeg"));
        let logs = sink.take_logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains("will try PATH"));
    }

    #[test]
    fn test_log_resolved_path_not_yet_exists() {
        let sink = crate::test_helpers::RecordingSink::new();
        log_resolved_path(&sink, "ffmpeg", Path::new("/some/specific/path/ffmpeg"));
        let logs = sink.take_logs();
        assert_eq!(logs.len(), 1);
        assert!(logs[0].contains("does not exist yet"));
    }

    // ── exe_dir ─────────────────────────────────────────────────

    #[test]
    fn test_exe_dir_returns_some() {
        // We're running from a real binary, so exe_dir should work
        let result = exe_dir();
        assert!(result.is_some());
        assert!(result.unwrap().exists());
    }

    // ── is_available (async) ────────────────────────────────────

    #[tokio::test]
    async fn test_is_available_returns_bool() {
        // Just verify it doesn't panic; the result depends on the
        // host environment (ffmpeg may or may not be installed).
        let _ = is_available().await;
    }
}

#[cfg(all(test, feature = "downloader"))]
mod download_tests {
    use super::*;
    use crate::test_helpers::{NoopSink, RecordingSink};

    const HELLO_SHA256: &str = "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824";
    const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

    fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn sha256_verify_accepts_matching_digest() {
        let dir = tempfile::tempdir().unwrap();
        let f = write(dir.path(), "a.bin", b"hello");
        assert!(verify_sha256(&f, HELLO_SHA256).is_ok());
        assert!(verify_sha256(&f, &HELLO_SHA256.to_uppercase()).is_ok());
    }

    #[test]
    fn sha256_verify_reports_expected_and_actual_on_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let f = write(dir.path(), "a.bin", b"hello!");
        let err = verify_sha256(&f, HELLO_SHA256).unwrap_err();
        assert!(err.contains(HELLO_SHA256), "{err}");
        assert!(err.contains(&sha256_file(&f).unwrap()), "{err}");
    }

    #[test]
    fn sha256_verify_errors_on_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(verify_sha256(&dir.path().join("nope"), HELLO_SHA256).is_err());
    }

    #[test]
    fn placeholder_detection() {
        assert!(is_placeholder_sha256(ZEROS));
        assert!(!is_placeholder_sha256(HELLO_SHA256));
        assert!(!is_placeholder_sha256(
            "0000000000000000000000000000000000000000000000000000000000000001"
        ));
        assert!(!is_placeholder_sha256(""));
    }

    #[test]
    fn histv_pins_are_well_formed() {
        for pin in [
            histv_release::SHA256_LINUX_X64,
            histv_release::SHA256_WINDOWS_X64,
            histv_release::SHA256_MACOS_ARM64,
            histv_release::SHA256_MACOS_X64,
        ] {
            assert_eq!(pin.len(), 64);
            assert!(pin
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)));
        }
    }

    #[test]
    fn candidate_list_puts_verified_histv_release_first() {
        let list = download_urls();
        if list.is_empty() {
            return; // platform without a download source
        }
        let first = &list[0];
        assert!(first
            .url
            .contains("/releases/download/ffmpeg-n8.1.3-histv.2/histv-ffmpeg-"));
        assert!(first.sha256.is_some());
        assert!(list[1..].iter().all(|c| c.sha256.is_none()));
    }

    // ── Local HTTP server and archive fixtures (Linux only: uses tar) ──

    #[cfg(target_os = "linux")]
    mod linux {
        use super::*;
        use std::process::Command as StdCommand;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        /// Incompressible filler so the archive clears the 500 kB size check.
        fn filler(len: usize) -> Vec<u8> {
            let mut x: u32 = 0x1234_5678;
            (0..len)
                .map(|_| {
                    x = x.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (x >> 24) as u8
                })
                .collect()
        }

        fn tar(args: &[&str], cwd: &Path) {
            let out = StdCommand::new("tar")
                .args(args)
                .current_dir(cwd)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }

        /// Flat archive: ffmpeg + ffprobe at the root, as the histv release ships.
        fn make_flat_tar_gz(dir: &Path) -> PathBuf {
            let src = dir.join("flat-src");
            std::fs::create_dir_all(&src).unwrap();
            write(&src, "ffmpeg", &filler(600_000));
            write(&src, "ffprobe", b"probe");
            write(&src, "SHA256SUMS", b"x");
            write(&src, "LICENSE.txt", b"x");
            write(&src, "BUILDINFO.txt", b"x");
            let out = dir.join("flat.tar.gz");
            tar(&["czf", out.to_str().unwrap(), "."], &src);
            out
        }

        /// BtbN layout: <dir>/bin/{ffmpeg,ffprobe}, xz compressed.
        fn make_btbn_tar_xz(dir: &Path) -> PathBuf {
            let src = dir.join("btbn-src");
            let bin = src.join("ffmpeg-master-latest-linux64-gpl/bin");
            std::fs::create_dir_all(&bin).unwrap();
            write(&bin, "ffmpeg", &filler(600_000));
            write(&bin, "ffprobe", b"probe");
            let out = dir.join("btbn.tar.xz");
            tar(&["cJf", out.to_str().unwrap(), "."], &src);
            out
        }

        #[test]
        fn extracts_flat_tar_gz() {
            let work = tempfile::tempdir().unwrap();
            let archive = make_flat_tar_gz(work.path());
            let target = tempfile::tempdir().unwrap();
            extract_from_tar(&archive, target.path()).unwrap();
            assert_eq!(
                std::fs::metadata(target.path().join("ffmpeg"))
                    .unwrap()
                    .len(),
                600_000
            );
            assert_eq!(
                std::fs::read(target.path().join("ffprobe")).unwrap(),
                b"probe"
            );
            assert!(!target.path().join("LICENSE.txt").exists());
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(target.path().join("ffmpeg"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111);
        }

        #[test]
        fn extracts_btbn_layout_tar_xz() {
            let work = tempfile::tempdir().unwrap();
            let archive = make_btbn_tar_xz(work.path());
            let target = tempfile::tempdir().unwrap();
            extract_from_tar(&archive, target.path()).unwrap();
            assert!(target.path().join("ffmpeg").is_file());
            assert_eq!(
                std::fs::read(target.path().join("ffprobe")).unwrap(),
                b"probe"
            );
        }

        #[test]
        fn extract_errors_when_no_binaries_present() {
            let work = tempfile::tempdir().unwrap();
            let src = work.path().join("src");
            std::fs::create_dir_all(&src).unwrap();
            write(&src, "README", b"nothing here");
            let archive = work.path().join("empty.tar.gz");
            tar(&["czf", archive.to_str().unwrap(), "."], &src);
            let target = tempfile::tempdir().unwrap();
            assert!(extract_from_tar(&archive, target.path()).is_err());
        }

        /// Serve `routes` (path -> body) on 127.0.0.1; unknown paths get 404.
        /// Returns the base URL and a handle that records requested paths.
        async fn serve(
            routes: Vec<(&'static str, Vec<u8>)>,
        ) -> (String, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let hits = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let hits_task = hits.clone();
            tokio::spawn(async move {
                loop {
                    let Ok((mut sock, _)) = listener.accept().await else {
                        return;
                    };
                    let mut buf = vec![0u8; 4096];
                    let n = sock.read(&mut buf).await.unwrap_or(0);
                    let req = String::from_utf8_lossy(&buf[..n]).to_string();
                    let path = req.split_whitespace().nth(1).unwrap_or("/").to_string();
                    hits_task.lock().unwrap().push(path.clone());
                    let body = routes
                        .iter()
                        .find(|(p, _)| *p == path)
                        .map(|(_, b)| b.clone());
                    let head = match &body {
                        Some(b) => format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                            b.len()
                        ),
                        None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_string(),
                    };
                    let _ = sock.write_all(head.as_bytes()).await;
                    if let Some(b) = body {
                        let _ = sock.write_all(&b).await;
                    }
                    let _ = sock.shutdown().await;
                }
            });
            (base, hits)
        }

        fn client() -> reqwest::Client {
            reqwest::Client::builder().build().unwrap()
        }

        fn cand(base: &str, path: &str, sha: Option<&'static str>) -> Candidate {
            Candidate {
                url: format!("{base}{path}"),
                archive_type: "tar.gz",
                sha256: sha,
            }
        }

        fn leftover_archives(dir: &Path) -> Vec<String> {
            std::fs::read_dir(dir)
                .unwrap()
                .flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .filter(|n| n.starts_with("_histv_dl_"))
                .collect()
        }

        #[tokio::test]
        async fn download_file_verifies_and_extracts_on_match() {
            let work = tempfile::tempdir().unwrap();
            let archive = std::fs::read(make_flat_tar_gz(work.path())).unwrap();
            let digest: &'static str = Box::leak(
                sha256_file(&work.path().join("flat.tar.gz"))
                    .unwrap()
                    .into_boxed_str(),
            );
            let (base, _) = serve(vec![("/a.tar.gz", archive)]).await;
            let target = tempfile::tempdir().unwrap();
            download_file(
                &client(),
                &cand(&base, "/a.tar.gz", Some(digest)),
                target.path(),
                "ffmpeg",
                &NoopSink,
            )
            .await
            .unwrap();
            assert!(target.path().join("ffmpeg").is_file());
            assert!(leftover_archives(target.path()).is_empty());
        }

        #[tokio::test]
        async fn download_file_rejects_mismatch_before_extracting() {
            let work = tempfile::tempdir().unwrap();
            let archive = std::fs::read(make_flat_tar_gz(work.path())).unwrap();
            let actual = sha256_file(&work.path().join("flat.tar.gz")).unwrap();
            let wrong = "1111111111111111111111111111111111111111111111111111111111111111";
            let (base, _) = serve(vec![("/a.tar.gz", archive)]).await;
            let target = tempfile::tempdir().unwrap();
            let err = download_file(
                &client(),
                &cand(&base, "/a.tar.gz", Some(wrong)),
                target.path(),
                "ffmpeg",
                &NoopSink,
            )
            .await
            .unwrap_err();
            assert!(err.contains(wrong) && err.contains(&actual), "{err}");
            assert!(!target.path().join("ffmpeg").exists());
            assert!(leftover_archives(target.path()).is_empty());
        }

        #[tokio::test]
        async fn download_file_removes_archive_when_extraction_fails() {
            // Large enough to pass the size check, but not a tar archive.
            let (base, _) = serve(vec![("/a.tar.gz", vec![0xA5u8; 700_000])]).await;
            let target = tempfile::tempdir().unwrap();
            let err = download_file(
                &client(),
                &cand(&base, "/a.tar.gz", None),
                target.path(),
                "ffmpeg",
                &NoopSink,
            )
            .await
            .unwrap_err();
            assert!(err.contains("Extraction failed"), "{err}");
            assert!(!target.path().join("ffmpeg").exists());
            assert_eq!(leftover_archives(target.path()), Vec::<String>::new());
        }

        #[tokio::test]
        async fn candidates_skip_placeholder_then_fall_back_past_mismatch_and_404() {
            let work = tempfile::tempdir().unwrap();
            let archive = std::fs::read(make_flat_tar_gz(work.path())).unwrap();
            let wrong = "1111111111111111111111111111111111111111111111111111111111111111";
            let (base, hits) = serve(vec![
                ("/placeholder.tar.gz", archive.clone()),
                ("/tampered.tar.gz", archive.clone()),
                ("/fallback.tar.gz", archive),
            ])
            .await;
            let list = vec![
                cand(&base, "/placeholder.tar.gz", Some(ZEROS)),
                cand(&base, "/tampered.tar.gz", Some(wrong)),
                cand(&base, "/missing.tar.gz", None),
                cand(&base, "/fallback.tar.gz", None),
            ];
            let target = tempfile::tempdir().unwrap();
            let sink = RecordingSink::new();
            download_from_candidates(&client(), &list, target.path(), "ffmpeg", &sink)
                .await
                .unwrap();
            assert!(target.path().join("ffmpeg").is_file());
            // The placeholder was never requested; the rest were tried in order.
            assert_eq!(
                *hits.lock().unwrap(),
                vec!["/tampered.tar.gz", "/missing.tar.gz", "/fallback.tar.gz"]
            );
            let logs = sink.take_logs().join("\n");
            assert!(
                logs.contains("Skipping") && logs.contains("/placeholder.tar.gz"),
                "{logs}"
            );
        }

        #[tokio::test]
        async fn candidates_stop_at_first_verified_success() {
            let work = tempfile::tempdir().unwrap();
            let archive = std::fs::read(make_flat_tar_gz(work.path())).unwrap();
            let digest: &'static str = Box::leak(
                sha256_file(&work.path().join("flat.tar.gz"))
                    .unwrap()
                    .into_boxed_str(),
            );
            let (base, hits) =
                serve(vec![("/a.tar.gz", archive.clone()), ("/b.tar.gz", archive)]).await;
            let list = vec![
                cand(&base, "/a.tar.gz", Some(digest)),
                cand(&base, "/b.tar.gz", None),
            ];
            let target = tempfile::tempdir().unwrap();
            download_from_candidates(&client(), &list, target.path(), "ffmpeg", &NoopSink)
                .await
                .unwrap();
            assert_eq!(*hits.lock().unwrap(), vec!["/a.tar.gz"]);
        }

        /// Network test: downloads the published histv release (about 40 MB).
        /// Run with `cargo test --lib -- --ignored live_download`.
        #[cfg(target_arch = "x86_64")]
        #[tokio::test]
        #[ignore = "network: downloads the published ffmpeg release"]
        async fn live_download_uses_pinned_histv_release() {
            use std::os::unix::fs::PermissionsExt;
            let target = tempfile::tempdir().unwrap();
            let sink = RecordingSink::new();
            download_to_dir(target.path(), &sink).await.unwrap();

            // A fallback attempt or a skipped pin would leave a log line.
            let logs = sink.take_logs();
            assert!(
                !logs
                    .iter()
                    .any(|l| l.contains("Mirror") || l.contains("Skipping")),
                "histv release was not used on the first attempt: {logs:?}"
            );
            assert!(leftover_archives(target.path()).is_empty());

            for name in ["ffmpeg", "ffprobe"] {
                let path = target.path().join(name);
                let mode = std::fs::metadata(&path).unwrap().permissions().mode();
                assert_eq!(mode & 0o111, 0o111, "{name} is not executable");
            }
            let out = StdCommand::new(target.path().join("ffmpeg"))
                .arg("-version")
                .output()
                .unwrap();
            let version = String::from_utf8_lossy(&out.stdout);
            assert!(out.status.success(), "{version}");
            assert!(version.contains("n8.1.3"), "{version}");
        }

        #[tokio::test]
        async fn candidates_error_when_every_candidate_fails() {
            let (base, _) = serve(vec![]).await;
            let list = vec![
                cand(&base, "/x.tar.gz", None),
                cand(&base, "/y.tar.gz", Some(ZEROS)),
            ];
            let target = tempfile::tempdir().unwrap();
            let err =
                download_from_candidates(&client(), &list, target.path(), "ffmpeg", &NoopSink)
                    .await
                    .unwrap_err();
            assert!(err.contains("404"), "{err}");
        }
    }
}
