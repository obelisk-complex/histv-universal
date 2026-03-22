use serde::{Deserialize, Serialize};

use crate::events::EventSink;
use crate::ffmpeg;
use crate::queue::AudioStreamInfo;

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
    pub audio_streams: Vec<AudioStreamInfo>,
    pub duration_secs: f64,
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

/// Public ffprobe runner for use by other modules (e.g. mkv_tags deep repair).
pub async fn run_ffprobe_public(args: &[&str]) -> Result<String, String> {
    run_ffprobe(args).await
}

/// Parse a duration tag like "01:23:45.678" into seconds (#10).
/// Splits on ':' only so that the seconds part retains its fractional
/// component (e.g. "45.678" parses as 45.678 via f64::parse).
fn parse_duration_tag(s: &str) -> Option<f64> {
    if !s.contains(':') {
        return None;
    }
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 3 {
        return None;
    }
    let h: f64 = parts[0].parse().ok()?;
    let m: f64 = parts[1].parse().ok()?;
    let sec: f64 = parts[2].parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Attempt to parse a non-empty, non-"N/A" numeric string.
fn parse_numeric(s: &str) -> Option<f64> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed == "N/A" {
        return None;
    }
    trimmed.parse::<f64>().ok().filter(|v| *v > 0.0)
}

/// Full file probe: codec, dimensions, bitrate, HDR, and audio streams -
/// all gathered in a single ffprobe invocation where possible.
///
/// Previously this spawned 3-6 separate ffprobe processes per file.
/// Now the primary call requests all video/audio stream data plus format
/// metadata in one go. Only the packet-counting bitrate fallback (tier 4)
/// requires a second invocation, and that's rare.
pub async fn probe_file(file_path: &str, sink: &dyn EventSink) -> Result<ProbeResult, String> {
    // ── Single-pass probe: all streams + format in one call ──
    let json_raw = run_ffprobe(&[
        "-v", "error",
        "-show_streams",
        "-show_format",
        "-of", "json",
        file_path,
    ])
    .await
    .unwrap_or_default();

    let json: serde_json::Value =
        serde_json::from_str(&json_raw).unwrap_or(serde_json::Value::Null);

    let streams = json["streams"].as_array();
    let format = &json["format"];

    // ── Single-pass stream extraction (#9) ──
    // Extract both video and audio stream info in one iteration.
    let mut video_codec = String::new();
    let mut video_width: u32 = 0;
    let mut video_height: u32 = 0;
    let mut stream_bitrate: Option<f64> = None;
    let mut is_hdr = false;
    let mut color_transfer = String::new();
    let mut tag_bytes: Option<f64> = None;
    let mut tag_duration: Option<f64> = None;
    let mut audio_streams: Vec<AudioStreamInfo> = Vec::new();
    let mut audio_index: u32 = 0;

    if let Some(streams_arr) = streams {
        for s in streams_arr {
            let codec_type = s["codec_type"].as_str().unwrap_or("");

            if codec_type == "video" && video_codec.is_empty() {
                // First video stream
                video_codec = s["codec_name"].as_str().unwrap_or("").to_string();
                video_width = s["width"].as_u64().unwrap_or(0) as u32;
                video_height = s["height"].as_u64().unwrap_or(0) as u32;

                // Tier 1: stream-level bit_rate
                if let Some(br_str) = s["bit_rate"].as_str() {
                    stream_bitrate = parse_numeric(br_str);
                }

                // MKV stream tags - always extract for duration even if bitrate is found
                let tags = &s["tags"];
                if stream_bitrate.is_none() {
                    // Tier 2: NUMBER_OF_BYTES for bitrate calculation
                    for key in &["NUMBER_OF_BYTES", "number_of_bytes", "Number_Of_Bytes"] {
                        if let Some(v) = tags[*key].as_str() {
                            tag_bytes = parse_numeric(v);
                            if tag_bytes.is_some() {
                                break;
                            }
                        }
                    }
                }
                // Always try to extract DURATION tag (needed for duration_secs)
                if tag_duration.is_none() {
                    for key in &["DURATION", "duration", "Duration"] {
                        if let Some(v) = tags[*key].as_str() {
                            tag_duration = parse_duration_tag(v);
                            if tag_duration.is_some() {
                                break;
                            }
                        }
                    }
                }

                // HDR detection via colour metadata
                let ct = s["color_transfer"].as_str().unwrap_or("");
                if ct == "smpte2084" || ct == "arib-std-b67" {
                    is_hdr = true;
                    color_transfer = ct.to_string();
                }
            } else if codec_type == "audio" {
                // Audio stream - extract in the same pass (#9)
                let codec = s["codec_name"].as_str().unwrap_or("unknown").to_string();
                let br_kbps: u32 = s["bit_rate"]
                    .as_str()
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(|v| (v / 1000) as u32)
                    .unwrap_or(999);

                audio_streams.push(AudioStreamInfo {
                    index: audio_index,
                    codec,
                    bitrate_kbps: br_kbps,
                });
                audio_index += 1;
            }
        }
    }

    // ── Resolve bitrate using the tier waterfall ──
    let video_bitrate_bps = if let Some(bps) = stream_bitrate {
        // Tier 1: stream header
        bps
    } else if let (Some(bytes), Some(dur)) = (tag_bytes, tag_duration) {
        // Tier 2: MKV stream tags
        if dur > 0.0 { (bytes * 8.0) / dur } else { 0.0 }
    } else if let Some(bps) = format["bit_rate"]
        .as_str()
        .and_then(parse_numeric)
    {
        // Tier 3: format-level (container) bitrate
        bps
    } else {
        // Tier 4: packet counting fallback - requires a second ffprobe call
        packet_count_bitrate(file_path, sink).await
    };

    let video_bitrate_mbps = video_bitrate_bps / 1_000_000.0;

    // ── Extract duration ──
    // Six-tier waterfall: format duration → video stream duration →
    // video stream MKV tags → audio stream duration → audio stream
    // MKV tags → packet-scan fallback.
    let duration_secs = format["duration"]
        .as_str()
        .and_then(parse_numeric)
        .or_else(|| {
            // Tier 2: video stream "duration" field
            streams.and_then(|arr| {
                arr.iter().find_map(|s| {
                    if s["codec_type"].as_str().unwrap_or("") == "video" {
                        s["duration"].as_str().and_then(parse_numeric)
                    } else {
                        None
                    }
                })
            })
        })
        .or(tag_duration) // Tier 3: video stream MKV DURATION tag
        .or_else(|| {
            // Tier 4: audio stream "duration" field
            streams.and_then(|arr| {
                arr.iter().find_map(|s| {
                    if s["codec_type"].as_str().unwrap_or("") == "audio" {
                        s["duration"].as_str().and_then(parse_numeric)
                    } else {
                        None
                    }
                })
            })
        })
        .or_else(|| {
            // Tier 5: audio stream MKV DURATION tags
            streams.and_then(|arr| {
                arr.iter().find_map(|s| {
                    if s["codec_type"].as_str().unwrap_or("") == "audio" {
                        let tags = &s["tags"];
                        for key in &["DURATION", "duration", "Duration"] {
                            if let Some(v) = tags[*key].as_str() {
                                let parsed = parse_duration_tag(v);
                                if parsed.is_some() {
                                    return parsed;
                                }
                            }
                        }
                    }
                    None
                })
            })
        })
        .unwrap_or(0.0);

    // Tier 6: if all metadata-based approaches failed, ask ffprobe to
    // calculate duration by scanning the file. This is slower but
    // reliable for containers with missing or corrupt duration headers.
    let duration_secs = if duration_secs > 0.0 {
        duration_secs
    } else {
        packet_scan_duration(file_path, sink).await
    };

    Ok(ProbeResult {
        video_codec,
        video_width,
        video_height,
        video_bitrate_bps,
        video_bitrate_mbps,
        is_hdr,
        color_transfer,
        audio_streams,
        duration_secs,
    })
}

/// Tier 6 duration fallback: determine duration by reading the last
/// video packet's PTS from the file. This handles containers where the
/// duration header is missing or corrupt (e.g. some MKV muxing tools).
///
/// Uses `-read_intervals 99999%+#1` to seek near the end and read one
/// packet, extracting its pts_time as an approximation of total duration.
/// Falls back to a full `-count_packets` scan if the seek approach fails.
async fn packet_scan_duration(file_path: &str, sink: &dyn EventSink) -> f64 {
    sink.log("[probe] Duration missing from metadata, scanning file...");

    // Approach 1: seek near the end and read the last packet's PTS.
    // This is fast even on large files since ffprobe seeks by percentage.
    let raw = run_ffprobe(&[
        "-v", "error",
        "-select_streams", "v:0",
        "-read_intervals", "99999%+#1",
        "-show_entries", "packet=pts_time",
        "-of", "default=noprint_wrappers=1:nokey=1",
        file_path,
    ])
    .await
    .unwrap_or_default();

    if let Some(dur) = parse_numeric(raw.lines().last().unwrap_or("")) {
        sink.log(&format!("[probe] Duration from packet scan: {:.2}s", dur));
        return dur;
    }

    // Approach 2: force ffprobe to calculate duration via the demuxer
    // by requesting format duration with -count_packets.
    let raw2 = run_ffprobe(&[
        "-v", "error",
        "-count_packets",
        "-show_entries", "format=duration",
        "-of", "default=noprint_wrappers=1:nokey=1",
        file_path,
    ])
    .await
    .unwrap_or_default();

    let dur = parse_numeric(&raw2).unwrap_or(0.0);
    if dur > 0.0 {
        sink.log(&format!("[probe] Duration from packet count: {:.2}s", dur));
    }
    dur
}

/// Tier 4 bitrate fallback: sum all video packet sizes and divide by duration.
/// This is the only case that requires a second ffprobe invocation - it's
/// needed for files where neither stream headers, MKV tags, nor format
/// metadata contain a usable bitrate (e.g. some older AVI files).
async fn packet_count_bitrate(file_path: &str, sink: &dyn EventSink) -> f64 {
    sink.log(&format!(
        "[probe] Scanning packets for bitrate (this may take a moment)... {}",
        file_path
    ));

    let raw = match run_ffprobe(&[
        "-v", "error",
        "-select_streams", "v:0",
        "-show_entries", "packet=size",
        "-show_entries", "stream=duration",
        "-show_entries", "format=duration",
        "-of", "json",
        file_path,
    ])
    .await
    {
        Ok(r) => r,
        Err(_) => return 0.0,
    };

    let json: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or(serde_json::Value::Null);

    // Sum packet sizes
    let total_bytes: f64 = json["packets"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|p| {
                    p["size"]
                        .as_str()
                        .and_then(|s| s.parse::<f64>().ok())
                })
                .sum()
        })
        .unwrap_or(0.0);

    if total_bytes <= 0.0 {
        return 0.0;
    }

    // Try stream duration first, then format duration
    let duration = json["streams"]
        .as_array()
        .and_then(|arr| {
            arr.iter().find_map(|s| {
                s["duration"].as_str().and_then(parse_numeric)
            })
        })
        .or_else(|| {
            json["format"]["duration"]
                .as_str()
                .and_then(parse_numeric)
        })
        .unwrap_or(0.0);

    if duration > 0.0 {
        (total_bytes * 8.0) / duration
    } else {
        0.0
    }
}