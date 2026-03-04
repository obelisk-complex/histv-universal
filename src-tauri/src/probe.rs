use serde::{Deserialize, Serialize};

use crate::ffmpeg;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProbeResult {
    pub video_codec: String,
    pub video_width: u32,
    pub video_height: u32,
    pub video_bitrate_bps: f64,
    pub video_bitrate_mbps: f64,
    pub is_hdr: bool,
    pub color_transfer: String,
}

/// Run ffprobe with the given arguments and return trimmed stdout.
async fn run_ffprobe(args: &[&str]) -> Result<String, String> {
    let output = ffmpeg::ffprobe_command()
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| format!("Failed to spawn ffprobe: {e}"))?
        .wait_with_output()
        .await
        .map_err(|e| format!("ffprobe wait failed: {e}"))?;

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// Three-tier bitrate detection strategy (§5.6):
/// 1. Stream header bit_rate
/// 2. MKV stream tags (NUMBER_OF_BYTES / DURATION)
/// 3. Packet counting fallback
async fn detect_bitrate(file_path: &str) -> f64 {
    // Tier 1: stream header
    if let Ok(raw) = run_ffprobe(&[
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "stream=bit_rate",
        "-of", "default=noprint_wrappers=1:nokey=1",
        file_path,
    ])
    .await
    {
        if let Ok(bps) = raw.parse::<f64>() {
            if bps > 0.0 {
                return bps;
            }
        }
    }

    // Tier 2: MKV stream tags
    if let Ok(tag_info) = run_ffprobe(&[
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "stream_tags=NUMBER_OF_BYTES,DURATION",
        "-of", "default=noprint_wrappers=1:nokey=1",
        file_path,
    ])
    .await
    {
        let lines: Vec<&str> = tag_info.lines().filter(|l| !l.trim().is_empty()).collect();
        let tag_bytes: Option<f64> = lines.iter().find_map(|l| l.trim().parse::<f64>().ok());
        let tag_dur: Option<f64> = lines.iter().find_map(|l| parse_duration_tag(l.trim()));
        if let (Some(bytes), Some(dur)) = (tag_bytes, tag_dur) {
            if dur > 0.0 {
                return (bytes * 8.0) / dur;
            }
        }
    }

    // Tier 3: packet counting fallback
    if let Ok(raw) = run_ffprobe(&[
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "stream=bit_rate",
        "-read_intervals", "%+#99999999",
        "-of", "default=noprint_wrappers=1:nokey=1",
        file_path,
    ])
    .await
    {
        if let Ok(bps) = raw.parse::<f64>() {
            if bps > 0.0 {
                return bps;
            }
        }
    }

    0.0
}

/// Parse a duration tag like "01:23:45.678" into seconds.
fn parse_duration_tag(s: &str) -> Option<f64> {
    // Must contain colons to be a time format
    if !s.contains(':') {
        return None;
    }
    let parts: Vec<&str> = s.split(|c| c == ':' || c == '.').collect();
    if parts.len() < 3 {
        return None;
    }
    let h: f64 = parts[0].parse().ok()?;
    let m: f64 = parts[1].parse().ok()?;
    let sec: f64 = parts[2].parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Full file probe: codec, dimensions, bitrate, HDR.
pub async fn probe_file(file_path: &str) -> Result<ProbeResult, String> {
    // Probe codec and dimensions
    let video_info = run_ffprobe(&[
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "stream=codec_name,width,height",
        "-of", "csv=p=0",
        file_path,
    ])
    .await
    .unwrap_or_default();

    let mut video_codec = String::new();
    let mut video_width: u32 = 0;
    let mut video_height: u32 = 0;

    if !video_info.is_empty() && video_info != "N/A" {
        let parts: Vec<&str> = video_info.split(',').collect();
        if !parts.is_empty() {
            video_codec = parts[0].trim().to_string();
        }
        if parts.len() >= 2 {
            video_width = parts[1].trim().parse().unwrap_or(0);
        }
        if parts.len() >= 3 {
            video_height = parts[2].trim().parse().unwrap_or(0);
        }
    }

    // Probe bitrate (three-tier)
    let video_bitrate_bps = detect_bitrate(file_path).await;
    let video_bitrate_mbps = video_bitrate_bps / 1_000_000.0;

    // Probe HDR colour metadata
    let colour_info = run_ffprobe(&[
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "stream=color_transfer,color_primaries,color_space",
        "-of", "default=noprint_wrappers=1:nokey=1",
        file_path,
    ])
    .await
    .unwrap_or_default();

    let mut is_hdr = false;
    let mut color_transfer = String::new();
    for line in colour_info.lines() {
        let trimmed = line.trim();
        if trimmed == "smpte2084" || trimmed == "arib-std-b67" {
            is_hdr = true;
            color_transfer = trimmed.to_string();
            break;
        }
    }

    Ok(ProbeResult {
        video_codec,
        video_width,
        video_height,
        video_bitrate_bps,
        video_bitrate_mbps,
        is_hdr,
        color_transfer,
    })
}
