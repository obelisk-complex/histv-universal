use serde::{Deserialize, Serialize};

use crate::events::EventSink;
use crate::ffmpeg;
use crate::queue::AudioStreamInfo;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
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
    pub dovi_profile: Option<u8>,
    pub dovi_bl_compat_id: Option<u8>,
    pub has_hdr10plus: bool,
    pub subtitle_stream_count: u32,
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
    // Animated WebP: ffprobe can't extract metadata, use RIFF parser
    if file_path.to_lowercase().ends_with(".webp") {
        if let Ok(Some(info)) = crate::webp_decode::probe_webp(std::path::Path::new(file_path)) {
            let duration = info.total_duration_ms as f64 / 1000.0;
            let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
            let bitrate_bps = if duration > 0.0 {
                (file_size as f64 * 8.0) / duration
            } else {
                0.0
            };
            return Ok(ProbeResult {
                video_codec: "webp".to_string(),
                video_width: info.width,
                video_height: info.height,
                video_bitrate_bps: bitrate_bps,
                video_bitrate_mbps: bitrate_bps / 1_000_000.0,
                is_hdr: false,
                color_transfer: String::new(),
                audio_streams: Vec::new(),
                duration_secs: duration,
                dovi_profile: None,
                dovi_bl_compat_id: None,
                has_hdr10plus: false,
                subtitle_stream_count: 0,
            });
        }
        // Fall through to normal probe if RIFF parse fails
        // (might be a static WebP, which ffprobe can handle)
    }

    // ── Single-pass probe: all streams + format in one call ──
    let json_raw = run_ffprobe(&[
        "-v",
        "error",
        "-show_streams",
        "-show_format",
        "-of",
        "json",
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
    let mut dovi_profile: Option<u8> = None;
    let mut dovi_bl_compat_id: Option<u8> = None;
    let mut has_hdr10plus = false;
    let mut subtitle_stream_count: u32 = 0;

    if let Some(streams_arr) = streams {
        for s in streams_arr {
            let codec_type = s["codec_type"].as_str().unwrap_or("");
            if codec_type == "subtitle" {
                subtitle_stream_count += 1;
            }

            if codec_type == "video" && video_codec.is_empty() {
                // First video stream

                // ── Dolby Vision / HDR10+ side data detection ──
                if let Some(side_data_list) = s["side_data_list"].as_array() {
                    for sd in side_data_list {
                        let sd_type = sd["side_data_type"].as_str().unwrap_or("");
                        if sd_type == "DOVI configuration record" {
                            dovi_profile =
                                sd["dv_profile"].as_u64().and_then(|v| u8::try_from(v).ok());
                            dovi_bl_compat_id = sd["dv_bl_signal_compatibility_id"]
                                .as_u64()
                                .and_then(|v| u8::try_from(v).ok());
                        }
                        if sd_type.contains("HDR10+")
                            || sd_type.contains("SMPTE2094-40")
                            || sd_type.contains("HDR Dynamic Metadata")
                        {
                            has_hdr10plus = true;
                        }
                    }
                }
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
                    .and_then(|v| u32::try_from(v / 1000).ok())
                    .unwrap_or(0);

                audio_streams.push(AudioStreamInfo {
                    index: audio_index,
                    codec,
                    bitrate_kbps: br_kbps,
                });
                audio_index += 1;
            }
        }
    }

    // ── Resolve video bitrate ──
    // Compute from file size and duration rather than trusting stream/format
    // metadata, which can report nominal or max rates instead of actual
    // averages (e.g. MPEG1 streams report the sequence header's declared
    // rate, not the real average).
    let file_size = std::fs::metadata(file_path).map(|m| m.len()).unwrap_or(0);
    let audio_total_bps: f64 = audio_streams
        .iter()
        .map(|s| s.bitrate_kbps as f64 * 1000.0)
        .sum();

    // Duration: resolve early so we can use it for bitrate computation.
    // Reuses the same six-tier waterfall that was previously below.

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
        let scanned = packet_scan_duration(file_path, sink).await;
        if scanned <= 0.0 {
            return Err(format!(
                "Could not determine duration for '{}' — all six tiers failed",
                file_path
            ));
        }
        scanned
    };

    let video_bitrate_bps = if file_size > 0 && duration_secs > 0.0 {
        let total_bps = (file_size as f64 * 8.0) / duration_secs;
        (total_bps - audio_total_bps).max(0.0)
    } else if let Some(bps) = stream_bitrate {
        bps
    } else if let (Some(bytes), Some(dur)) = (tag_bytes, tag_duration) {
        if dur > 0.0 {
            (bytes * 8.0) / dur
        } else {
            0.0
        }
    } else if let Some(bps) = format["bit_rate"].as_str().and_then(parse_numeric) {
        bps
    } else {
        packet_count_bitrate(file_path, sink).await
    };

    // If no video stream was found, the file is not a valid video
    if video_codec.is_empty() {
        return Err("No video stream found".to_string());
    }

    let video_bitrate_mbps = video_bitrate_bps / 1_000_000.0;

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
        dovi_profile,
        dovi_bl_compat_id,
        has_hdr10plus,
        subtitle_stream_count,
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
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-read_intervals",
        "99999%+#1",
        "-show_entries",
        "packet=pts_time",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
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
        "-v",
        "error",
        "-count_packets",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
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
///
/// Uses two lightweight ffprobe calls with CSV output to avoid building a
/// multi-hundred-MB JSON DOM for large files.
async fn packet_count_bitrate(file_path: &str, sink: &dyn EventSink) -> f64 {
    sink.log(&format!(
        "[probe] Scanning packets for bitrate (this may take a moment)... {}",
        file_path
    ));

    // Sum packet sizes using CSV output (one size per line).
    let raw = match run_ffprobe(&[
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "packet=size",
        "-of",
        "csv=p=0",
        file_path,
    ])
    .await
    {
        Ok(r) => r,
        Err(_) => return 0.0,
    };

    let total_bytes: f64 = raw
        .lines()
        .filter_map(|line| line.trim().parse::<f64>().ok())
        .sum();

    if total_bytes <= 0.0 {
        return 0.0;
    }

    // Fetch duration separately (tiny response, no DOM needed).
    let dur_raw = match run_ffprobe(&[
        "-v",
        "error",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=duration",
        "-show_entries",
        "format=duration",
        "-of",
        "default=noprint_wrappers=1:nokey=1",
        file_path,
    ])
    .await
    {
        Ok(r) => r,
        Err(_) => return 0.0,
    };

    // The output contains one or two lines (stream duration, format duration).
    // Take the first valid numeric value.
    let duration = dur_raw
        .lines()
        .filter_map(|line| parse_numeric(line.trim()))
        .find(|&v| v > 0.0)
        .unwrap_or(0.0);

    if duration > 0.0 {
        (total_bytes * 8.0) / duration
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── parse_duration_tag ────────────────────────────────────────

    #[test]
    fn test_parse_duration_standard_format() {
        // 1 hour 30 minutes 45.5 seconds = 5445.5
        let result = parse_duration_tag("01:30:45.500").unwrap();
        assert!((result - 5445.5).abs() < 0.001);
    }

    #[test]
    fn test_parse_duration_zero() {
        let result = parse_duration_tag("00:00:00.000").unwrap();
        assert!((result - 0.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_duration_whole_seconds() {
        // No fractional component: "01:00:30" = 3630.0
        let result = parse_duration_tag("01:00:30").unwrap();
        assert!((result - 3630.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_duration_nanosecond_precision() {
        // MKV tags often use "00:42:17.123456789"
        let result = parse_duration_tag("00:42:17.123456789").unwrap();
        let expected = 42.0 * 60.0 + 17.123456789;
        assert!((result - expected).abs() < 1e-6);
    }

    #[test]
    fn test_parse_duration_no_colons() {
        // Just a plain number with no colons should return None.
        assert!(parse_duration_tag("12345").is_none());
    }

    #[test]
    fn test_parse_duration_only_two_parts() {
        // "MM:SS" format — fewer than three colon-separated parts.
        assert!(parse_duration_tag("30:45").is_none());
    }

    #[test]
    fn test_parse_duration_non_numeric() {
        assert!(parse_duration_tag("ab:cd:ef").is_none());
    }

    #[test]
    fn test_parse_duration_empty_string() {
        assert!(parse_duration_tag("").is_none());
    }

    #[test]
    fn test_parse_duration_extra_parts_ignored() {
        // Four colon-separated parts — only the first three are used.
        // "01:02:03:04" → h=1, m=2, s=3 → 3723.0 (the ":04" is ignored)
        let result = parse_duration_tag("01:02:03:04").unwrap();
        assert!((result - 3723.0).abs() < 0.001);
    }

    // ── parse_numeric ─────────────────────────────────────────────

    #[test]
    fn test_parse_numeric_valid() {
        assert_eq!(parse_numeric("12345.67"), Some(12345.67));
    }

    #[test]
    fn test_parse_numeric_whitespace() {
        assert_eq!(parse_numeric("  100  "), Some(100.0));
    }

    #[test]
    fn test_parse_numeric_empty() {
        assert!(parse_numeric("").is_none());
    }

    #[test]
    fn test_parse_numeric_na() {
        assert!(parse_numeric("N/A").is_none());
    }

    #[test]
    fn test_parse_numeric_zero() {
        // Zero is filtered out (must be > 0.0).
        assert!(parse_numeric("0").is_none());
    }

    #[test]
    fn test_parse_numeric_negative() {
        // Negative values are filtered out (must be > 0.0).
        assert!(parse_numeric("-5.0").is_none());
    }

    #[test]
    fn test_parse_numeric_non_numeric() {
        assert!(parse_numeric("abc").is_none());
    }

    #[test]
    fn test_parse_duration_mkv_90_seconds() {
        let result = parse_duration_tag("00:01:30.000000000").unwrap();
        assert!((result - 90.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_duration_large_hours() {
        let result = parse_duration_tag("100:00:00.000").unwrap();
        assert!((result - 360000.0).abs() < 0.001);
    }

    #[test]
    fn test_parse_duration_fractional_only() {
        let result = parse_duration_tag("0:0:0.5").unwrap();
        assert!((result - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_parse_numeric_large_value() {
        assert_eq!(parse_numeric("1000000000"), Some(1000000000.0));
    }

    #[test]
    fn test_parse_numeric_decimal() {
        assert_eq!(parse_numeric("3.14"), Some(3.14));
    }

    #[test]
    fn test_parse_numeric_negative_returns_none() {
        assert!(parse_numeric("-100").is_none());
    }
}
