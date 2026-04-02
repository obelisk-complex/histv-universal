//! Dolby Vision and HDR10+ tool discovery and capability reporting.
//!
//! Mirrors the `ffmpeg.rs` pattern: discover MP4Box at startup, cache its
//! path, offer download if absent. The `dolby_vision` and `hdr10plus`
//! crates are compiled in and always available.

use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::events::EventSink;

// ── Cached MP4Box path and version ────────────────────────────────

static MP4BOX_PATH: RwLock<Option<PathBuf>> = RwLock::new(None);
/// Whether the discovered MP4Box supports the `:dvp=` DV profile syntax (GPAC >= 2.2).
/// Set during init() after running `MP4Box -version`.
static MP4BOX_DVP_OK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "windows")]
const EXE_EXT: &str = ".exe";
#[cfg(not(target_os = "windows"))]
const EXE_EXT: &str = "";

/// Reports which DV/HDR10+ capabilities are available.
pub struct DoviCapabilities {
    /// dolby_vision crate is compiled in (always true when `dovi` feature is enabled).
    pub can_process_dovi: bool,
    /// hdr10plus crate is compiled in (always true when `dovi` feature is enabled).
    pub can_process_hdr10plus: bool,
    /// MP4Box binary found AND supports `:dvp=` syntax (GPAC >= 2.2).
    /// Required for Tier 1 (full DV preservation).
    pub can_package_dovi_mp4: bool,
}

/// Query current capabilities based on compiled features and discovered tools.
pub fn capabilities() -> DoviCapabilities {
    let has_mp4box = MP4BOX_PATH
        .read()
        .ok()
        .and_then(|r| r.as_ref().map(|p| p.exists()))
        .unwrap_or(false);
    let dvp_ok = MP4BOX_DVP_OK.load(std::sync::atomic::Ordering::Relaxed);

    DoviCapabilities {
        can_process_dovi: cfg!(feature = "dovi"),
        can_process_hdr10plus: cfg!(feature = "dovi"),
        can_package_dovi_mp4: has_mp4box && dvp_ok,
    }
}

// ── Init / resolve ────────────────────────────────────────────────

/// Resolve and cache the MP4Box path. Safe to call multiple times
/// (e.g. after downloading MP4Box). Called once at startup and again
/// after download — concurrent calls are safe (RwLock + AtomicBool)
/// but not expected in normal operation.
pub fn init(resource_dir: Option<&Path>, sink: &dyn EventSink) {
    let name = format!("MP4Box{EXE_EXT}");
    let path = resolve_mp4box(resource_dir, &name);
    // resolve_mp4box returns a path that .exists() (or a bare name for PATH lookup).
    // Cache the result to avoid redundant stat syscalls.
    let found = path.exists();
    let is_bare = path.components().count() == 1;
    log_resolved(sink, &path, found);

    let dvp_ok = if found || is_bare {
        check_mp4box_version(&path, sink)
    } else {
        false
    };
    MP4BOX_DVP_OK.store(dvp_ok, std::sync::atomic::Ordering::Relaxed);

    if let Ok(mut w) = MP4BOX_PATH.write() {
        *w = Some(path);
    }
}

/// Re-resolve after a download. Delegates to `init`.
pub fn reinit(resource_dir: Option<&Path>, sink: &dyn EventSink) {
    init(resource_dir, sink);
}

fn log_resolved(sink: &dyn EventSink, path: &Path, found: bool) {
    if found {
        sink.log(&format!("[dovi] MP4Box resolved: {}", path.display()));
    } else if path.components().count() == 1 {
        sink.log("[dovi] MP4Box not found in known locations, will try PATH");
    } else {
        sink.log(&format!("[dovi] MP4Box not found: {}", path.display()));
    }
}

/// Run `MP4Box -version` and check the output for DV syntax support.
fn check_mp4box_version(path: &Path, sink: &dyn EventSink) -> bool {
    let output = std::process::Command::new(path)
        .arg("-version")
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output();

    match output {
        Ok(o) if o.status.success() => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr),
            );
            let ok = mp4box_supports_dvp(&combined);
            if ok {
                if let Some(major) = parse_mp4box_version(&combined) {
                    sink.log(&format!(
                        "[dovi] MP4Box GPAC version {}.x — DV packaging supported",
                        major
                    ));
                }
            } else {
                sink.log("[dovi] WARNING: MP4Box version too old for DV packaging (need GPAC >= 2.2). DV files will fall back to HDR10.");
            }
            ok
        }
        _ => {
            sink.log("[dovi] WARNING: Could not determine MP4Box version");
            false
        }
    }
}

/// Check if MP4Box is actually usable (can run `MP4Box -version`).
pub async fn is_mp4box_available() -> bool {
    let path = match MP4BOX_PATH.read().ok().and_then(|r| r.clone()) {
        Some(p) => p,
        None => return false,
    };

    tokio::process::Command::new(&path)
        .arg("-version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .await
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Parse the GPAC major version from `MP4Box -version` output.
///
/// Returns the major version number: `26` for "26.02", `2` for "2.2.0".
/// The `:dvp=` DV profile syntax requires GPAC >= 2.2. Older versions
/// used `:dv-profile=` which we don't support. Returns None if the
/// version cannot be determined.
pub fn parse_mp4box_version(version_output: &str) -> Option<u32> {
    // Format: "MP4Box - GPAC version 26.02-rev..." or "GPAC version 2.2.0-..."
    let marker = "GPAC version ";
    let ver_start = version_output.find(marker)? + marker.len();
    let ver_str = &version_output[ver_start..];
    // Take the first numeric segment (before '.' or '-')
    let major_str: String = ver_str.chars().take_while(|c| c.is_ascii_digit()).collect();
    major_str.parse().ok()
}

/// Check if the installed MP4Box supports the `:dvp=` DV profile syntax.
///
/// GPAC versions use either `MAJOR.MINOR` (old: 2.0, 2.2) or `YY.MM`
/// (new: 23.06, 24.02, 26.02). The `:dvp=` syntax was introduced in
/// GPAC 2.2. Since YY.MM versions have major >= 23, any major >= 3
/// is guaranteed to support it.
pub fn mp4box_supports_dvp(version_output: &str) -> bool {
    match parse_mp4box_version(version_output) {
        Some(major) if major >= 3 => true, // YY.MM format (23+) or old 3.x+
        Some(2) => {
            // Old format: need to check minor >= 2
            let marker = "GPAC version 2.";
            if let Some(pos) = version_output.find(marker) {
                let after = &version_output[pos + marker.len()..];
                let minor: String = after.chars().take_while(|c| c.is_ascii_digit()).collect();
                minor.parse::<u32>().map(|m| m >= 2).unwrap_or(false)
            } else {
                false
            }
        }
        _ => false, // Version 0, 1, or unparseable — too old
    }
}

/// Return a configured Command for MP4Box.
pub fn mp4box_command() -> tokio::process::Command {
    let path = MP4BOX_PATH
        .read()
        .ok()
        .and_then(|r| r.clone())
        .unwrap_or_else(|| PathBuf::from(format!("MP4Box{EXE_EXT}")));

    #[allow(unused_mut)]
    let mut cmd = tokio::process::Command::new(&path);
    #[cfg(target_os = "windows")]
    crate::ffmpeg::hide_window(&mut cmd);
    cmd
}

// ── Binary resolution ─────────────────────────────────────────────

fn resolve_mp4box(resource_dir: Option<&Path>, name: &str) -> PathBuf {
    // 1. Tauri resource/sidecar directory
    if let Some(dir) = resource_dir {
        let p = dir.join(name);
        if p.exists() {
            return p;
        }
    }

    // 2. Same directory as the running executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join(name);
            if p.exists() {
                return p;
            }
        }
    }

    // 3. App-data bin directory (where ffmpeg downloads go too)
    if let Some(dir) = crate::ffmpeg::app_data_bin_dir() {
        let p = dir.join(name);
        if p.exists() {
            return p;
        }
    }

    // 4. Well-known platform directories
    #[cfg(target_os = "linux")]
    {
        for dir in &["/usr/bin", "/usr/local/bin", "/snap/bin"] {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                return p;
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        for dir in &["/opt/homebrew/bin", "/usr/local/bin"] {
            let p = PathBuf::from(dir).join(name);
            if p.exists() {
                return p;
            }
        }
    }
    #[cfg(target_os = "windows")]
    {
        // Check common install locations
        if let Ok(pf) = std::env::var("ProgramFiles") {
            let p = PathBuf::from(&pf).join("GPAC").join(name);
            if p.exists() {
                return p;
            }
        }
    }

    // 5. Bare name fallback (OS PATH lookup at spawn time)
    PathBuf::from(name)
}

// ── Download ──────────────────────────────────────────────────────

/// GPAC stable release download URLs and SHA-256 checksums.
/// Linux: .deb package (extract MP4Box from it)
/// Windows: .exe installer (extract via 7z)
/// macOS: .pkg (extract via pkgutil)
#[cfg(feature = "downloader")]
struct GpacDownload {
    url: &'static str,
    sha256: &'static str,
}

#[cfg(feature = "downloader")]
fn mp4box_download_info() -> Option<GpacDownload> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        Some(GpacDownload {
        url: "https://download.tsi.telecom-paristech.fr/gpac/release/26.02/gpac_26.02-rev0-g118e60a9-master_amd64.deb",
        sha256: "48b6ec3a04f8b4fb8b933885d11706c149f5f9c3c3c00b3df550dafd6a19d09e",
    })
    }
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    {
        Some(GpacDownload {
        url: "https://download.tsi.telecom-paristech.fr/gpac/release/26.02/gpac-26.02-rev0-g118e60a9-master-x64.exe",
        sha256: "", // TODO: compute and pin when Windows build is tested
    })
    }
    #[cfg(all(
        target_os = "macos",
        any(target_arch = "x86_64", target_arch = "aarch64")
    ))]
    {
        Some(GpacDownload {
        url: "https://download.tsi.telecom-paristech.fr/gpac/release/26.02/gpac-26.02-rev0-g118e60a9-master.pkg",
        sha256: "", // TODO: compute and pin when macOS build is tested
    })
    }
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64"),
        all(
            target_os = "macos",
            any(target_arch = "x86_64", target_arch = "aarch64")
        ),
    )))]
    {
        None
    }
}

/// Download MP4Box to the app-data bin directory.
#[cfg(feature = "downloader")]
pub async fn download_mp4box(sink: &dyn EventSink) -> Result<(), String> {
    let info = mp4box_download_info().ok_or_else(|| {
        "Automatic MP4Box download is not available for this platform.".to_string()
    })?;
    let url = info.url;

    let target_dir = crate::ffmpeg::app_data_bin_dir()
        .ok_or_else(|| "Could not determine app data directory".to_string())?;

    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("Could not create directory {}: {e}", target_dir.display()))?;

    sink.log(&format!("[dovi] Downloading GPAC (MP4Box) from {url}..."));
    sink.ffmpeg_download_progress("Downloading MP4Box...");

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::limited(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {e}"))?;

    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| format!("Download failed: {e}"))?;

    if !response.status().is_success() {
        return Err(format!("Download failed: HTTP {}", response.status()));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read download: {e}"))?;

    // Verify SHA-256 checksum if one is pinned for this platform
    {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let actual = format!("{:x}", hasher.finalize());
        if !info.sha256.is_empty() {
            if actual != info.sha256 {
                return Err(format!(
                    "Checksum mismatch: expected {}, got {}. Download may be corrupted or tampered with.",
                    info.sha256, actual,
                ));
            }
            sink.log("[dovi] SHA-256 checksum verified");
        } else {
            sink.log(&format!(
                "[dovi] WARNING: No pinned checksum for this platform (SHA-256: {}). \
                 Pin this hash in dovi_tools.rs for verified downloads.",
                actual,
            ));
        }
    }

    sink.ffmpeg_download_progress("Extracting MP4Box...");

    #[cfg(target_os = "linux")]
    extract_from_deb(&bytes, &target_dir, sink)?;

    #[cfg(target_os = "macos")]
    extract_from_pkg(&bytes, &target_dir, sink)?;

    #[cfg(target_os = "windows")]
    extract_from_exe(&bytes, &target_dir, sink)?;

    // Verify the binary exists
    let mp4box_path = target_dir.join(format!("MP4Box{EXE_EXT}"));
    if !mp4box_path.exists() {
        return Err("Extraction completed but MP4Box not found".to_string());
    }

    sink.ffmpeg_download_progress("MP4Box ready!");
    sink.log(&format!(
        "[dovi] MP4Box installed to {}",
        mp4box_path.display()
    ));

    Ok(())
}

/// Extract MP4Box from a .deb package.
///
/// GPAC's MP4Box is dynamically linked against libgpac and other system libs,
/// so standalone extraction doesn't work reliably. Strategy:
/// 1. Try `dpkg -i` (needs root/sudo) for a proper system install
/// 2. Fall back to `dpkg-deb -x` + copy binary + libs to target_dir
///    with a wrapper script that sets LD_LIBRARY_PATH
#[cfg(all(feature = "downloader", target_os = "linux"))]
fn extract_from_deb(
    deb_bytes: &[u8],
    target_dir: &Path,
    sink: &dyn EventSink,
) -> Result<(), String> {
    let tmp_dir = target_dir.join("_gpac_extract_tmp");
    let _ = std::fs::create_dir_all(&tmp_dir);

    let deb_path = tmp_dir.join("gpac.deb");
    std::fs::write(&deb_path, deb_bytes).map_err(|e| format!("Failed to write .deb: {e}"))?;

    // Strategy 1: try dpkg -i for a full system install
    // (This puts MP4Box in /usr/bin where our resolver will find it)
    sink.log("[dovi] Attempting system install via dpkg...");
    let dpkg_result = std::process::Command::new("dpkg")
        .args(["-i", &deb_path.to_string_lossy()])
        .output();

    if let Ok(o) = dpkg_result {
        if o.status.success() {
            // dpkg succeeded - fix any missing deps if dpkg flagged them
            let stderr = String::from_utf8_lossy(&o.stderr);
            if stderr.contains("dependency problems") {
                let _ = std::process::Command::new("apt-get")
                    .args(["install", "-f", "-y"])
                    .output();
            }
            let _ = std::fs::remove_dir_all(&tmp_dir);
            sink.log("[dovi] GPAC installed system-wide via dpkg");
            return Ok(());
        }
    }

    // Strategy 2: extract the .deb contents and copy MP4Box + libgpac
    sink.log("[dovi] System install failed (no root?), extracting locally...");
    let deb_extract = tmp_dir.join("deb_contents");
    let _ = std::fs::create_dir_all(&deb_extract);

    let output = std::process::Command::new("dpkg-deb")
        .args([
            "-x",
            &deb_path.to_string_lossy(),
            &deb_extract.to_string_lossy(),
        ])
        .output();

    let extracted = match output {
        Ok(o) if o.status.success() => true,
        _ => {
            sink.log("[dovi] dpkg-deb not available, trying ar + tar...");
            extract_deb_manual(&deb_path, &deb_extract)?
        }
    };

    if !extracted {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("Could not extract .deb package".to_string());
    }

    // Copy the MP4Box binary as MP4Box.bin (the wrapper becomes "MP4Box")
    if let Some(src) = find_binary_recursive(&deb_extract, "MP4Box") {
        let real_dst = target_dir.join("MP4Box.bin");
        if std::fs::rename(&src, &real_dst).is_err() {
            std::fs::copy(&src, &real_dst).map_err(|e| format!("Could not copy MP4Box: {e}"))?;
        }
        set_executable_mode(&real_dst);
    } else {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("MP4Box binary not found in GPAC .deb package".to_string());
    }

    // Copy libgpac shared library next to MP4Box
    let lib_dir = target_dir.join("lib");
    let _ = std::fs::create_dir_all(&lib_dir);
    if let Some(libgpac) = find_file_recursive(&deb_extract, "libgpac.so") {
        let _ = std::fs::copy(&libgpac, lib_dir.join(libgpac.file_name().unwrap()));
        let soname = libgpac.file_name().unwrap().to_string_lossy();
        if soname.contains(".so.") {
            let base = soname.split('.').take(3).collect::<Vec<_>>().join(".");
            let _ = std::os::unix::fs::symlink(libgpac.file_name().unwrap(), lib_dir.join(&base));
        }
    }

    // Create a wrapper script as "MP4Box" so the resolver finds it.
    // The wrapper sets LD_LIBRARY_PATH so libgpac is found at runtime.
    let wrapper_path = target_dir.join("MP4Box");
    let wrapper_content = format!(
        "#!/bin/sh\nLD_LIBRARY_PATH=\"{}:$LD_LIBRARY_PATH\" exec \"{}\" \"$@\"\n",
        lib_dir.display(),
        target_dir.join("MP4Box.bin").display(),
    );
    std::fs::write(&wrapper_path, &wrapper_content)
        .map_err(|e| format!("Could not write wrapper script: {e}"))?;
    set_executable_mode(&wrapper_path);

    let _ = std::fs::remove_dir_all(&tmp_dir);
    sink.log("[dovi] MP4Box extracted locally with wrapper script");
    Ok(())
}

/// Manual .deb extraction using ar + tar (fallback when dpkg-deb is not available).
#[cfg(all(feature = "downloader", target_os = "linux"))]
fn extract_deb_manual(deb_path: &Path, dest: &Path) -> Result<bool, String> {
    let tmp = dest.parent().unwrap_or(dest).join("_ar_tmp");
    let _ = std::fs::create_dir_all(&tmp);

    let ar_out = std::process::Command::new("ar")
        .args(["x", &deb_path.to_string_lossy()])
        .current_dir(&tmp)
        .output()
        .map_err(|e| format!("ar failed: {e}"))?;

    if !ar_out.status.success() {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err("ar extraction failed".to_string());
    }

    if let Ok(entries) = std::fs::read_dir(&tmp) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with("data.tar") {
                let tar_out = std::process::Command::new("tar")
                    .args([
                        "xf",
                        &entry.path().to_string_lossy(),
                        "-C",
                        &dest.to_string_lossy(),
                    ])
                    .output()
                    .map_err(|e| format!("tar failed: {e}"))?;

                let _ = std::fs::remove_dir_all(&tmp);
                return Ok(tar_out.status.success());
            }
        }
    }

    let _ = std::fs::remove_dir_all(&tmp);
    Err("No data.tar.* found in .deb".to_string())
}

#[cfg(all(feature = "downloader", target_os = "linux"))]
fn set_executable_mode(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    if let Ok(meta) = std::fs::metadata(path) {
        let mut perms = meta.permissions();
        perms.set_mode(0o755);
        let _ = std::fs::set_permissions(path, perms);
    }
}

/// Recursively find a file whose name starts with the given prefix.
#[cfg(all(feature = "downloader", target_os = "linux"))]
fn find_file_recursive(dir: &Path, prefix: &str) -> Option<PathBuf> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() {
                if let Some(name) = p.file_name() {
                    if name.to_string_lossy().starts_with(prefix) {
                        return Some(p);
                    }
                }
            }
            if p.is_dir() {
                if let Some(found) = find_file_recursive(&p, prefix) {
                    return Some(found);
                }
            }
        }
    }
    None
}

/// Extract MP4Box from a macOS .pkg.
#[cfg(all(feature = "downloader", target_os = "macos"))]
fn extract_from_pkg(
    pkg_bytes: &[u8],
    target_dir: &Path,
    _sink: &dyn EventSink,
) -> Result<(), String> {
    let tmp_dir = target_dir.join("_gpac_extract_tmp");
    let _ = std::fs::create_dir_all(&tmp_dir);

    let pkg_path = tmp_dir.join("gpac.pkg");
    std::fs::write(&pkg_path, pkg_bytes).map_err(|e| format!("Failed to write .pkg: {e}"))?;

    let expand_dir = tmp_dir.join("expanded");
    let _ = std::fs::create_dir_all(&expand_dir);

    // pkgutil --expand to unpack the .pkg
    let output = std::process::Command::new("pkgutil")
        .args([
            "--expand-full",
            &pkg_path.to_string_lossy(),
            &expand_dir.to_string_lossy(),
        ])
        .output()
        .map_err(|e| format!("pkgutil failed: {e}"))?;

    if !output.status.success() {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("pkgutil extraction failed".to_string());
    }

    if let Some(src) = find_binary_recursive(&expand_dir, "MP4Box") {
        let dst = target_dir.join("MP4Box");
        if std::fs::rename(&src, &dst).is_err() {
            std::fs::copy(&src, &dst).map_err(|e| format!("Could not copy MP4Box: {e}"))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if let Ok(meta) = std::fs::metadata(&dst) {
                let mut perms = meta.permissions();
                perms.set_mode(0o755);
                let _ = std::fs::set_permissions(&dst, perms);
            }
        }
    } else {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("MP4Box not found in GPAC .pkg".to_string());
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(())
}

/// Extract MP4Box from a Windows installer.
/// The GPAC Windows installer is NSIS-based. We use 7z to extract if available,
/// otherwise prompt the user to install GPAC manually.
#[cfg(all(feature = "downloader", target_os = "windows"))]
fn extract_from_exe(
    exe_bytes: &[u8],
    target_dir: &Path,
    _sink: &dyn EventSink,
) -> Result<(), String> {
    let tmp_dir = target_dir.join("_gpac_extract_tmp");
    let _ = std::fs::create_dir_all(&tmp_dir);

    let exe_path = tmp_dir.join("gpac_installer.exe");
    std::fs::write(&exe_path, exe_bytes).map_err(|e| format!("Failed to write installer: {e}"))?;

    // Try 7z extraction (NSIS installers can be unpacked with 7z)
    let output = std::process::Command::new("7z")
        .args([
            "x",
            &exe_path.to_string_lossy(),
            &format!("-o{}", tmp_dir.display()),
            "-y",
        ])
        .output();

    match output {
        Ok(o) if o.status.success() => {}
        _ => {
            let _ = std::fs::remove_dir_all(&tmp_dir);
            return Err(
                "Could not extract GPAC installer. Please install GPAC manually from gpac.io"
                    .to_string(),
            );
        }
    }

    let mp4box_name = "MP4Box.exe";
    if let Some(src) = find_binary_recursive(&tmp_dir, mp4box_name) {
        let dst = target_dir.join(mp4box_name);
        if std::fs::rename(&src, &dst).is_err() {
            std::fs::copy(&src, &dst).map_err(|e| format!("Could not copy MP4Box: {e}"))?;
        }
    } else {
        let _ = std::fs::remove_dir_all(&tmp_dir);
        return Err("MP4Box.exe not found in GPAC installer".to_string());
    }

    let _ = std::fs::remove_dir_all(&tmp_dir);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_version_yymm_format() {
        let output = "MP4Box - GPAC version 26.02-rev0-g118e60a9-master";
        assert_eq!(parse_mp4box_version(output), Some(26));
    }

    #[test]
    fn parse_version_old_format() {
        let output = "MP4Box - GPAC version 2.2.0-rev123";
        assert_eq!(parse_mp4box_version(output), Some(2));
    }

    #[test]
    fn parse_version_garbage() {
        assert_eq!(parse_mp4box_version("no version here"), None);
        assert_eq!(parse_mp4box_version(""), None);
    }

    #[test]
    fn dvp_supported_yymm() {
        assert!(mp4box_supports_dvp("GPAC version 26.02-rev0"));
        assert!(mp4box_supports_dvp("GPAC version 23.06-rev0"));
        assert!(mp4box_supports_dvp("GPAC version 24.02-rev0"));
    }

    #[test]
    fn dvp_supported_old_22() {
        assert!(mp4box_supports_dvp("GPAC version 2.2.0-rev0"));
        assert!(mp4box_supports_dvp("GPAC version 2.3.0-rev0"));
    }

    #[test]
    fn dvp_not_supported_old() {
        assert!(!mp4box_supports_dvp("GPAC version 2.0.0-rev0"));
        assert!(!mp4box_supports_dvp("GPAC version 2.1.0-rev0"));
        assert!(!mp4box_supports_dvp("GPAC version 1.0.0-rev0"));
    }

    #[test]
    fn dvp_not_supported_unparseable() {
        assert!(!mp4box_supports_dvp(""));
        assert!(!mp4box_supports_dvp("not a version string"));
    }
}

/// Recursively find a binary by name in a directory tree.
#[cfg(feature = "downloader")]
fn find_binary_recursive(dir: &Path, name: &str) -> Option<PathBuf> {
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let p = entry.path();
            if p.is_file() && p.file_name().map(|n| n == name).unwrap_or(false) {
                return Some(p);
            }
            if p.is_dir() {
                if let Some(found) = find_binary_recursive(&p, name) {
                    return Some(found);
                }
            }
        }
    }
    None
}
