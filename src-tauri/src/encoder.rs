use crate::events::{EventSink, BatchControl};
use crate::queue::{QueueItem, QueueItemStatus};
use crate::ffmpeg as ffbin;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::process::Command;

// ── Encoder info & abstraction layer ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderInfo {
    pub name: String,
    pub codec_family: String, // "hevc" or "h264"
    pub is_hardware: bool,
}

/// Encoder priority tables per platform (§8.2).
#[cfg(target_os = "windows")]
const HEVC_PRIORITY: &[&str] = &["hevc_amf", "hevc_nvenc", "hevc_qsv", "libx265"];
#[cfg(target_os = "windows")]
const H264_PRIORITY: &[&str] = &["h264_amf", "h264_nvenc", "h264_qsv", "libx264"];

#[cfg(target_os = "macos")]
const HEVC_PRIORITY: &[&str] = &["hevc_videotoolbox", "libx265"];
#[cfg(target_os = "macos")]
const H264_PRIORITY: &[&str] = &["h264_videotoolbox", "libx264"];

#[cfg(target_os = "linux")]
const HEVC_PRIORITY: &[&str] = &["hevc_vaapi", "hevc_nvenc", "hevc_qsv", "libx265"];
#[cfg(target_os = "linux")]
const H264_PRIORITY: &[&str] = &["h264_vaapi", "h264_nvenc", "h264_qsv", "libx264"];

// Fallback for other platforms (compilation only)
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const HEVC_PRIORITY: &[&str] = &["libx265"];
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const H264_PRIORITY: &[&str] = &["libx264"];

const AUDIO_ENCODERS: &[&str] = &["ac3", "eac3", "aac"];

/// Fixed tonemap filter chain for HDR→SDR conversion (#1).
/// Defined as a static str to avoid per-file heap allocation.
const TONEMAP_HABLE: &str =
    "zscale=t=linear,tonemap=hable:desat=0,zscale=t=bt709:p=bt709:m=bt709:r=tv,format=yuv420p";

/// Per-encoder flag mapping for VBR mode (§9.3).
/// Accepts pre-formatted bitrate strings to avoid re-allocating per file.
pub fn vbr_flags(encoder: &str, target: &str, peak: &str) -> Vec<String> {
    match encoder {
        "hevc_amf" | "h264_amf" => vec![
            "-quality".into(), "quality".into(),
            "-rc".into(), "vbr_peak".into(),
            "-b:v".into(), target.into(),
            "-maxrate".into(), peak.into(),
        ],
        "hevc_nvenc" | "h264_nvenc" => vec![
            "-preset".into(), "p7".into(),
            "-rc".into(), "vbr".into(),
            "-b:v".into(), target.into(),
            "-maxrate".into(), peak.into(),
        ],
        "hevc_qsv" | "h264_qsv" => vec![
            "-preset".into(), "veryslow".into(),
            "-b:v".into(), target.into(),
            "-maxrate".into(), peak.into(),
        ],
        "hevc_videotoolbox" | "h264_videotoolbox" => vec![
            "-b:v".into(), target.into(),
            "-maxrate".into(), peak.into(),
        ],
        "hevc_vaapi" | "h264_vaapi" => vec![
            "-rc_mode".into(), "VBR".into(),
            "-b:v".into(), target.into(),
            "-maxrate".into(), peak.into(),
        ],
        "libx265" | "libx264" => vec![
            "-preset".into(), "slow".into(),
            "-b:v".into(), target.into(),
            "-maxrate".into(), peak.into(),
        ],
        _ => vec![
            "-b:v".into(), target.into(),
            "-maxrate".into(), peak.into(),
        ],
    }
}

/// Per-encoder flag mapping for CQP mode (§9.4).
/// Accepts pre-formatted QP strings to avoid re-allocating per file.
pub fn cqp_flags(encoder: &str, qi: &str, qp: &str) -> Vec<String> {
    match encoder {
        "hevc_amf" | "h264_amf" => vec![
            "-quality".into(), "quality".into(),
            "-rc".into(), "cqp".into(),
            "-qp_i".into(), qi.into(),
            "-qp_p".into(), qp.into(),
        ],
        "hevc_nvenc" | "h264_nvenc" => vec![
            "-preset".into(), "p7".into(),
            "-rc".into(), "constqp".into(),
            "-qp".into(), qi.into(),
        ],
        "hevc_qsv" | "h264_qsv" => vec![
            "-preset".into(), "veryslow".into(),
            "-global_quality".into(), qi.into(),
        ],
        "hevc_videotoolbox" | "h264_videotoolbox" => vec![
            "-q:v".into(), qi.into(),
        ],
        "hevc_vaapi" | "h264_vaapi" => vec![
            "-rc_mode".into(), "CQP".into(),
            "-qp".into(), qi.into(),
        ],
        "libx265" | "libx264" => vec![
            "-preset".into(), "slow".into(),
            "-qp".into(), qi.into(),
        ],
        _ => vec![
            "-qp".into(), qi.into(),
        ],
    }
}

/// Per-encoder flag mapping for CRF mode - only valid for libx265/libx264.
/// Hardware encoders do not support CRF; fall back to CQP for them.
/// Accepts pre-formatted strings to avoid re-allocating per file.
pub fn crf_flags(encoder: &str, crf_str: &str, qi: &str, qp: &str) -> Vec<String> {
    match encoder {
        "libx265" | "libx264" => vec![
            "-preset".into(), "slow".into(),
            "-crf".into(), crf_str.into(),
        ],
        // HW encoders don't support CRF - fall back to CQP
        _ => cqp_flags(encoder, qi, qp),
    }
}

/// Software fallback encoder for a given codec family.
pub fn software_fallback(codec_family: &str) -> &'static str {
    if codec_family == "H.264" {
        "libx264"
    } else {
        "libx265"
    }
}

// ── Encoding strategy decision ─────────────────────────────────

/// The encoding decision for a single file, determined purely from its
/// probed metadata and the batch settings. Used by both the dry-run plan
/// display and the encoding loop to ensure consistent decisions.
#[derive(Debug, Clone)]
pub enum EncodeDecision {
    /// Stream-copy: already the target codec and at/below the bitrate threshold.
    Copy,
    /// VBR transcode: source exceeds the bitrate threshold.
    Vbr { target_bps: u64, peak_bps: u64 },
    /// CQP transcode: at/below threshold, cross-codec or zero-bitrate probe.
    Cqp { qi: u32, qp: u32 },
    /// CRF transcode: at/below threshold, CRF rate-control selected.
    Crf { crf: u32, qi_fallback: u32, qp_fallback: u32 },
}

/// Determine the encoding strategy for a single file.
///
/// This is a pure function with no I/O - it uses only the probed metadata
/// and batch settings to produce a decision. Both the GUI's encoding loop
/// and the CLI's dry-run call this to avoid duplicating the decision logic.
///
/// `video_codec` is the source file's codec name (e.g. "hevc", "h264").
/// `target_codec` is the target codec name (e.g. "hevc", "h264") - note
/// this is the lowercase ffmpeg name, not the display name ("HEVC"/"H.264").
pub fn decide_encode_strategy(
    bitrate_mbps: f64,
    threshold: f64,
    video_codec: &str,
    target_codec: &str,
    rate_control_mode: &str,
    qp_i: u32,
    qp_p: u32,
    crf_val: u32,
    peak_multiplier: f64,
) -> EncodeDecision {
    // Codecs that cannot be stream-copied into MKV/MP4 - always transcode.
    let is_copyable = !matches!(video_codec, "gif" | "apng" | "mjpeg");
    let is_already_target = video_codec == target_codec;

    if bitrate_mbps <= threshold || bitrate_mbps <= 0.0 {
        if bitrate_mbps > 0.0 && is_copyable {
            EncodeDecision::Copy
        } else if rate_control_mode == "CRF" || rate_control_mode == "crf" {
            EncodeDecision::Crf {
                crf: crf_val,
                qi_fallback: qp_i,
                qp_fallback: qp_p,
            }
        } else {
            EncodeDecision::Cqp { qi: qp_i, qp: qp_p }
        }
    } else if is_already_target && is_copyable && bitrate_mbps <= threshold * 1.15 {
        EncodeDecision::Copy
    } else {
        let target_bps = (threshold * 1_000_000.0) as u64;
        let mult = if peak_multiplier > 1.0 { peak_multiplier } else { 1.5 };
        let peak_bps = (target_bps as f64 * mult) as u64;
        EncodeDecision::Vbr { target_bps, peak_bps }
    }
}

/// Format a human-readable description of the encoding decision for a file.
pub fn describe_decision(
    decision: &EncodeDecision,
    item_codec: &str,
    item_width: u32,
    item_height: u32,
    item_bitrate_mbps: f64,
    codec_family: &str,
    threshold: f64,
) -> String {
    match decision {
        EncodeDecision::Copy => {
            format!(
                "Already {} at {:.2}Mbps (at/below target) - copy",
                codec_family, item_bitrate_mbps
            )
        }
        EncodeDecision::Vbr { peak_bps, .. } => {
            let peak_mbps = *peak_bps as f64 / 1_000_000.0;
            format!(
                "{:.2}Mbps [{}] {}x{} - VBR target {}Mbps peak {:.2}Mbps",
                item_bitrate_mbps, item_codec, item_width, item_height,
                threshold, peak_mbps
            )
        }
        EncodeDecision::Cqp { qi, qp } => {
            format!(
                "{:.2}Mbps [{}] {}x{} - CQP ({}/{})",
                item_bitrate_mbps, item_codec, item_width, item_height,
                qi, qp
            )
        }
        EncodeDecision::Crf { crf, .. } => {
            format!(
                "{:.2}Mbps [{}] {}x{} - CRF {}",
                item_bitrate_mbps, item_codec, item_width, item_height,
                crf
            )
        }
    }
}

// ── Encoder detection (§8) ──────────────────────────────────────

/// Detect available encoders by running `ffmpeg -encoders`, then verifying
/// each hardware encoder with a single-frame test encode.
/// Software encoders (libx265/libx264) skip the test - they always work.
pub async fn detect_encoders(sink: &dyn EventSink) -> (Vec<EncoderInfo>, Vec<String>) {
    sink.log("[detect] Starting encoder detection...");

    let output = match ffbin::ffmpeg_command()
        .args(["-encoders"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => match child.wait_with_output().await {
            Ok(o) => o,
            Err(e) => {
                sink.log(&format!("[detect] ffmpeg wait error: {e}"));
                return fallback_encoders();
            }
        },
        Err(e) => {
            sink.log(&format!("[detect] ffmpeg not found: {e}"));
            return fallback_encoders();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut video_encoders: Vec<EncoderInfo> = Vec::new();
    let mut audio_encoders: Vec<String> = Vec::new();

    // Parse the full encoder list once into a HashSet for O(1) lookups
    let available: HashSet<&str> = stdout.lines()
        .flat_map(|line| line.split_whitespace())
        .collect();

    // Check each video encoder in priority order
    for &enc_name in HEVC_PRIORITY.iter().chain(H264_PRIORITY.iter()) {
        if !available.contains(enc_name) {
            sink.log(&format!("[detect] {enc_name} not listed in ffmpeg"));
            continue;
        }

        let family = if enc_name.starts_with("hevc") || enc_name == "libx265" {
            "hevc"
        } else {
            "h264"
        };
        let is_hw = !enc_name.starts_with("lib");

        // Software encoders always work - skip test encode
        if !is_hw {
            sink.log(&format!("[detect] {enc_name} (SW) - available"));
            video_encoders.push(EncoderInfo {
                name: enc_name.to_string(),
                codec_family: family.to_string(),
                is_hardware: false,
            });
            continue;
        }

        // Hardware encoder - verify with a 1-frame test encode
        sink.log(&format!("[detect] {enc_name} - testing..."));
        if test_encode(enc_name).await {
            sink.log(&format!("[detect] {enc_name} (HW) - works"));
            video_encoders.push(EncoderInfo {
                name: enc_name.to_string(),
                codec_family: family.to_string(),
                is_hardware: true,
            });
        } else {
            sink.log(&format!("[detect] {enc_name} (HW) - not available (test encode failed)"));
        }
    }

    // Check audio encoders
    for &aenc in AUDIO_ENCODERS {
        if available.contains(aenc) {
            audio_encoders.push(aenc.to_string());
        }
    }

    // Ensure software fallbacks are always present
    ensure_fallback(&mut video_encoders, "libx265", "hevc");
    ensure_fallback(&mut video_encoders, "libx264", "h264");
    if !audio_encoders.contains(&"ac3".to_string()) {
        audio_encoders.push("ac3".to_string());
    }

    let total = video_encoders.len() + audio_encoders.len();
    sink.log(&format!(
        "[detect] Detection complete: {} encoders ({} video, {} audio)",
        total,
        video_encoders.len(),
        audio_encoders.len()
    ));

    (video_encoders, audio_encoders)
}

/// Test a hardware encoder by encoding a single black frame to a temp file.
/// Returns true if the encoder produced output successfully.
async fn test_encode(encoder_name: &str) -> bool {
    // Use a temp file - some hardware encoders fail with -f null
    let tmp_dir = std::env::temp_dir();
    let tmp_file = tmp_dir.join(format!("_histv_test_{}.mp4", encoder_name));
    let tmp_str = tmp_file.to_string_lossy().to_string();

    // 256x256 minimum - some HW encoders reject smaller resolutions
    // nv12 pixel format - required by most hardware encoders
    let result = ffbin::ffmpeg_command()
        .args([
            "-y",
            "-f", "lavfi",
            "-i", "color=black:s=256x256:d=0.04:r=25",
            "-frames:v", "1",
            "-pix_fmt", "nv12",
            "-c:v", encoder_name,
            &tmp_str,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();

    let success = match result {
        Ok(child) => match child.wait_with_output().await {
            Ok(o) => o.status.success(),
            Err(_) => false,
        },
        Err(_) => false,
    };

    // Clean up temp file regardless of outcome
    let _ = std::fs::remove_file(&tmp_file);

    success
}

fn ensure_fallback(encoders: &mut Vec<EncoderInfo>, name: &str, family: &str) {
    if !encoders.iter().any(|e| e.name == name) {
        encoders.push(EncoderInfo {
            name: name.to_string(),
            codec_family: family.to_string(),
            is_hardware: false,
        });
    }
}

fn fallback_encoders() -> (Vec<EncoderInfo>, Vec<String>) {
    (
        vec![
            EncoderInfo {
                name: "libx265".into(),
                codec_family: "hevc".into(),
                is_hardware: false,
            },
            EncoderInfo {
                name: "libx264".into(),
                codec_family: "h264".into(),
                is_hardware: false,
            },
        ],
        vec!["ac3".into()],
    )
}

// ── Batch encoding (§10) ────────────────────────────────────────

/// Batch encoding settings, extracted from either CLI args or GUI settings JSON.
#[derive(Debug, Clone)]
pub struct BatchSettings {
    pub output_folder: String,
    pub output_container: String,
    pub output_mode: String, // "folder" | "beside" | "replace"
    pub threshold: f64,
    pub qp_i: u32,
    pub qp_p: u32,
    pub crf_val: u32,
    pub rate_control_mode: String,
    pub video_encoder: String,
    pub codec_family: String,
    pub audio_encoder: String,
    pub audio_cap: u32,
    pub pix_fmt: String,
    pub delete_source: bool,
    pub save_log: bool,
    pub post_command: Option<String>,
    pub peak_multiplier: f64,
    pub threads: u32,
    pub low_priority: bool,
    pub precision_mode: bool,
}

/// Shared encoding loop used by the CLI. Takes trait objects for output and
/// control, and a mutable queue of items to encode.
///
/// Returns (done_count, fail_count, skip_count, was_cancelled).
pub async fn run_encode_loop(
    sink: &dyn EventSink,
    batch_control: &dyn BatchControl,
    queue: &mut Vec<QueueItem>,
    settings: &BatchSettings,
) -> (u32, u32, u32, bool) {
    let sw_fallback = software_fallback(&settings.codec_family).to_string();
    let qi_str = settings.qp_i.to_string();
    let qp_str = settings.qp_p.to_string();
    let crf_str = settings.crf_val.to_string();

    let target_codec = if settings.codec_family == "H.264" || settings.codec_family == "h264" {
        "h264"
    } else {
        "hevc"
    };

    // ── Pre-loop invariant computation (#2, #3, #4) ──
    let preserve_hdr = settings.pix_fmt == "p010le";
    let is_sw = settings.video_encoder == "libx265" || settings.video_encoder == "libx264";
    let cached_ram_gb = get_system_ram_gb();

    // Collect pending indices
    let pending_indices: Vec<usize> = queue.iter()
        .enumerate()
        .filter(|(_, item)| item.status == QueueItemStatus::Pending)
        .map(|(i, _)| i)
        .collect();

    let total = pending_indices.len();
    let mut done_count: u32 = 0;
    let mut fail_count: u32 = 0;
    let mut skip_count: u32 = 0;
    let save_log = settings.save_log;
    let batch_start = std::time::Instant::now();
    let mut file_counter: u32 = 0;
    let mut was_cancelled = false;

    // ── Incremental log file (#7) ──
    // Open the log file at batch start and append each line as it's produced,
    // instead of buffering all lines in a Vec<String>.
    let mut log_writer: Option<std::io::BufWriter<std::fs::File>> = if save_log {
        let log_filename = format!(
            "encode_log_{}.txt",
            chrono::Local::now().format("%Y%m%d_%H%M%S")
        );
        let log_path = std::path::Path::new(&settings.output_folder).join(&log_filename);
        match std::fs::File::create(&log_path) {
            Ok(f) => {
                sink.log(&format!("  Log file: {}", log_path.display()));
                Some(std::io::BufWriter::new(f))
            }
            Err(e) => {
                sink.log(&format!("  WARNING: Could not create log file: {e}"));
                None
            }
        }
    } else {
        None
    };

    // Helper closure: append a line to the log file if open
    let write_log = |writer: &mut Option<std::io::BufWriter<std::fs::File>>, line: &str| {
        if let Some(ref mut w) = writer {
            use std::io::Write;
            let _ = writeln!(w, "{}", line);
        }
    };

    sink.batch_started();

    for &idx in &pending_indices {
        // Pause / cancel check - wait while paused before starting next file
        while batch_control.is_paused() {
            if batch_control.should_cancel_all() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        if batch_control.should_cancel_all() {
            was_cancelled = true;
            break;
        }
        batch_control.clear_cancel_current();

        file_counter += 1;
        sink.batch_progress(file_counter, total);

        // Extract read-only data from the queue item. Cloning individual
        // fields avoids a full QueueItem clone (which includes audio_streams
        // Vec, all Strings, and fields unused in the loop body).
        let item_full_path = queue[idx].full_path.clone();
        let item_file_name = queue[idx].file_name.clone();
        let item_base_name = queue[idx].base_name.clone();
        let item_video_codec = queue[idx].video_codec.clone();
        let item_video_width = queue[idx].video_width;
        let item_video_height = queue[idx].video_height;
        let item_video_bitrate_mbps = queue[idx].video_bitrate_mbps;
        let item_duration_secs = queue[idx].duration_secs;
        let item_is_hdr = queue[idx].is_hdr;
        let is_image_source = matches!(item_video_codec.as_str(), "gif" | "apng" | "mjpeg");

        // Per-file pixel format and optional tonemap filter (#1, #2).
        // preserve_hdr is hoisted above the loop; tonemap uses a static str.
        let (file_pix_fmt, tonemap_filter): (&str, Option<&'static str>) = if item_is_hdr && !preserve_hdr {
            // HDR source, user wants SDR - tonemap via zscale + Hable curve
            ("yuv420p", Some(TONEMAP_HABLE))
        } else if item_is_hdr {
            // HDR source, preserve HDR
            ("p010le", None)
        } else {
            // SDR source
            ("yuv420p", None)
        };

        sink.batch_status(&format!("[{}/{}] {}", file_counter, total, item_file_name));

        if tonemap_filter.is_some() {
            sink.log("  HDR → SDR: tonemapping with Hable curve");
        }

        queue[idx].status = QueueItemStatus::Encoding;
        sink.queue_item_updated(idx, "Encoding");

        let ext = if settings.output_container == "mp4" { "mp4" } else { "mkv" };
        let is_replace_mode = settings.output_mode == "replace";

        // Determine output path based on output mode (#8 - eliminate clone)
        let (output_file, temp_output_file) = match settings.output_mode.as_str() {
            "beside" => {
                // Create an "output" subfolder next to the input file
                let input_dir = std::path::Path::new(&item_full_path)
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."));
                let out_dir = input_dir.join("output");
                if !out_dir.exists() {
                    if let Err(e) = std::fs::create_dir_all(&out_dir) {
                        let err_msg = format!("  ERROR: Could not create output folder: {e}");
                        sink.log(&err_msg);
                        write_log(&mut log_writer, &err_msg);
                        queue[idx].status = QueueItemStatus::Failed;
                        sink.queue_item_updated(idx, "Failed");
                        fail_count += 1;
                        continue;
                    }
                }
                let out = out_dir.join(format!("{}.{}", item_base_name, ext));
                // Both paths are the same - no clone needed
                let out_str = out.to_string_lossy().to_string();
                let out2 = std::path::PathBuf::from(&out_str);
                (out, out2)
            }
            "replace" => {
                // Encode to a temp file in the same directory, then replace the source
                let input_path = std::path::Path::new(&item_full_path);
                let input_dir = input_path
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."));
                let final_path = input_dir.join(format!("{}.{}", item_base_name, ext));
                let temp_path = input_dir.join(format!("{}.histv-tmp.{}", item_base_name, ext));
                (final_path, temp_path)
            }
            _ => {
                // "folder" - default: use the configured output folder
                let out = std::path::Path::new(&settings.output_folder)
                    .join(format!("{}.{}", item_base_name, ext));
                // Both paths are the same - no clone needed
                let out_str = out.to_string_lossy().to_string();
                let out2 = std::path::PathBuf::from(&out_str);
                (out, out2)
            }
        };
        let output_str = temp_output_file.to_string_lossy().to_string();
        let final_output_str = output_file.to_string_lossy().to_string();

        let log_msg = format!("[{}/{}] {}", file_counter, total, item_file_name);
        sink.log(&log_msg);
        write_log(&mut log_writer, &log_msg);

        // Overwrite check - in replace mode, we're replacing the source so
        // no overwrite prompt is needed. In other modes, check the final output.
        if !is_replace_mode && output_file.exists() {
            if !batch_control.overwrite_always() {
                let response = batch_control.overwrite_prompt(&final_output_str);
                match response.as_str() {
                    "no" | "skip" => {
                        sink.log("  Skipped (output exists)");
                        write_log(&mut log_writer, "  Skipped (output exists)");
                        queue[idx].status = QueueItemStatus::Skipped;
                        sink.queue_item_updated(idx, "Skipped");
                        skip_count += 1;
                        continue;
                    }
                    "always" => {
                        batch_control.set_overwrite_always();
                    }
                    "cancel" => {
                        was_cancelled = true;
                        break;
                    }
                    _ => {} // "yes" - proceed
                }
            }
        }

        // Determine encoding strategy
        let decision = decide_encode_strategy(
            item_video_bitrate_mbps,
            settings.threshold,
            &item_video_codec,
            target_codec,
            &settings.rate_control_mode,
            settings.qp_i,
            settings.qp_p,
            settings.crf_val,
            settings.peak_multiplier,
        );

        // Build video args from the decision
        let mut video_args: Vec<String>;
        let mut mode_desc: String;

        match &decision {
            EncodeDecision::Copy => {
                video_args = vec!["-c:v".into(), "copy".into()];
                mode_desc = format!(
                    "  Already {} at {:.2}Mbps (at/below target) - copying video",
                    settings.codec_family, item_video_bitrate_mbps
                );
            }
            EncodeDecision::Vbr { target_bps, peak_bps } => {
                let target_str = target_bps.to_string();
                let peak_str = peak_bps.to_string();
                video_args = vec!["-c:v".into(), settings.video_encoder.clone()];
                video_args.extend(vbr_flags(&settings.video_encoder, &target_str, &peak_str));
                let peak_mbps = *peak_bps as f64 / 1_000_000.0;
                mode_desc = format!(
                    "  Video: {:.2}Mbps [{}] {}x{} - VBR target {}Mbps peak {:.2}Mbps",
                    item_video_bitrate_mbps, item_video_codec,
                    item_video_width, item_video_height,
                    settings.threshold, peak_mbps
                );
            }
            EncodeDecision::Cqp { .. } => {
                video_args = vec!["-c:v".into(), settings.video_encoder.clone()];
                video_args.extend(cqp_flags(&settings.video_encoder, &qi_str, &qp_str));
                mode_desc = format!(
                    "  Video: {:.2}Mbps [{}] {}x{} - CQP ({}/{})",
                    item_video_bitrate_mbps, item_video_codec,
                    item_video_width, item_video_height,
                    settings.qp_i, settings.qp_p
                );
            }
            EncodeDecision::Crf { crf, .. } => {
                video_args = vec!["-c:v".into(), settings.video_encoder.clone()];
                video_args.extend(crf_flags(&settings.video_encoder, &crf_str, &qi_str, &qp_str));
                mode_desc = format!(
                    "  Video: {:.2}Mbps [{}] {}x{} - CRF {}",
                    item_video_bitrate_mbps, item_video_codec,
                    item_video_width, item_video_height, crf
                );
            }
        }

        // ── Precision mode: CRF viability probe + lookahead + maxrate ──
        // Only applies to CRF decisions with software encoders.
        // is_sw is hoisted above the loop (#4).
        let is_crf_decision = matches!(decision, EncodeDecision::Crf { .. });

        if settings.precision_mode && is_sw && is_crf_decision && !is_image_source {
            // Probe CRF viability with 3 x 10-second samples
            sink.log("  Precision mode: probing CRF viability...");

            let probe_result = probe_crf_viability(
                &item_full_path,
                item_duration_secs,
                &video_args,
                file_pix_fmt,
                settings.threads,
                settings.low_priority,
                sink,
                batch_control,
            ).await;

            // Check for cancellation during probe
            if batch_control.should_cancel_all() {
                was_cancelled = true;
                queue[idx].status = QueueItemStatus::Cancelled;
                sink.queue_item_updated(idx, "Cancelled");
                break;
            }
            if batch_control.should_cancel_current() {
                queue[idx].status = QueueItemStatus::Cancelled;
                sink.queue_item_updated(idx, "Cancelled");
                continue;
            }

            match probe_result {
                Some(avg_mbps) if item_video_bitrate_mbps > 0.0 && avg_mbps > item_video_bitrate_mbps => {
                    // CRF would produce a larger file than source - fall back to CQP
                    sink.log(&format!(
                        "  Precision mode: CRF estimate {:.2}Mbps exceeds source {:.2}Mbps - falling back to CQP",
                        avg_mbps, item_video_bitrate_mbps
                    ));
                    write_log(&mut log_writer, &format!(
                        "  Precision: CRF {:.2}Mbps > source {:.2}Mbps - CQP fallback",
                        avg_mbps, item_video_bitrate_mbps
                    ));
                    // Rebuild video args as CQP
                    video_args = vec!["-c:v".into(), settings.video_encoder.clone()];
                    video_args.extend(cqp_flags(&settings.video_encoder, &qi_str, &qp_str));
                    mode_desc = format!(
                        "  Video: {:.2}Mbps [{}] {}x{} - CQP ({}/{}) (precision fallback)",
                        item_video_bitrate_mbps, item_video_codec,
                        item_video_width, item_video_height,
                        settings.qp_i, settings.qp_p
                    );
                    sink.log(&mode_desc);
                    write_log(&mut log_writer, &mode_desc);
                }
                Some(_) | None => {
                    // CRF is viable (or file was too short to probe) - enhance
                    // based on available system RAM (#3 - use cached value).
                    let use_lookahead = !precision_needs_two_pass_with_ram(cached_ram_gb);
                    let lookahead = if use_lookahead { lookahead_for_ram_with_cache(cached_ram_gb) } else { 0 };

                    if lookahead > 0 {
                        video_args.extend(vec![
                            "-rc-lookahead".into(), lookahead.to_string(),
                        ]);
                        sink.log(&format!("  Precision mode: rc-lookahead {} ({}GB RAM)", lookahead, cached_ram_gb));
                    } else if use_lookahead {
                        sink.log(&format!("  Precision mode: default lookahead ({}GB RAM)", cached_ram_gb));
                    } else {
                        sink.log(&format!("  Precision mode: two-pass ({}GB RAM - insufficient for extended lookahead)", cached_ram_gb));
                    }

                    // Cap with maxrate based on the user's target and peak multiplier
                    if settings.threshold > 0.0 {
                        let maxrate_bps = (settings.threshold * 1_000_000.0 * settings.peak_multiplier) as u64;
                        video_args.extend(vec![
                            "-maxrate".into(), maxrate_bps.to_string(),
                            "-bufsize".into(), (maxrate_bps * 2).to_string(),
                        ]);
                        sink.log(&format!(
                            "  Precision mode: maxrate {:.1}Mbps ({}Mbps x {}x)",
                            maxrate_bps as f64 / 1_000_000.0,
                            settings.threshold, settings.peak_multiplier
                        ));
                    }

                    let precision_note = if lookahead > 0 {
                        format!(" (precision: lookahead {})", lookahead)
                    } else if !use_lookahead {
                        " (precision: 2-pass)".to_string()
                    } else {
                        " (precision)".to_string()
                    };
                    mode_desc = format!("{}{}", mode_desc, precision_note);
                    sink.log(&mode_desc);
                    write_log(&mut log_writer, &mode_desc);
                }
            }
        } else {
            sink.log(&mode_desc);
            write_log(&mut log_writer, &mode_desc);
        }

        // Build audio args (skipped for image sources like GIF)
        let (audio_map_args, audio_codec_args) = if is_image_source {
            (Vec::new(), Vec::new())
        } else {
            build_audio_args_from_probe(
                &queue[idx].audio_streams, &settings.audio_encoder, settings.audio_cap, sink,
            )
        };

        // Assemble ffmpeg command
        let video_arg_refs: Vec<&str> = video_args.iter().map(|s| s.as_str()).collect();
        let ffmpeg_args = assemble_ffmpeg_args(
            &item_full_path, &video_arg_refs, file_pix_fmt,
            &audio_map_args, &audio_codec_args, &output_str,
            is_image_source, settings.threads,
            tonemap_filter,
        );

        let cmd_line = format!("ffmpeg {}", ffmpeg_args.join(" "));
        sink.batch_command(&cmd_line);
        let cmd_log = format!("  CMD: {cmd_line}");
        sink.log(&cmd_log);
        write_log(&mut log_writer, &cmd_log);

        // Spawn ffmpeg - optionally as a two-pass encode.
        // Precision mode on low-RAM systems (<8GB) uses two-pass CRF instead
        // of extended lookahead, since the lookahead would consume too much memory.
        let precision_two_pass = settings.precision_mode && is_sw
            && is_crf_decision && precision_needs_two_pass_with_ram(cached_ram_gb);
        let use_two_pass = precision_two_pass;

        let proc_start = std::time::Instant::now();
        let exit_code: i32;

        if use_two_pass {
            // ── Two-pass VBR (software encoders only) (#5) ──
            let passlog_prefix = std::env::temp_dir()
                .join(format!("histv_2pass_{}", std::process::id()))
                .to_string_lossy()
                .to_string();

            let fmt = if settings.output_container == "mp4" { "mp4" } else { "matroska" };

            // Pass 1: analysis only - build args fresh from components
            let pass1_args = build_two_pass_args(
                &ffmpeg_args, 1, &passlog_prefix, fmt,
            );

            let pass1_cmd = format!("ffmpeg {}", pass1_args.join(" "));
            sink.batch_command(&pass1_cmd);
            write_log(&mut log_writer, &format!("  CMD (pass 1): {pass1_cmd}"));

            match run_ffmpeg_with_progress(
                &pass1_args, item_duration_secs, Some((1, 2)), settings.low_priority, sink, batch_control,
            ).await {
                Ok(r) if r.was_cancelled_all => {
                    sink.log("  Cancelled (batch cancel)");
                    write_log(&mut log_writer, "  Cancelled (batch cancel)");
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    was_cancelled = true;
                    cleanup_passlog_files(&passlog_prefix);
                    break;
                }
                Ok(r) if r.was_cancelled_current => {
                    sink.log("  Cancelled current file");
                    write_log(&mut log_writer, "  Cancelled");
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    cleanup_passlog_files(&passlog_prefix);
                    continue;
                }
                Ok(r) if r.exit_code != 0 => {
                    sink.log(&format!("  ERROR: Pass 1 failed (exit code {})", r.exit_code));
                    write_log(&mut log_writer, "  ERROR: Pass 1 failed");
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    fail_count += 1;
                    cleanup_passlog_files(&passlog_prefix);
                    continue;
                }
                Err(e) => {
                    sink.log(&format!("  ERROR: {e}"));
                    write_log(&mut log_writer, &format!("  ERROR: {e}"));
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    fail_count += 1;
                    cleanup_passlog_files(&passlog_prefix);
                    continue;
                }
                _ => {} // pass 1 OK
            }

            // Pass 2: actual encode with stats from pass 1
            let pass2_args = build_two_pass_args(
                &ffmpeg_args, 2, &passlog_prefix, fmt,
            );

            let pass2_cmd = format!("ffmpeg {}", pass2_args.join(" "));
            sink.batch_command(&pass2_cmd);
            write_log(&mut log_writer, &format!("  CMD (pass 2): {pass2_cmd}"));

            match run_ffmpeg_with_progress(
                &pass2_args, item_duration_secs, Some((2, 2)), settings.low_priority, sink, batch_control,
            ).await {
                Ok(r) if r.was_cancelled_all => {
                    sink.log("  Cancelled (batch cancel)");
                    write_log(&mut log_writer, "  Cancelled (batch cancel)");
                    if temp_output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    was_cancelled = true;
                    cleanup_passlog_files(&passlog_prefix);
                    break;
                }
                Ok(r) if r.was_cancelled_current => {
                    sink.log("  Cancelled current file");
                    write_log(&mut log_writer, "  Cancelled");
                    if temp_output_file.exists() {
                        let _ = std::fs::remove_file(&temp_output_file);
                        sink.log("  Partial output removed");
                    }
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    cleanup_passlog_files(&passlog_prefix);
                    continue;
                }
                Ok(r) => { exit_code = r.exit_code; }
                Err(e) => {
                    sink.log(&format!("  ERROR: {e}"));
                    write_log(&mut log_writer, &format!("  ERROR: {e}"));
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    fail_count += 1;
                    cleanup_passlog_files(&passlog_prefix);
                    continue;
                }
            }
            cleanup_passlog_files(&passlog_prefix);
        } else {
            // ── Single-pass encode ──
            match run_ffmpeg_with_progress(
                &ffmpeg_args, item_duration_secs, None, settings.low_priority, sink, batch_control,
            ).await {
                Ok(r) if r.was_cancelled_all => {
                    sink.log("  Cancelled (batch cancel)");
                    write_log(&mut log_writer, "  Cancelled (batch cancel)");
                    if output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    was_cancelled = true;
                    break;
                }
                Ok(r) if r.was_cancelled_current => {
                    sink.log("  Cancelled current file");
                    write_log(&mut log_writer, "  Cancelled");
                    if output_file.exists() {
                        let _ = std::fs::remove_file(&temp_output_file);
                        sink.log("  Partial output removed");
                    }
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    continue;
                }
                Ok(r) => { exit_code = r.exit_code; }
                Err(e) => {
                    sink.log(&format!("  ERROR: {e}"));
                    write_log(&mut log_writer, &format!("  ERROR: {e}"));
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    fail_count += 1;
                    continue;
                }
            }
        }

        let proc_duration = proc_start.elapsed();
        // Encoder failure fallback (#6 - use run_ffmpeg_with_progress)
        if exit_code != 0
            && proc_duration.as_secs() < 30
            && !matches!(decision, EncodeDecision::Copy)
            && settings.video_encoder != sw_fallback
        {
            if !batch_control.hw_fallback_offered() {
                batch_control.set_hw_fallback_offered();
                sink.log(&format!("  HW encoder failed for {}", item_file_name));
                let response = batch_control.fallback_prompt(&item_file_name);

                if response == "yes" {
                    sink.log(&format!("  Falling back to software encoder ({})...", sw_fallback));
                    write_log(&mut log_writer, &format!("  Fallback to {}", sw_fallback));

                    if output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }

                    // Rebuild with software encoder
                    let mut sw_video_args: Vec<String>;
                    match &decision {
                        EncodeDecision::Vbr { target_bps, peak_bps } => {
                            sw_video_args = vec!["-c:v".into(), sw_fallback.clone()];
                            sw_video_args.extend(vbr_flags(&sw_fallback, &target_bps.to_string(), &peak_bps.to_string()));
                        }
                        _ => {
                            sw_video_args = vec!["-c:v".into(), sw_fallback.clone()];
                            sw_video_args.extend(cqp_flags(&sw_fallback, &qi_str, &qp_str));
                        }
                    }

                    // Reuse audio args from the primary encode - inputs haven't changed
                    let sw_ref: Vec<&str> = sw_video_args.iter().map(|s| s.as_str()).collect();
                    let sw_args = assemble_ffmpeg_args(
                        &item_full_path, &sw_ref, file_pix_fmt,
                        &audio_map_args, &audio_codec_args, &output_str,
                        is_image_source, settings.threads,
                        tonemap_filter,
                    );

                    let sw_cmd = format!("ffmpeg {}", sw_args.join(" "));
                    sink.batch_command(&sw_cmd);
                    let sw_cmd_log = format!("  CMD (fallback): {sw_cmd}");
                    sink.log(&sw_cmd_log);
                    write_log(&mut log_writer, &sw_cmd_log);

                    // Use shared helper for progress, pause/cancel, low-priority (#6)
                    match run_ffmpeg_with_progress(
                        &sw_args, item_duration_secs, None, settings.low_priority, sink, batch_control,
                    ).await {
                        Ok(r) if r.was_cancelled_all => {
                            sink.log("  Cancelled (batch cancel during fallback)");
                            write_log(&mut log_writer, "  Cancelled (batch cancel during fallback)");
                            if temp_output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }
                            queue[idx].status = QueueItemStatus::Cancelled;
                            sink.queue_item_updated(idx, "Cancelled");
                            was_cancelled = true;
                            break;
                        }
                        Ok(r) if r.was_cancelled_current => {
                            sink.log("  Cancelled current file (during fallback)");
                            write_log(&mut log_writer, "  Cancelled (during fallback)");
                            if temp_output_file.exists() {
                                let _ = std::fs::remove_file(&temp_output_file);
                                sink.log("  Partial output removed");
                            }
                            queue[idx].status = QueueItemStatus::Cancelled;
                            sink.queue_item_updated(idx, "Cancelled");
                            continue;
                        }
                        Ok(r) if r.exit_code != 0 => {
                            sink.log("  ERROR: Software encoder also failed - stopping batch");
                            write_log(&mut log_writer, "  ERROR: Software encoder also failed");
                            if temp_output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }
                            queue[idx].status = QueueItemStatus::Failed;
                            sink.queue_item_updated(idx, "Failed");
                            fail_count += 1;
                            break;
                        }
                        Err(e) => {
                            sink.log(&format!("  ERROR: Could not launch fallback: {e}"));
                            write_log(&mut log_writer, &format!("  ERROR: Fallback launch failed: {e}"));
                            queue[idx].status = QueueItemStatus::Failed;
                            sink.queue_item_updated(idx, "Failed");
                            fail_count += 1;
                            break;
                        }
                        _ => {} // fallback encode OK
                    }
                } else {
                    sink.log("  Stopping batch due to encoder failure");
                    write_log(&mut log_writer, "  Batch stopped (encoder failure)");
                    if output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    fail_count += 1;
                    break;
                }
            }
        } else if exit_code != 0 {
            let err_msg = format!("  ERROR: ffmpeg exited with code {}", exit_code);
            sink.log(&err_msg);
            write_log(&mut log_writer, &err_msg);
            if output_file.exists() {
                let _ = std::fs::remove_file(&temp_output_file);
                sink.log("  Failed output removed");
            }
            queue[idx].status = QueueItemStatus::Failed;
            sink.queue_item_updated(idx, "Failed");
            fail_count += 1;
            continue;
        }

        // Post-encode size check
        // In replace mode, ffmpeg wrote to temp_output_file; in other modes temp == final.
        if temp_output_file.exists() {
            let src_size = std::fs::metadata(&item_full_path).map(|m| m.len()).unwrap_or(0);
            let dst_size = std::fs::metadata(&temp_output_file).map(|m| m.len()).unwrap_or(0);

            if dst_size > src_size && src_size > 0 && !is_image_source {
                sink.log(&format!(
                    "  WARNING: Output ({:.1}MB) larger than source ({:.1}MB) - remuxing source instead",
                    dst_size as f64 / 1_000_000.0, src_size as f64 / 1_000_000.0,
                ));
                write_log(&mut log_writer, "  Output larger than source - remuxing");
                let _ = std::fs::remove_file(&temp_output_file);

                let remux_status = ffbin::ffmpeg_command()
                    .args(["-y", "-i", &item_full_path, "-map", "0", "-c", "copy", &output_str])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .await;

                match remux_status {
                    Ok(s) if s.success() => {
                        sink.log(&format!("  Remuxed source to {} → {}", ext.to_uppercase(), output_str));
                        write_log(&mut log_writer, &format!("  Remuxed to {}", ext.to_uppercase()));
                    }
                    _ => {
                        sink.log("  ERROR: Remux failed");
                        write_log(&mut log_writer, "  ERROR: Remux failed");
                        if temp_output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }
                        queue[idx].status = QueueItemStatus::Failed;
                        sink.queue_item_updated(idx, "Failed");
                        fail_count += 1;
                        continue;
                    }
                }
            } else {
                sink.log(&format!(
                    "  Done → {} ({:.1}MB from {:.1}MB)",
                    final_output_str, dst_size as f64 / 1_000_000.0, src_size as f64 / 1_000_000.0,
                ));
                write_log(&mut log_writer, &format!(
                    "  Done: {:.1}MB from {:.1}MB",
                    dst_size as f64 / 1_000_000.0, src_size as f64 / 1_000_000.0,
                ));
            }

            // Replace mode: delete source, rename temp to final path
            if is_replace_mode && temp_output_file.exists() {
                // Delete the original source file
                if let Err(e) = std::fs::remove_file(&item_full_path) {
                    let warn = format!("  WARNING: Could not delete source for replacement: {e}");
                    sink.log(&warn);
                    write_log(&mut log_writer, &warn);
                    // Don't fail - the encode succeeded, we just can't replace
                }
                // Rename temp to final
                if temp_output_file != output_file {
                    if let Err(e) = std::fs::rename(&temp_output_file, &output_file) {
                        let warn = format!("  WARNING: Could not rename temp to final path: {e}");
                        sink.log(&warn);
                        write_log(&mut log_writer, &warn);
                    } else {
                        let msg = format!("  Replaced source → {}", final_output_str);
                        sink.log(&msg);
                        write_log(&mut log_writer, &msg);
                    }
                }
            } else if settings.delete_source && temp_output_file.exists() {
                // Normal delete-source mode
                match std::fs::remove_file(&item_full_path) {
                    Ok(_) => {
                        sink.log("  Source file deleted");
                        write_log(&mut log_writer, "  Source deleted");
                    }
                    Err(e) => {
                        let warn = format!("  WARNING: Could not delete source: {e}");
                        sink.log(&warn);
                        write_log(&mut log_writer, &warn);
                    }
                }
            }

            queue[idx].status = QueueItemStatus::Done;
            sink.queue_item_updated(idx, "Done");
            done_count += 1;
        } else {
            sink.log("  ERROR: Output file not found after encode");
            write_log(&mut log_writer, "  ERROR: Output file not found");
            queue[idx].status = QueueItemStatus::Failed;
            sink.queue_item_updated(idx, "Failed");
            fail_count += 1;
        }
    }

    // Batch completion
    let batch_duration = batch_start.elapsed();
    let dur_string = format!(
        "{:02}:{:02}:{:02}",
        batch_duration.as_secs() / 3600,
        (batch_duration.as_secs() % 3600) / 60,
        batch_duration.as_secs() % 60
    );

    let status_msg = if was_cancelled { "cancelled" } else { "done" };
    let summary = format!(
        "Batch {}. Done: {}, Failed: {}, Skipped: {}. Duration: {}",
        status_msg, done_count, fail_count, skip_count, dur_string
    );
    sink.log("");
    sink.log(&summary);
    write_log(&mut log_writer, "");
    write_log(&mut log_writer, &summary);

    // Flush and close the log file (#7)
    if let Some(ref mut w) = log_writer {
        use std::io::Write;
        let _ = w.flush();
    }
    // Log file path was already printed at batch start; no need to repeat here.

    // Post-command
    if let Some(ref cmd) = settings.post_command {
        if !cmd.is_empty() {
            sink.log(&format!("Running post-command: {}", cmd));
            #[cfg(unix)]
            {
                let _ = tokio::process::Command::new("sh")
                    .args(["-c", cmd])
                    .status()
                    .await;
            }
            #[cfg(windows)]
            {
                let mut proc = tokio::process::Command::new("cmd");
                proc.args(["/C", cmd]);
                ffbin::hide_window(&mut proc);
                let _ = proc.status().await;
            }
        }
    }

    sink.batch_finished(done_count, fail_count, skip_count, &dur_string);

    (done_count, fail_count, skip_count, was_cancelled)
}

// ── Shared helpers ──────────────────────────────────────────────

/// Null device path for two-pass first-pass output.
#[cfg(windows)]
const NULL_DEVICE: &str = "NUL";
#[cfg(not(windows))]
const NULL_DEVICE: &str = "/dev/null";

/// Result from running an ffmpeg child with progress tracking.
struct FfmpegRunResult {
    exit_code: i32,
    was_cancelled_current: bool,
    was_cancelled_all: bool,
}

/// Build two-pass argument lists directly from components (#5).
/// Instead of cloning the base args and inserting at arbitrary positions,
/// this reconstructs the arg list with pass flags in the correct place.
///
/// For pass 1: strips the output path and replaces it with -f <fmt> NUL,
/// adds -an -sn (no audio/subs for analysis pass), and inserts -pass 1.
/// For pass 2: inserts -pass 2 and -passlogfile before the output path.
fn build_two_pass_args(
    base_args: &[String],
    pass_num: u8,
    passlog_prefix: &str,
    container_fmt: &str,
) -> Vec<String> {
    // The output path is always the last element in the base args
    let output_path = base_args.last().map(|s| s.as_str()).unwrap_or("");
    let args_without_output = &base_args[..base_args.len().saturating_sub(1)];

    let mut args: Vec<String> = Vec::with_capacity(base_args.len() + 8);
    args.extend(args_without_output.iter().cloned());

    // Insert pass flags before the output
    args.extend([
        "-pass".into(), pass_num.to_string(),
        "-passlogfile".into(), passlog_prefix.to_string(),
    ]);

    if pass_num == 1 {
        // Pass 1: analysis only - no audio/subs, output to null device
        args.extend([
            "-an".into(), "-sn".into(),
            "-f".into(), container_fmt.into(),
            NULL_DEVICE.into(),
        ]);
    } else {
        // Pass 2: full encode to the real output path
        args.push(output_path.to_string());
    }

    args
}

/// Spawn ffmpeg with the given args, stream stderr for progress, handle
/// pause/cancel, and return the result. Extracted to avoid duplicating the
/// poll loop for two-pass encoding and software fallback.
async fn run_ffmpeg_with_progress(
    args: &[String],
    file_duration: f64,
    pass: Option<(u8, u8)>,
    low_priority: bool,
    sink: &dyn EventSink,
    batch_control: &dyn BatchControl,
) -> Result<FfmpegRunResult, String> {
    let mut cmd = ffbin::ffmpeg_command();
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Set below-normal priority on the ffmpeg child process
    if low_priority {
        #[cfg(target_os = "windows")]
        {
            // BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW
            const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x00004000;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            cmd.creation_flags(BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW);
        }
    }

    let mut child = cmd.spawn()
        .map_err(|e| format!("Failed to launch ffmpeg: {e}"))?;

    // On Unix, renice the child after spawn (avoids libc dependency)
    #[cfg(unix)]
    if low_priority {
        if let Some(pid) = child.id() {
            let _ = std::process::Command::new("renice")
                .args(["-n", "10", "-p", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    sink.log(&format!("  ffmpeg PID: {}", child.id().unwrap_or(0)));

    // Stream stderr for progress using a blocking thread
    let stderr = child.stderr.take();
    let progress_secs = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let stderr_thread = stderr.map(|tokio_stderr| {
        let std_stderr = {
            #[cfg(windows)]
            {
                let owned = tokio_stderr.into_owned_handle().unwrap();
                std::process::ChildStderr::from(owned)
            }
            #[cfg(unix)]
            {
                let owned = tokio_stderr.into_owned_fd().unwrap();
                std::process::ChildStderr::from(owned)
            }
        };
        let progress = Arc::clone(&progress_secs);
        let dur = file_duration;
        std::thread::spawn(move || {
            use std::io::Read;
            let mut reader = std::io::BufReader::new(std_stderr);
            let mut buf = [0u8; 4096];
            let mut line_buf = Vec::<u8>::with_capacity(256);

            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        for &byte in &buf[..n] {
                            if byte == b'\r' || byte == b'\n' {
                                if !line_buf.is_empty() {
                                    if dur > 0.0 {
                                        if let Some(pos) = line_buf.windows(5).position(|w| w == b"time=") {
                                            let ts_start = pos + 5;
                                            let ts_end = line_buf[ts_start..].iter()
                                                .position(|&b| b == b' ' || b == b'\t')
                                                .map(|p| ts_start + p)
                                                .unwrap_or(line_buf.len());
                                            if let Ok(ts_str) = std::str::from_utf8(&line_buf[ts_start..ts_end]) {
                                                if let Some(secs) = parse_ffmpeg_time(ts_str) {
                                                    progress.store(
                                                        secs.to_bits(),
                                                        std::sync::atomic::Ordering::Relaxed,
                                                    );
                                                }
                                            }
                                        }
                                    }
                                    line_buf.clear();
                                }
                            } else {
                                line_buf.push(byte);
                            }
                        }
                    }
                    Err(_) => break,
                }
            }
        })
    });

    // Poll for cancellation/pause while waiting for ffmpeg, and emit progress
    let mut last_progress_emit = std::time::Instant::now()
        - std::time::Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if batch_control.should_cancel_current() || batch_control.should_cancel_all() {
            if let Some(mut stdin) = child.stdin.take() {
                use tokio::io::AsyncWriteExt;
                let _ = stdin.write_all(b"q").await;
                let _ = stdin.flush().await;
            }
            let graceful = tokio::time::timeout(
                std::time::Duration::from_secs(5), child.wait(),
            ).await;
            if graceful.is_err() {
                let _ = child.kill().await;
            }
            break;
        }

        while batch_control.is_paused() {
            if batch_control.should_cancel_current() || batch_control.should_cancel_all() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        if file_duration > 0.0 {
            let now = std::time::Instant::now();
            if now.duration_since(last_progress_emit).as_millis() >= 250 {
                let bits = progress_secs.load(std::sync::atomic::Ordering::Relaxed);
                let secs = f64::from_bits(bits);
                if secs > 0.0 {
                    let pct = (secs / file_duration * 100.0).min(100.0);
                    sink.file_progress(pct, secs, file_duration, pass);
                    last_progress_emit = now;
                }
            }
        }

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    let exit_status = child.wait().await;

    // Join stderr thread and emit final progress
    if let Some(handle) = stderr_thread {
        let _ = handle.join();
    }
    if file_duration > 0.0 {
        sink.file_progress(100.0, file_duration, file_duration, pass);
    }

    let exit_code = exit_status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

    Ok(FfmpegRunResult {
        exit_code,
        was_cancelled_current: batch_control.should_cancel_current(),
        was_cancelled_all: batch_control.should_cancel_all(),
    })
}

/// Clean up ffmpeg two-pass log files (e.g. prefix-0.log, prefix-0.log.mbtree).
fn cleanup_passlog_files(prefix: &str) {
    for suffix in &["-0.log", "-0.log.mbtree", "-0.log.temp", "-0.log.mbtree.temp"] {
        let path = format!("{}{}", prefix, suffix);
        let _ = std::fs::remove_file(&path);
    }
}

// ── Precision mode helpers ─────────────────────────────────────

/// Detect total system RAM in gigabytes (approximate, zero-dependency).
pub fn get_system_ram_gb() -> u64 {
    #[cfg(target_os = "windows")]
    {
        use std::mem;
        #[repr(C)]
        struct MemoryStatusEx {
            dw_length: u32,
            dw_memory_load: u32,
            ull_total_phys: u64,
            ull_avail_phys: u64,
            ull_total_page_file: u64,
            ull_avail_page_file: u64,
            ull_total_virtual: u64,
            ull_avail_virtual: u64,
            ull_avail_extended_virtual: u64,
        }
        extern "system" {
            fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> i32;
        }
        unsafe {
            let mut status: MemoryStatusEx = mem::zeroed();
            status.dw_length = mem::size_of::<MemoryStatusEx>() as u32;
            if GlobalMemoryStatusEx(&mut status) != 0 {
                return status.ull_total_phys / (1024 * 1024 * 1024);
            }
        }
        0
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(contents) = std::fs::read_to_string("/proc/meminfo") {
            for line in contents.lines() {
                if line.starts_with("MemTotal:") {
                    let kb: u64 = line.split_whitespace()
                        .nth(1)
                        .and_then(|s| s.parse().ok())
                        .unwrap_or(0);
                    return kb / (1024 * 1024);
                }
            }
        }
        0
    }
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sysctl")
            .args(["-n", "hw.memsize"])
            .output();
        if let Ok(o) = output {
            let s = String::from_utf8_lossy(&o.stdout);
            let bytes: u64 = s.trim().parse().unwrap_or(0);
            return bytes / (1024 * 1024 * 1024);
        }
        0
    }
    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    { 0 }
}

/// Determine the optimal rc-lookahead value based on available system RAM (#3).
/// Accepts a pre-cached RAM value to avoid re-querying the OS.
/// Returns 0 to leave ffmpeg's default, or a value up to 250.
/// Only meaningful for single-pass CRF - two-pass already has full-file data.
pub fn lookahead_for_ram_with_cache(ram_gb: u64) -> u32 {
    if ram_gb >= 16 {
        250
    } else if ram_gb >= 8 {
        120
    } else {
        0 // low RAM - precision mode uses two-pass instead of lookahead
    }
}

/// Convenience wrapper that queries system RAM and returns the lookahead.
/// Used by callers outside the encoding loop (e.g. CLI dry-run display).
pub fn lookahead_for_ram() -> u32 {
    lookahead_for_ram_with_cache(get_system_ram_gb())
}

/// Returns true if the system has insufficient RAM for extended lookahead (#3).
/// Accepts a pre-cached RAM value.
pub fn precision_needs_two_pass_with_ram(ram_gb: u64) -> bool {
    ram_gb < 8
}

/// Convenience wrapper for callers outside the encoding loop.
pub fn precision_needs_two_pass() -> bool {
    precision_needs_two_pass_with_ram(get_system_ram_gb())
}

/// Probe CRF viability by encoding three 10-second samples from 25%, 50%,
/// and 75% through the file and returning the average output bitrate in Mbps.
///
/// The sink receives progress updates as "Probing 1/3", "Probing 2/3", etc.
/// via batch_status. Returns None if the file is too short to sample (<120s)
/// or if any sample encode fails.
pub async fn probe_crf_viability(
    input_path: &str,
    duration_secs: f64,
    video_args: &[String],
    pix_fmt: &str,
    threads: u32,
    low_priority: bool,
    sink: &dyn EventSink,
    batch_control: &dyn BatchControl,
) -> Option<f64> {
    // Skip probing for files under 2 minutes - not worth the overhead
    if duration_secs < 120.0 {
        sink.log("  Precision probe: file too short, skipping viability check");
        return None;
    }

    let sample_duration = 10.0;
    let seek_points = [0.25, 0.50, 0.75];
    let mut total_bits: f64 = 0.0;
    let mut total_sample_secs: f64 = 0.0;

    for (i, &fraction) in seek_points.iter().enumerate() {
        if batch_control.should_cancel_current() || batch_control.should_cancel_all() {
            return None;
        }

        let seek_secs = duration_secs * fraction;
        let sample_num = i + 1;
        sink.batch_status(&format!("Probing CRF viability ({}/{})", sample_num, seek_points.len()));
        sink.file_progress(
            (sample_num as f64 - 1.0) / seek_points.len() as f64 * 100.0,
            0.0, 0.0, Some((sample_num as u8, seek_points.len() as u8)),
        );

        // Build sample encode args: seek, duration limit, video only, temp output
        let tmp_file = std::env::temp_dir()
            .join(format!("histv_crf_probe_{}_{}.mkv", std::process::id(), i));
        let tmp_str = tmp_file.to_string_lossy().to_string();

        let mut args: Vec<String> = vec![
            "-y".into(),
            "-ss".into(), format!("{:.2}", seek_secs),
            "-t".into(), format!("{:.2}", sample_duration),
            "-i".into(), input_path.to_string(),
            "-map".into(), "0:v:0".into(),
            "-an".into(), "-sn".into(),
        ];
        if threads > 0 {
            args.extend(vec!["-threads".into(), threads.to_string()]);
        }
        args.extend(video_args.iter().cloned());
        args.extend(vec!["-pix_fmt".into(), pix_fmt.to_string()]);
        args.push(tmp_str.clone());

        let result = run_ffmpeg_with_progress(
            &args, sample_duration, Some((sample_num as u8, seek_points.len() as u8)),
            low_priority, sink, batch_control,
        ).await;

        match result {
            Ok(r) if r.was_cancelled_current || r.was_cancelled_all => {
                let _ = std::fs::remove_file(&tmp_file);
                return None;
            }
            Ok(r) if r.exit_code != 0 => {
                sink.log(&format!("  Precision probe: sample {} failed (exit code {})", sample_num, r.exit_code));
                let _ = std::fs::remove_file(&tmp_file);
                return None;
            }
            Err(e) => {
                sink.log(&format!("  Precision probe: sample {} error: {}", sample_num, e));
                let _ = std::fs::remove_file(&tmp_file);
                return None;
            }
            _ => {}
        }

        // Stat the sample output to get its size
        let sample_size = std::fs::metadata(&tmp_file)
            .map(|m| m.len())
            .unwrap_or(0);
        let _ = std::fs::remove_file(&tmp_file);

        if sample_size == 0 {
            sink.log(&format!("  Precision probe: sample {} produced empty output", sample_num));
            return None;
        }

        total_bits += sample_size as f64 * 8.0;
        total_sample_secs += sample_duration;
    }

    sink.file_progress(100.0, 0.0, 0.0, None);

    if total_sample_secs > 0.0 {
        let avg_bps = total_bits / total_sample_secs;
        let avg_mbps = avg_bps / 1_000_000.0;
        sink.log(&format!("  Precision probe: estimated CRF bitrate {:.2}Mbps", avg_mbps));
        Some(avg_mbps)
    } else {
        None
    }
}

/// Assemble the full ffmpeg argument list from its component parts.
/// Used by both the primary encode and the software-fallback path
/// to avoid duplicating the invocation pattern.
fn assemble_ffmpeg_args(
    input_path: &str,
    video_args: &[&str],
    pix_fmt: &str,
    audio_map_args: &[String],
    audio_codec_args: &[String],
    output_path: &str,
    is_image_source: bool,
    threads: u32,
    tonemap_filter: Option<&str>,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-err_detect".into(), "ignore_err".into(),
        "-probesize".into(), "100M".into(),
        "-analyzeduration".into(), "100M".into(),
        "-y".into(),
        "-i".into(), input_path.to_string(),
        "-map".into(), "0:v:0".into(),
    ];

    // Thread limit (0 = ffmpeg default / auto)
    if threads > 0 {
        args.extend(vec!["-threads".into(), threads.to_string()]);
    }

    if is_image_source {
        // GIF/APNG: no audio or subtitle streams, pad odd dimensions,
        // force yuv420p (GIF decodes to rgb24/rgba).
        args.extend(vec!["-an".into(), "-sn".into()]);
        args.extend(video_args.iter().map(|s| s.to_string()));
        args.extend(vec![
            "-vf".into(), "pad=ceil(iw/2)*2:ceil(ih/2)*2".into(),
            "-pix_fmt".into(), "yuv420p".into(),
        ]);
        // MP4: put moov atom at front for streaming
        if output_path.ends_with(".mp4") {
            args.extend(vec!["-movflags".into(), "+faststart".into()]);
        }
    } else {
        // Normal video: per-stream audio maps + subtitle passthrough
        args.extend_from_slice(audio_map_args);
        args.extend(vec!["-map".into(), "0:s?".into()]);
        args.extend(video_args.iter().map(|s| s.to_string()));
        // Tonemap filter for HDR→SDR conversion
        if let Some(filter) = tonemap_filter {
            args.extend(vec!["-vf".into(), filter.to_string()]);
        }
        args.extend(vec!["-pix_fmt".into(), pix_fmt.to_string()]);
        args.extend_from_slice(audio_codec_args);
        args.extend(vec![
            "-c:s".into(), "copy".into(),
            "-disposition:s:0".into(), "default".into(),
        ]);
    }

    args.push(output_path.to_string());
    args
}

/// Build per-stream audio arguments from pre-probed audio stream data (§6.2).
/// This avoids spawning a separate ffprobe during encoding - audio metadata
/// was already collected during the initial probe and stored on QueueItem.
fn build_audio_args_from_probe(
    audio_streams: &[crate::queue::AudioStreamInfo],
    audio_encoder: &str,
    audio_cap: u32,
    sink: &dyn EventSink,
) -> (Vec<String>, Vec<String>) {
    if audio_encoder == "copy" {
        sink.log("  Audio: copying all streams");
        return (
            vec!["-map".into(), "0:a".into()],
            vec!["-c:a".into(), "copy".into()],
        );
    }

    if audio_streams.is_empty() {
        sink.log("  WARNING: No audio streams found");
        return (Vec::new(), Vec::new());
    }

    let mut map_args = Vec::new();
    let mut codec_args = Vec::new();
    let mut output_idx: u32 = 0;

    for stream in audio_streams {
        // Unknown codecs can't be decoded or muxed - skip them entirely
        if stream.codec == "unknown" {
            sink.log(&format!(
                "  WARNING: Audio {} has an unrecognised codec and will be excluded from the output",
                stream.index
            ));
            continue;
        }

        // Map this specific input audio stream
        map_args.extend(vec![
            "-map".into(), format!("0:a:{}", stream.index),
        ]);

        let should_copy =
            (stream.codec == audio_encoder || stream.codec == "copy")
            && stream.bitrate_kbps < audio_cap;

        if should_copy {
            codec_args.extend(vec![
                format!("-c:a:{}", output_idx),
                "copy".into(),
            ]);
            sink.log(&format!(
                "  Audio {} : {} @ {}kbps - copying",
                stream.index, stream.codec, stream.bitrate_kbps
            ));
        } else {
            let target_br = stream.bitrate_kbps.min(audio_cap);
            codec_args.extend(vec![
                format!("-c:a:{}", output_idx),
                audio_encoder.to_string(),
                format!("-b:a:{}", output_idx),
                format!("{}k", target_br),
            ]);
            sink.log(&format!(
                "  Audio {} : {} @ {}kbps - encoding to {} {}kbps",
                stream.index, stream.codec, stream.bitrate_kbps,
                audio_encoder, target_br
            ));
        }

        output_idx += 1;
    }

    (map_args, codec_args)
}

/// Execute a post-batch action (§7.6).
pub async fn execute_post_action(action: &str) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    let linux_user = std::env::var("USER").unwrap_or_default();

    let (cmd, args): (&str, Vec<&str>) = match action {
        #[cfg(target_os = "windows")]
        "Shutdown" => ("shutdown", vec!["/s", "/t", "0"]),
        #[cfg(target_os = "windows")]
        "Sleep" => ("rundll32", vec!["powrprof.dll,SetSuspendState", "0,1,0"]),
        #[cfg(target_os = "windows")]
        "Log Out" => ("shutdown", vec!["/l"]),

        #[cfg(target_os = "macos")]
        "Shutdown" => ("osascript", vec!["-e", "tell app \"System Events\" to shut down"]),
        #[cfg(target_os = "macos")]
        "Sleep" => ("pmset", vec!["sleepnow"]),
        #[cfg(target_os = "macos")]
        "Log Out" => ("osascript", vec!["-e", "tell app \"System Events\" to log out"]),

        #[cfg(target_os = "linux")]
        "Shutdown" => ("systemctl", vec!["poweroff"]),
        #[cfg(target_os = "linux")]
        "Sleep" => ("systemctl", vec!["suspend"]),
        #[cfg(target_os = "linux")]
        "Log Out" => ("loginctl", vec!["terminate-user", &linux_user]),

        _ => return Ok(()),
    };

    let mut proc = Command::new(cmd);
    proc.args(&args);
    ffbin::hide_window(&mut proc);
    proc.spawn()
        .map_err(|e| format!("Failed to execute {}: {}", action, e))?;

    Ok(())
}

/// Parse ffmpeg's time= format "HH:MM:SS.ms" or "HH:MM:SS" into seconds.
fn parse_ffmpeg_time(s: &str) -> Option<f64> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 3 {
        return None;
    }
    let h: f64 = parts[0].parse().ok()?;
    let m: f64 = parts[1].parse().ok()?;
    let sec: f64 = parts[2].parse().ok()?;
    Some(h * 3600.0 + m * 60.0 + sec)
}

/// Resolve the base directory for relative output paths.
/// On Linux/macOS: uses $OWD (set by AppImage to the launch directory),
/// then falls back to $HOME, then to the current directory.
/// On Windows: uses the executable's parent directory.
pub fn resolve_base_dir() -> std::path::PathBuf {
    #[cfg(target_os = "windows")]
    {
        // On Windows, CWD is typically the app's folder - but use exe dir to be safe
        if let Ok(exe) = std::env::current_exe() {
            if let Some(parent) = exe.parent() {
                return parent.to_path_buf();
            }
        }
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    }

    #[cfg(not(target_os = "windows"))]
    {
        // $OWD is set by AppImage to the directory the user launched from
        if let Ok(owd) = std::env::var("OWD") {
            let p = std::path::PathBuf::from(&owd);
            if p.exists() {
                return p;
            }
        }
        // Fall back to $HOME
        if let Ok(home) = std::env::var("HOME") {
            let p = std::path::PathBuf::from(&home);
            if p.exists() {
                return p;
            }
        }
        std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
    }
}