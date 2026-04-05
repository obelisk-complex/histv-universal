//! Shared helpers for integration tests.

use std::path::PathBuf;
use std::process::Stdio;

/// Check whether ffmpeg is available on the system PATH.
/// Integration tests that require ffmpeg should call this at the top
/// and return early if it returns false.
#[allow(dead_code)]
pub fn require_ffmpeg() -> bool {
    std::process::Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Generate a synthetic test video using ffmpeg's built-in test source.
/// Returns the path to the generated file. The caller is responsible for
/// cleanup (use a tempdir).
///
/// Returns `None` if ffmpeg is unavailable or the generation fails.
#[allow(dead_code)]
pub fn create_test_source(dir: &std::path::Path, duration_secs: u32) -> Option<PathBuf> {
    let output = dir.join("test_source.mkv");
    let status = std::process::Command::new("ffmpeg")
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            &format!("testsrc2=d={duration_secs}:r=25:s=320x240"),
            "-f",
            "lavfi",
            "-i",
            &format!("sine=f=440:d={duration_secs}:r=44100"),
            "-c:v",
            "libx264",
            "-preset",
            "ultrafast",
            "-crf",
            "28",
            "-c:a",
            "aac",
            "-b:a",
            "64k",
        ])
        .arg(&output)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;

    if status.success() && output.exists() {
        Some(output)
    } else {
        None
    }
}
