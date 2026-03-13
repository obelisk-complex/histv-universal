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

/// Per-encoder flag mapping for CRF mode — only valid for libx265/libx264.
/// Hardware encoders do not support CRF; fall back to CQP for them.
/// Accepts pre-formatted strings to avoid re-allocating per file.
pub fn crf_flags(encoder: &str, crf_str: &str, qi: &str, qp: &str) -> Vec<String> {
    match encoder {
        "libx265" | "libx264" => vec![
            "-preset".into(), "slow".into(),
            "-crf".into(), crf_str.into(),
        ],
        // HW encoders don't support CRF — fall back to CQP
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
/// `target_codec` is the target codec name (e.g. "hevc", "h264") — note
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
) -> EncodeDecision {
    let is_already_target = video_codec == target_codec;

    if bitrate_mbps <= threshold || bitrate_mbps <= 0.0 {
        if is_already_target && bitrate_mbps > 0.0 {
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
    } else if is_already_target && bitrate_mbps <= threshold * 1.15 {
        EncodeDecision::Copy
    } else {
        let target_bps = (threshold * 1_000_000.0) as u64;
        let peak_bps = (target_bps as f64 * 1.5) as u64;
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
                "Already {} at {:.2}Mbps (at/below target) — copy",
                codec_family, item_bitrate_mbps
            )
        }
        EncodeDecision::Vbr { peak_bps, .. } => {
            let peak_mbps = *peak_bps as f64 / 1_000_000.0;
            format!(
                "{:.2}Mbps [{}] {}x{} — VBR target {}Mbps peak {:.2}Mbps",
                item_bitrate_mbps, item_codec, item_width, item_height,
                threshold, peak_mbps
            )
        }
        EncodeDecision::Cqp { qi, qp } => {
            format!(
                "{:.2}Mbps [{}] {}x{} — CQP ({}/{})",
                item_bitrate_mbps, item_codec, item_width, item_height,
                qi, qp
            )
        }
        EncodeDecision::Crf { crf, .. } => {
            format!(
                "{:.2}Mbps [{}] {}x{} — CRF {}",
                item_bitrate_mbps, item_codec, item_width, item_height,
                crf
            )
        }
    }
}

// ── Encoder detection (§8) ──────────────────────────────────────

/// Detect available encoders by running `ffmpeg -encoders`, then verifying
/// each hardware encoder with a single-frame test encode.
/// Software encoders (libx265/libx264) skip the test — they always work.
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

        // Software encoders always work — skip test encode
        if !is_hw {
            sink.log(&format!("[detect] {enc_name} (SW) — available"));
            video_encoders.push(EncoderInfo {
                name: enc_name.to_string(),
                codec_family: family.to_string(),
                is_hardware: false,
            });
            continue;
        }

        // Hardware encoder — verify with a 1-frame test encode
        sink.log(&format!("[detect] {enc_name} — testing..."));
        if test_encode(enc_name).await {
            sink.log(&format!("[detect] {enc_name} (HW) — works"));
            video_encoders.push(EncoderInfo {
                name: enc_name.to_string(),
                codec_family: family.to_string(),
                is_hardware: true,
            });
        } else {
            sink.log(&format!("[detect] {enc_name} (HW) — not available (test encode failed)"));
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
    // Use a temp file — some hardware encoders fail with -f null
    let tmp_dir = std::env::temp_dir();
    let tmp_file = tmp_dir.join(format!("_histv_test_{}.mp4", encoder_name));
    let tmp_str = tmp_file.to_string_lossy().to_string();

    // 256x256 minimum — some HW encoders reject smaller resolutions
    // nv12 pixel format — required by most hardware encoders
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
    let mut log_lines: Vec<String> = Vec::new();
    let batch_start = std::time::Instant::now();
    let mut file_counter: u32 = 0;
    let mut was_cancelled = false;

    sink.batch_started();

    for &idx in &pending_indices {
        // Pause / cancel check
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

        sink.batch_status(&format!("[{}/{}] {}", file_counter, total, item_file_name));

        queue[idx].status = QueueItemStatus::Encoding;
        sink.queue_item_updated(idx, "Encoding");

        let ext = if settings.output_container == "mp4" { "mp4" } else { "mkv" };
        let is_replace_mode = settings.output_mode == "replace";

        // Determine output path based on output mode
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
                        if save_log { log_lines.push(err_msg); }
                        queue[idx].status = QueueItemStatus::Failed;
                        sink.queue_item_updated(idx, "Failed");
                        fail_count += 1;
                        continue;
                    }
                }
                let out = out_dir.join(format!("{}.{}", item_base_name, ext));
                (out.clone(), out)
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
                // "folder" — default: use the configured output folder
                let out = std::path::Path::new(&settings.output_folder)
                    .join(format!("{}.{}", item_base_name, ext));
                (out.clone(), out)
            }
        };
        let output_str = temp_output_file.to_string_lossy().to_string();
        let final_output_str = output_file.to_string_lossy().to_string();

        let log_msg = format!("[{}/{}] {}", file_counter, total, item_file_name);
        sink.log(&log_msg);
        if save_log { log_lines.push(log_msg); }

        // Overwrite check — in replace mode, we're replacing the source so
        // no overwrite prompt is needed. In other modes, check the final output.
        if !is_replace_mode && output_file.exists() {
            if !batch_control.overwrite_always() {
                let response = batch_control.overwrite_prompt(&final_output_str);
                match response.as_str() {
                    "no" | "skip" => {
                        sink.log("  Skipped (output exists)");
                        if save_log { log_lines.push("  Skipped (output exists)".into()); }
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
                    _ => {} // "yes" — proceed
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
        );

        // Build video args from the decision
        let mut video_args: Vec<String>;
        let mode_desc: String;

        match &decision {
            EncodeDecision::Copy => {
                video_args = vec!["-c:v".into(), "copy".into()];
                mode_desc = format!(
                    "  Already {} at {:.2}Mbps (at/below target) — copying video",
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
                    "  Video: {:.2}Mbps [{}] {}x{} — VBR target {}Mbps peak {:.2}Mbps",
                    item_video_bitrate_mbps, item_video_codec,
                    item_video_width, item_video_height,
                    settings.threshold, peak_mbps
                );
            }
            EncodeDecision::Cqp { .. } => {
                video_args = vec!["-c:v".into(), settings.video_encoder.clone()];
                video_args.extend(cqp_flags(&settings.video_encoder, &qi_str, &qp_str));
                mode_desc = format!(
                    "  Video: {:.2}Mbps [{}] {}x{} — CQP ({}/{})",
                    item_video_bitrate_mbps, item_video_codec,
                    item_video_width, item_video_height,
                    settings.qp_i, settings.qp_p
                );
            }
            EncodeDecision::Crf { crf, .. } => {
                video_args = vec!["-c:v".into(), settings.video_encoder.clone()];
                video_args.extend(crf_flags(&settings.video_encoder, &crf_str, &qi_str, &qp_str));
                mode_desc = format!(
                    "  Video: {:.2}Mbps [{}] {}x{} — CRF {}",
                    item_video_bitrate_mbps, item_video_codec,
                    item_video_width, item_video_height, crf
                );
            }
        }

        sink.log(&mode_desc);
        if save_log { log_lines.push(mode_desc); }

        // Build audio args
        let (audio_map_args, audio_codec_args) = build_audio_args_from_probe(
            &queue[idx].audio_streams, &settings.audio_encoder, settings.audio_cap, sink,
        );

        // Assemble ffmpeg command
        let video_arg_refs: Vec<&str> = video_args.iter().map(|s| s.as_str()).collect();
        let ffmpeg_args = assemble_ffmpeg_args(
            &item_full_path, &video_arg_refs, &settings.pix_fmt,
            &audio_map_args, &audio_codec_args, &output_str,
        );

        let cmd_line = format!("ffmpeg {}", ffmpeg_args.join(" "));
        sink.batch_command(&cmd_line);
        let cmd_log = format!("  CMD: {cmd_line}");
        sink.log(&cmd_log);
        if save_log { log_lines.push(cmd_log); }

        // Spawn ffmpeg
        let proc_start = std::time::Instant::now();
        let mut child = match ffbin::ffmpeg_command()
            .args(&ffmpeg_args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
        {
            Ok(c) => c,
            Err(e) => {
                let err_msg = format!("  ERROR: Failed to launch ffmpeg: {e}");
                sink.log(&err_msg);
                if save_log { log_lines.push(err_msg); }
                queue[idx].status = QueueItemStatus::Failed;
                sink.queue_item_updated(idx, "Failed");
                fail_count += 1;
                continue;
            }
        };

        sink.log(&format!("  ffmpeg PID: {}", child.id().unwrap_or(0)));

        // Stream stderr for progress using a blocking thread.
        // The thread parses ffmpeg's time= output and writes the current
        // seconds (as f64 bits) to an AtomicU64. The main polling loop
        // reads this and calls sink.file_progress() with it.
        let stderr = child.stderr.take();
        let file_duration = item_duration_secs;
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
            std::thread::spawn(move || {
                use std::io::Read;
                let mut reader = std::io::BufReader::new(std_stderr);
                let mut buf = [0u8; 4096];
                // Use a byte buffer instead of String to avoid UTF-8 allocation
                // per read() call. ffmpeg's time= output is pure ASCII.
                let mut line_buf = Vec::<u8>::with_capacity(256);

                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            for &byte in &buf[..n] {
                                if byte == b'\r' || byte == b'\n' {
                                    if !line_buf.is_empty() {
                                        if file_duration > 0.0 {
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

        // Poll for cancellation while waiting for ffmpeg, and emit progress
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

            // Emit progress from the stderr thread's parsed data
            if file_duration > 0.0 {
                let now = std::time::Instant::now();
                if now.duration_since(last_progress_emit).as_millis() >= 250 {
                    let bits = progress_secs.load(std::sync::atomic::Ordering::Relaxed);
                    let secs = f64::from_bits(bits);
                    if secs > 0.0 {
                        let pct = (secs / file_duration * 100.0).min(100.0);
                        sink.file_progress(pct, secs, file_duration);
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
            sink.file_progress(100.0, file_duration, file_duration);
        }

        let proc_duration = proc_start.elapsed();

        // Handle cancellation
        if batch_control.should_cancel_all() {
            sink.log("  Cancelled (batch cancel)");
            if save_log { log_lines.push("  Cancelled (batch cancel)".into()); }
            if output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }
            queue[idx].status = QueueItemStatus::Cancelled;
            sink.queue_item_updated(idx, "Cancelled");
            was_cancelled = true;
            break;
        }

        if batch_control.should_cancel_current() {
            sink.log("  Cancelled current file");
            if save_log { log_lines.push("  Cancelled".into()); }
            if output_file.exists() {
                let _ = std::fs::remove_file(&temp_output_file);
                sink.log("  Partial output removed");
            }
            queue[idx].status = QueueItemStatus::Cancelled;
            sink.queue_item_updated(idx, "Cancelled");
            continue;
        }

        // Check exit code
        let exit_code = exit_status
            .map(|s| s.code().unwrap_or(-1))
            .unwrap_or(-1);

        // Encoder failure fallback
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
                    if save_log { log_lines.push(format!("  Fallback to {}", sw_fallback)); }

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

                    // Reuse audio args from the primary encode — inputs haven't changed
                    let sw_ref: Vec<&str> = sw_video_args.iter().map(|s| s.as_str()).collect();
                    let sw_args = assemble_ffmpeg_args(
                        &item_full_path, &sw_ref, &settings.pix_fmt,
                        &audio_map_args, &audio_codec_args, &output_str,
                    );

                    let sw_cmd = format!("ffmpeg {}", sw_args.join(" "));
                    sink.batch_command(&sw_cmd);
                    let sw_cmd_log = format!("  CMD (fallback): {sw_cmd}");
                    sink.log(&sw_cmd_log);
                    if save_log { log_lines.push(sw_cmd_log); }

                    match ffbin::ffmpeg_command()
                        .args(&sw_args)
                        .stdin(std::process::Stdio::piped())
                        .stdout(std::process::Stdio::piped())
                        .stderr(std::process::Stdio::piped())
                        .spawn()
                    {
                        Ok(mut sw_child) => {
                            loop {
                                match sw_child.try_wait() {
                                    Ok(Some(_)) => break,
                                    Ok(None) => {}
                                    Err(_) => break,
                                }
                                if batch_control.should_cancel_current() || batch_control.should_cancel_all() {
                                    if let Some(mut stdin) = sw_child.stdin.take() {
                                        use tokio::io::AsyncWriteExt;
                                        let _ = stdin.write_all(b"q").await;
                                    }
                                    let g = tokio::time::timeout(
                                        std::time::Duration::from_secs(5), sw_child.wait(),
                                    ).await;
                                    if g.is_err() { let _ = sw_child.kill().await; }
                                    break;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                            }
                            let sw_code = sw_child.wait().await
                                .map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
                            if sw_code != 0 {
                                sink.log("  ERROR: Software encoder also failed — stopping batch");
                                if save_log { log_lines.push("  ERROR: Software encoder also failed".into()); }
                                if output_file.exists() { let _ = std::fs::remove_file(&temp_output_file); }
                                queue[idx].status = QueueItemStatus::Failed;
                                sink.queue_item_updated(idx, "Failed");
                                fail_count += 1;
                                break;
                            }
                        }
                        Err(e) => {
                            sink.log(&format!("  ERROR: Could not launch fallback: {e}"));
                            if save_log { log_lines.push(format!("  ERROR: Fallback launch failed: {e}")); }
                            queue[idx].status = QueueItemStatus::Failed;
                            sink.queue_item_updated(idx, "Failed");
                            fail_count += 1;
                            break;
                        }
                    }
                } else {
                    sink.log("  Stopping batch due to encoder failure");
                    if save_log { log_lines.push("  Batch stopped (encoder failure)".into()); }
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
            if save_log { log_lines.push(err_msg); }
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

            if dst_size > src_size && src_size > 0 {
                sink.log(&format!(
                    "  WARNING: Output ({:.1}MB) larger than source ({:.1}MB) — remuxing source instead",
                    dst_size as f64 / 1_000_000.0, src_size as f64 / 1_000_000.0,
                ));
                if save_log { log_lines.push("  Output larger than source — remuxing".into()); }
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
                        if save_log { log_lines.push(format!("  Remuxed to {}", ext.to_uppercase())); }
                    }
                    _ => {
                        sink.log("  ERROR: Remux failed");
                        if save_log { log_lines.push("  ERROR: Remux failed".into()); }
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
                if save_log {
                    log_lines.push(format!(
                        "  Done: {:.1}MB from {:.1}MB",
                        dst_size as f64 / 1_000_000.0, src_size as f64 / 1_000_000.0,
                    ));
                }
            }

            // Replace mode: delete source, rename temp to final path
            if is_replace_mode && temp_output_file.exists() {
                // Delete the original source file
                if let Err(e) = std::fs::remove_file(&item_full_path) {
                    let warn = format!("  WARNING: Could not delete source for replacement: {e}");
                    sink.log(&warn);
                    if save_log { log_lines.push(warn); }
                    // Don't fail — the encode succeeded, we just can't replace
                }
                // Rename temp to final
                if temp_output_file != output_file {
                    if let Err(e) = std::fs::rename(&temp_output_file, &output_file) {
                        let warn = format!("  WARNING: Could not rename temp to final path: {e}");
                        sink.log(&warn);
                        if save_log { log_lines.push(warn); }
                    } else {
                        let msg = format!("  Replaced source → {}", final_output_str);
                        sink.log(&msg);
                        if save_log { log_lines.push(msg); }
                    }
                }
            } else if settings.delete_source && temp_output_file.exists() {
                // Normal delete-source mode
                match std::fs::remove_file(&item_full_path) {
                    Ok(_) => {
                        sink.log("  Source file deleted");
                        if save_log { log_lines.push("  Source deleted".into()); }
                    }
                    Err(e) => {
                        let warn = format!("  WARNING: Could not delete source: {e}");
                        sink.log(&warn);
                        if save_log { log_lines.push(warn); }
                    }
                }
            }

            queue[idx].status = QueueItemStatus::Done;
            sink.queue_item_updated(idx, "Done");
            done_count += 1;
        } else {
            sink.log("  ERROR: Output file not found after encode");
            if save_log { log_lines.push("  ERROR: Output file not found".into()); }
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
    if save_log {
        log_lines.push(String::new());
        log_lines.push(summary);
    }

    // Save log
    if settings.save_log {
        let log_filename = format!(
            "encode_log_{}.txt",
            chrono::Local::now().format("%Y%m%d_%H%M%S")
        );
        let log_path = std::path::Path::new(&settings.output_folder).join(log_filename);
        match std::fs::write(&log_path, log_lines.join("\n")) {
            Ok(_) => sink.log(&format!("  Log saved to {}", log_path.display())),
            Err(e) => sink.log(&format!("  WARNING: Could not save log: {e}")),
        }
    }

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
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-err_detect".into(), "ignore_err".into(),
        "-probesize".into(), "100M".into(),
        "-analyzeduration".into(), "100M".into(),
        "-y".into(),
        "-i".into(), input_path.to_string(),
        "-map".into(), "0:v:0".into(),
    ];
    // Per-stream audio maps (skipping unknown codecs) instead of blanket -map 0:a
    args.extend_from_slice(audio_map_args);
    args.extend(vec!["-map".into(), "0:s?".into()]);
    args.extend(video_args.iter().map(|s| s.to_string()));
    args.extend(vec!["-pix_fmt".into(), pix_fmt.to_string()]);
    args.extend_from_slice(audio_codec_args);
    args.extend(vec![
        "-c:s".into(), "copy".into(),
        "-disposition:s:0".into(), "default".into(),
        output_path.to_string(),
    ]);
    args
}

/// Build per-stream audio arguments from pre-probed audio stream data (§6.2).
/// This avoids spawning a separate ffprobe during encoding — audio metadata
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
        // Unknown codecs can't be decoded or muxed — skip them entirely
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
                "  Audio {} : {} @ {}kbps — copying",
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
                "  Audio {} : {} @ {}kbps — encoding to {} {}kbps",
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