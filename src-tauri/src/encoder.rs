use crate::queue::{QueueItem, QueueItemStatus};
use crate::AppState;
use crate::ffmpeg as ffbin;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::Arc;
use tauri::{AppHandle, Emitter};
use tokio::io::AsyncWriteExt;
use tokio::process::Command;

// ── Per-file progress event payload (#12 — avoids serde_json::json! allocation) ──

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct FileProgress {
    percent: f64,
    time_secs: f64,
    total_secs: f64,
}

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

// ── Encoder detection (§8) ──────────────────────────────────────

/// Detect available encoders by running `ffmpeg -encoders`, then verifying
/// each hardware encoder with a single-frame test encode.
/// Software encoders (libx265/libx264) skip the test — they always work.
pub async fn detect_encoders(app: &AppHandle) -> (Vec<EncoderInfo>, Vec<String>) {
    let _ = app.emit("log", "[detect] Starting encoder detection...");

    let output = match ffbin::ffmpeg_command()
        .args(["-encoders"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
    {
        Ok(child) => match child.wait_with_output().await {
            Ok(o) => o,
            Err(e) => {
                let _ = app.emit("log", format!("[detect] ffmpeg wait error: {e}"));
                return fallback_encoders();
            }
        },
        Err(e) => {
            let _ = app.emit("log", format!("[detect] ffmpeg not found: {e}"));
            return fallback_encoders();
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut video_encoders: Vec<EncoderInfo> = Vec::new();
    let mut audio_encoders: Vec<String> = Vec::new();

    // Check each video encoder in priority order
    for &enc_name in HEVC_PRIORITY.iter().chain(H264_PRIORITY.iter()) {
        // Match encoder name as a word in the output
        let found = stdout.lines().any(|line| {
            line.split_whitespace()
                .any(|token| token == enc_name)
        });

        if !found {
            let _ = app.emit("log", format!("[detect] {enc_name} not listed in ffmpeg"));
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
            let _ = app.emit("log", format!("[detect] {enc_name} (SW) — available"));
            video_encoders.push(EncoderInfo {
                name: enc_name.to_string(),
                codec_family: family.to_string(),
                is_hardware: false,
            });
            continue;
        }

        // Hardware encoder — verify with a 1-frame test encode
        let _ = app.emit("log", format!("[detect] {enc_name} — testing..."));
        if test_encode(enc_name).await {
            let _ = app.emit("log", format!("[detect] {enc_name} (HW) — works"));
            video_encoders.push(EncoderInfo {
                name: enc_name.to_string(),
                codec_family: family.to_string(),
                is_hardware: true,
            });
        } else {
            let _ = app.emit("log", format!("[detect] {enc_name} (HW) — not available (test encode failed)"));
        }
    }

    // Check audio encoders
    for &aenc in AUDIO_ENCODERS {
        let found = stdout.lines().any(|line| {
            line.split_whitespace().any(|token| token == aenc)
        });
        if found {
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
    let _ = app.emit(
        "log",
        format!(
            "[detect] Detection complete: {} encoders ({} video, {} audio)",
            total,
            video_encoders.len(),
            audio_encoders.len()
        ),
    );

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

/// Assemble the full ffmpeg argument list from its component parts.
/// Used by both the primary encode and the software-fallback path
/// to avoid duplicating the invocation pattern.
fn assemble_ffmpeg_args(
    input_path: &str,
    video_args: &[&str],
    pix_fmt: &str,
    audio_args: &[String],
    output_path: &str,
) -> Vec<String> {
    let mut args: Vec<String> = vec![
        "-err_detect".into(), "ignore_err".into(),
        "-probesize".into(), "100M".into(),
        "-analyzeduration".into(), "100M".into(),
        "-y".into(),
        "-i".into(), input_path.to_string(),
        "-map".into(), "0:v:0".into(),
        "-map".into(), "0:a".into(),
        "-map".into(), "0:s?".into(),
    ];
    args.extend(video_args.iter().map(|s| s.to_string()));
    args.extend(vec!["-pix_fmt".into(), pix_fmt.to_string()]);
    args.extend_from_slice(audio_args);
    args.extend(vec![
        "-c:s".into(), "copy".into(),
        "-disposition:s:0".into(), "default".into(),
        output_path.to_string(),
    ]);
    args
}

/// Start the batch encoding loop in a background task.
pub async fn start_batch_encode(
    app: AppHandle,
    state: Arc<AppState>,
    settings: serde_json::Value,
) -> Result<(), String> {
    // Parse settings from the frontend
    let output_folder = settings["outputFolder"]
        .as_str()
        .unwrap_or("output")
        .to_string();
    let output_container = settings["outputContainer"]
        .as_str()
        .unwrap_or("mkv")
        .to_string();
    let output_next_to_input = settings["outputNextToInput"]
        .as_bool()
        .unwrap_or(false);
    let threshold: f64 = settings["targetBitrate"]
        .as_f64()
        .unwrap_or(5.0);
    let qp_i: u32 = settings["qpI"].as_u64().unwrap_or(20) as u32;
    let qp_p: u32 = settings["qpP"].as_u64().unwrap_or(22) as u32;
    let crf_val: u32 = settings["crf"].as_u64().unwrap_or(20) as u32;
    let rate_control_mode = settings["rateControlMode"]
        .as_str()
        .unwrap_or("QP")
        .to_string();
    let video_encoder = settings["videoEncoder"]
        .as_str()
        .unwrap_or("libx265")
        .to_string();
    let codec_family = settings["codecFamily"]
        .as_str()
        .unwrap_or("HEVC")
        .to_string();
    let audio_encoder = settings["audioEncoder"]
        .as_str()
        .unwrap_or("ac3")
        .to_string();
    let audio_cap: u32 = settings["audioBitrateCap"]
        .as_u64()
        .unwrap_or(640) as u32;
    let pix_fmt = settings["pixFmt"]
        .as_str()
        .unwrap_or("yuv420p")
        .to_string();
    let overwrite = settings["overwrite"].as_bool().unwrap_or(false);
    let delete_source = settings["deleteSource"].as_bool().unwrap_or(false);
    let save_log = settings["saveLog"].as_bool().unwrap_or(false);
    let show_toast = settings["showToast"].as_bool().unwrap_or(false);
    let post_action = settings["postAction"]
        .as_str()
        .unwrap_or("None")
        .to_string();
    let post_countdown: u32 = settings["postCountdown"]
        .as_u64()
        .unwrap_or(0) as u32;

    let sw_fallback = software_fallback(&codec_family).to_string();

    // Pre-format numeric values as strings once, reused across all files
    let qi_str = qp_i.to_string();
    let qp_str = qp_p.to_string();
    let crf_str = crf_val.to_string();

    // Collect pending indices
    let pending_indices: Vec<usize> = {
        let q = state.queue.lock().await;
        q.iter()
            .enumerate()
            .filter(|(_, item)| item.status == QueueItemStatus::Pending)
            .map(|(i, _)| i)
            .collect()
    };

    if pending_indices.is_empty() {
        return Err("No pending files in the queue.".into());
    }

    // Create output folder if needed (only when not using per-input output)
    if !output_next_to_input {
        if !Path::new(&output_folder).exists() {
            std::fs::create_dir_all(&output_folder)
                .map_err(|e| format!("Could not create output folder: {e}"))?;
        }
    }

    // Set batch running
    {
        let mut b = state.batch.lock().await;
        b.running = true;
        b.cancel_current = false;
        b.cancel_all = false;
        b.paused = false;
        b.overwrite_always = false;
        b.hw_fallback_offered = false;
        b.overwrite_response = None;
        b.fallback_response = None;
    }
    let _ = app.emit("batch-started", ());

    // Spawn the encoding loop
    let state_clone = state.clone();
    let app_clone = app.clone();
    tauri::async_runtime::spawn(async move {
        let total = pending_indices.len();
        let mut done_count: u32 = 0;
        let mut fail_count: u32 = 0;
        let mut skip_count: u32 = 0;
        let mut log_lines: Vec<String> = Vec::new();
        let batch_start = std::time::Instant::now();
        let mut file_counter: u32 = 0;

        for &idx in &pending_indices {
            // ── Pause / cancel check (single lock acquisition) ──
            loop {
                let mut b = state_clone.batch.lock().await;
                if b.cancel_all {
                    // Drop lock before breaking out of both loops
                    drop(b);
                    break;
                }
                if !b.paused {
                    b.cancel_current = false;
                    drop(b);
                    break;
                }
                drop(b);
                tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            }

            // Check if we broke out due to cancel_all
            {
                let b = state_clone.batch.lock().await;
                if b.cancel_all {
                    break;
                }
            }

            file_counter += 1;
            let _ = app_clone.emit(
                "batch-progress",
                serde_json::json!({
                    "current": file_counter,
                    "total": total,
                }),
            );

            // Get item info
            let item: QueueItem = {
                let q = state_clone.queue.lock().await;
                if idx >= q.len() {
                    continue;
                }
                q[idx].clone()
            };

            let _ = app_clone.emit(
                "batch-status",
                format!("[{}/{}] {}", file_counter, total, item.file_name),
            );

            // Update queue status to Encoding
            {
                let mut q = state_clone.queue.lock().await;
                if idx < q.len() {
                    q[idx].status = QueueItemStatus::Encoding;
                }
            }
            let _ = app_clone.emit("queue-item-updated", (idx, "Encoding"));

            let ext = if output_container == "mp4" { "mp4" } else { "mkv" };
            let actual_output_folder = if output_next_to_input {
                // Create an "output" subfolder next to the input file
                let input_dir = Path::new(&item.full_path)
                    .parent()
                    .unwrap_or_else(|| Path::new("."));
                let out_dir = input_dir.join("output");
                if !out_dir.exists() {
                    if let Err(e) = std::fs::create_dir_all(&out_dir) {
                        let err_msg = format!("  ERROR: Could not create output folder next to input: {e}");
                        let _ = app_clone.emit("log", &err_msg);
                        log_lines.push(err_msg);
                        {
                            let mut q = state_clone.queue.lock().await;
                            if idx < q.len() {
                                q[idx].status = QueueItemStatus::Failed;
                            }
                        }
                        let _ = app_clone.emit("queue-item-updated", (idx, "Failed"));
                        fail_count += 1;
                        continue;
                    }
                }
                out_dir.to_string_lossy().to_string()
            } else {
                output_folder.clone()
            };
            let output_file = Path::new(&actual_output_folder)
                .join(format!("{}.{}", item.base_name, ext));
            let output_str = output_file.to_string_lossy().to_string();

            // Log file being processed
            let log_msg = format!("[{}/{}] {}", file_counter, total, item.file_name);
            let _ = app_clone.emit("log", &log_msg);
            log_lines.push(log_msg);

            // Overwrite check (§7.3)
            if output_file.exists() && !overwrite {
                let ow_always = {
                    let b = state_clone.batch.lock().await;
                    b.overwrite_always
                };
                if !ow_always {
                    // Ask the frontend
                    let _ = app_clone.emit(
                        "overwrite-prompt",
                        output_str.clone(),
                    );

                    // Wait for response
                    let response = wait_for_response(&state_clone, |b| {
                        b.overwrite_response.take()
                    })
                    .await;

                    match response.as_str() {
                        "no" => {
                            let _ = app_clone.emit("log", "  Skipped (output exists)");
                            log_lines.push("  Skipped (output exists)".into());
                            {
                                let mut q = state_clone.queue.lock().await;
                                if idx < q.len() {
                                    q[idx].status = QueueItemStatus::Skipped;
                                }
                            }
                            let _ = app_clone.emit("queue-item-updated", (idx, "Skipped"));
                            skip_count += 1;
                            continue;
                        }
                        "always" => {
                            let mut b = state_clone.batch.lock().await;
                            b.overwrite_always = true;
                        }
                        _ => {} // "yes" — proceed
                    }
                }
            }

            // Determine encoding strategy (§10.3)
            let bitrate_mbps = item.video_bitrate_mbps;
            let target_codec_name = if codec_family == "H.264" {
                "h264"
            } else {
                "hevc"
            };
            let is_already_target = (target_codec_name == "hevc" && item.video_codec == "hevc")
                || (target_codec_name == "h264" && item.video_codec == "h264");

            let mut video_args: Vec<String>;
            let mode_desc: String;

            if bitrate_mbps <= threshold || bitrate_mbps <= 0.0 {
                if is_already_target && bitrate_mbps > 0.0 {
                    // Copy
                    video_args = vec!["-c:v".into(), "copy".into()];
                    mode_desc = format!(
                        "  Already {} at {:.2}Mbps (at/below target) — copying video",
                        codec_family,
                        bitrate_mbps
                    );
                } else if rate_control_mode == "CRF" {
                    // CRF transcode (software encoders only; crf_flags falls back to CQP for HW)
                    video_args = vec!["-c:v".into(), video_encoder.clone()];
                    video_args.extend(crf_flags(&video_encoder, &crf_str, &qi_str, &qp_str));
                    mode_desc = format!(
                        "  Video: {:.2}Mbps [{}] {}x{} — CRF {}",
                        bitrate_mbps,
                        item.video_codec,
                        item.video_width,
                        item.video_height,
                        crf_val
                    );
                } else {
                    // CQP transcode
                    video_args = vec!["-c:v".into(), video_encoder.clone()];
                    video_args.extend(cqp_flags(&video_encoder, &qi_str, &qp_str));
                    mode_desc = format!(
                        "  Video: {:.2}Mbps [{}] {}x{} — CQP ({}/{})",
                        bitrate_mbps,
                        item.video_codec,
                        item.video_width,
                        item.video_height,
                        qp_i,
                        qp_p
                    );
                }
            } else {
                // VBR
                let target_bps = (threshold * 1_000_000.0) as u64;
                let peak_bps = (target_bps as f64 * 1.5) as u64;
                let target_str = target_bps.to_string();
                let peak_str = peak_bps.to_string();
                video_args = vec!["-c:v".into(), video_encoder.clone()];
                video_args.extend(vbr_flags(&video_encoder, &target_str, &peak_str));
                let peak_mbps = peak_bps as f64 / 1_000_000.0;
                mode_desc = format!(
                    "  Video: {:.2}Mbps [{}] {}x{} — VBR target {}Mbps peak {:.2}Mbps",
                    bitrate_mbps,
                    item.video_codec,
                    item.video_width,
                    item.video_height,
                    threshold,
                    peak_mbps
                );
            };

            let _ = app_clone.emit("log", &mode_desc);
            log_lines.push(mode_desc);

            // Build audio args from pre-probed stream data (§6.2)
            let audio_args = build_audio_args_from_probe(
                &item.audio_streams,
                &audio_encoder,
                audio_cap,
                &app_clone,
            );

            // Assemble full ffmpeg command (§10.3)
            let video_arg_refs: Vec<&str> = video_args.iter().map(|s| s.as_str()).collect();
            let ffmpeg_args = assemble_ffmpeg_args(
                &item.full_path,
                &video_arg_refs,
                &pix_fmt,
                &audio_args,
                &output_str,
            );

            let cmd_line = format!("ffmpeg {}", ffmpeg_args.join(" "));
            let _ = app_clone.emit("batch-command", &cmd_line);
            let _ = app_clone.emit("log", format!("  CMD: {}", cmd_line));
            log_lines.push(format!("  CMD: {}", cmd_line));

            // Spawn ffmpeg (§10.2)
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
                    let _ = app_clone.emit("log", &err_msg);
                    log_lines.push(err_msg);
                    {
                        let mut q = state_clone.queue.lock().await;
                        if idx < q.len() {
                            q[idx].status = QueueItemStatus::Failed;
                        }
                    }
                    let _ = app_clone.emit("queue-item-updated", (idx, "Failed"));
                    fail_count += 1;
                    continue;
                }
            };

            let _ = app_clone.emit("log", format!("  ffmpeg PID: {}", child.id().unwrap_or(0)));

            // Stream stderr for progress (§10.2)
            // ffmpeg uses \r to overwrite progress lines in-place, so we
            // read bytes and split on both \r and \n to catch every update.
            let stderr = child.stderr.take();
            let app_for_stderr = app_clone.clone();
            let file_duration = item.duration_secs;
            let stderr_task = if let Some(stderr) = stderr {
                Some(tauri::async_runtime::spawn(async move {
                    use tokio::io::AsyncReadExt;
                    let mut reader = tokio::io::BufReader::new(stderr);
                    let mut buf = vec![0u8; 4096];
                    let mut line_buf = String::new();
                    // Throttle progress events to at most every 250ms (#5)
                    let mut last_progress_emit = std::time::Instant::now()
                        - std::time::Duration::from_millis(500);

                    loop {
                        match reader.read(&mut buf).await {
                            Ok(0) => break, // EOF
                            Ok(n) => {
                                let chunk = String::from_utf8_lossy(&buf[..n]);
                                for ch in chunk.chars() {
                                    if ch == '\r' || ch == '\n' {
                                        if !line_buf.is_empty() {
                                            let _ = app_for_stderr.emit("ffmpeg-stderr", &line_buf);
                                            // Parse ffmpeg progress: extract time=HH:MM:SS.ms
                                            if file_duration > 0.0 {
                                                if let Some(pos) = line_buf.find("time=") {
                                                    let now = std::time::Instant::now();
                                                    if now.duration_since(last_progress_emit).as_millis() >= 250 {
                                                        let time_str = &line_buf[pos + 5..];
                                                        let end = time_str.find(|c: char| c == ' ' || c == '\t')
                                                            .unwrap_or(time_str.len());
                                                        let ts = &time_str[..end];
                                                        if let Some(secs) = parse_ffmpeg_time(ts) {
                                                            let pct = (secs / file_duration * 100.0).min(100.0);
                                                            let progress = FileProgress {
                                                                percent: pct,
                                                                time_secs: secs,
                                                                total_secs: file_duration,
                                                            };
                                                            let _ = app_for_stderr.emit("file-progress", &progress);
                                                            last_progress_emit = now;
                                                        }
                                                    }
                                                }
                                            }
                                            line_buf.clear();
                                        }
                                    } else {
                                        line_buf.push(ch);
                                    }
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    // Flush any remaining content
                    if !line_buf.is_empty() {
                        let _ = app_for_stderr.emit("ffmpeg-stderr", &line_buf);
                    }
                    // Emit final 100% progress
                    if file_duration > 0.0 {
                        let progress = FileProgress {
                            percent: 100.0,
                            time_secs: file_duration,
                            total_secs: file_duration,
                        };
                        let _ = app_for_stderr.emit("file-progress", &progress);
                    }
                }))
            } else {
                None
            };

            // Poll for cancellation while waiting for ffmpeg
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) => {}
                    Err(_) => break,
                }

                let should_cancel = {
                    let b = state_clone.batch.lock().await;
                    b.cancel_current || b.cancel_all
                };

                if should_cancel {
                    // Graceful stop: write 'q' to stdin (§10.4)
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(b"q").await;
                        let _ = stdin.flush().await;
                    }

                    // Wait up to 5 seconds
                    let graceful = tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        child.wait(),
                    )
                    .await;

                    if graceful.is_err() {
                        // Force kill
                        let _ = child.kill().await;
                    }
                    break;
                }

                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }

            // Wait for process to fully exit
            let exit_status = child.wait().await;
            let proc_duration = proc_start.elapsed();

            // Clean up stderr reader task
            if let Some(task) = stderr_task {
                task.abort();
            }

            // Handle cancellation
            let (cancel_current, cancel_all) = {
                let b = state_clone.batch.lock().await;
                (b.cancel_current, b.cancel_all)
            };

            if cancel_current && !cancel_all {
                let _ = app_clone.emit("log", "  Cancelled current file");
                log_lines.push("  Cancelled".into());
                if output_file.exists() {
                    let _ = std::fs::remove_file(&output_file);
                    let _ = app_clone.emit("log", "  Partial output removed");
                }
                {
                    let mut q = state_clone.queue.lock().await;
                    if idx < q.len() {
                        q[idx].status = QueueItemStatus::Cancelled;
                    }
                }
                let _ = app_clone.emit("queue-item-updated", (idx, "Cancelled"));
                continue;
            }

            if cancel_all {
                let _ = app_clone.emit("log", "  Cancelled (batch cancel)");
                log_lines.push("  Cancelled (batch cancel)".into());
                if output_file.exists() {
                    let _ = std::fs::remove_file(&output_file);
                }
                {
                    let mut q = state_clone.queue.lock().await;
                    if idx < q.len() {
                        q[idx].status = QueueItemStatus::Cancelled;
                    }
                }
                let _ = app_clone.emit("queue-item-updated", (idx, "Cancelled"));
                break;
            }

            // Check exit code
            let exit_code = exit_status
                .as_ref()
                .map(|s| s.code().unwrap_or(-1))
                .unwrap_or(-1);

            // Encoder failure fallback (§10.6)
            if exit_code != 0
                && proc_duration.as_secs() < 30
                && !is_already_target
                && video_encoder != sw_fallback
            {
                let already_offered = {
                    let b = state_clone.batch.lock().await;
                    b.hw_fallback_offered
                };
                if !already_offered {
                    {
                        let mut b = state_clone.batch.lock().await;
                        b.hw_fallback_offered = true;
                    }
                    let _ = app_clone.emit("fallback-prompt", item.file_name.clone());

                    let response = wait_for_response(&state_clone, |b| {
                        b.fallback_response.take()
                    })
                    .await;

                    if response == "yes" {
                        let _ = app_clone.emit(
                            "log",
                            format!("  Falling back to software encoder ({})...", sw_fallback),
                        );
                        log_lines.push(format!("  Fallback to {}", sw_fallback));

                        if output_file.exists() {
                            let _ = std::fs::remove_file(&output_file);
                        }

                        // Rebuild args with software encoder
                        let mut sw_video_args: Vec<String>;
                        if bitrate_mbps <= threshold {
                            sw_video_args = vec!["-c:v".into(), sw_fallback.clone()];
                            sw_video_args.extend(cqp_flags(&sw_fallback, &qi_str, &qp_str));
                        } else {
                            let target_bps = (threshold * 1_000_000.0) as u64;
                            let peak_bps = (target_bps as f64 * 1.5) as u64;
                            let target_str = target_bps.to_string();
                            let peak_str = peak_bps.to_string();
                            sw_video_args = vec!["-c:v".into(), sw_fallback.clone()];
                            sw_video_args.extend(vbr_flags(&sw_fallback, &target_str, &peak_str));
                        }

                        // Re-use same audio args from probe data
                        let sw_audio = build_audio_args_from_probe(
                            &item.audio_streams,
                            &audio_encoder,
                            audio_cap,
                            &app_clone,
                        );
                        let sw_video_ref: Vec<&str> = sw_video_args.iter().map(|s| s.as_str()).collect();
                        let sw_ffmpeg_args = assemble_ffmpeg_args(
                            &item.full_path,
                            &sw_video_ref,
                            &pix_fmt,
                            &sw_audio,
                            &output_str,
                        );

                        let sw_cmd = format!("ffmpeg {}", sw_ffmpeg_args.join(" "));
                        let _ = app_clone.emit("batch-command", &sw_cmd);
                        let _ = app_clone.emit("log", format!("  CMD (fallback): {}", sw_cmd));
                        log_lines.push(format!("  CMD (fallback): {}", sw_cmd));

                        match ffbin::ffmpeg_command()
                            .args(&sw_ffmpeg_args)
                            .stdin(std::process::Stdio::piped())
                            .stdout(std::process::Stdio::piped())
                            .stderr(std::process::Stdio::piped())
                            .spawn()
                        {
                            Ok(mut sw_child) => {
                                // Wait with cancel support
                                loop {
                                    match sw_child.try_wait() {
                                        Ok(Some(_)) => break,
                                        Ok(None) => {}
                                        Err(_) => break,
                                    }
                                    let should_cancel = {
                                        let b = state_clone.batch.lock().await;
                                        b.cancel_current || b.cancel_all
                                    };
                                    if should_cancel {
                                        if let Some(mut stdin) = sw_child.stdin.take() {
                                            let _ = stdin.write_all(b"q").await;
                                        }
                                        let graceful = tokio::time::timeout(
                                            std::time::Duration::from_secs(5),
                                            sw_child.wait(),
                                        ).await;
                                        if graceful.is_err() {
                                            let _ = sw_child.kill().await;
                                        }
                                        break;
                                    }
                                    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
                                }

                                let sw_exit = sw_child.wait().await;
                                let sw_code = sw_exit.as_ref().map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);

                                if sw_code != 0 {
                                    let _ = app_clone.emit("log", "  ERROR: Software encoder also failed — stopping batch");
                                    log_lines.push("  ERROR: Software encoder also failed".into());
                                    if output_file.exists() {
                                        let _ = std::fs::remove_file(&output_file);
                                    }
                                    {
                                        let mut q = state_clone.queue.lock().await;
                                        if idx < q.len() {
                                            q[idx].status = QueueItemStatus::Failed;
                                        }
                                    }
                                    let _ = app_clone.emit("queue-item-updated", (idx, "Failed"));
                                    fail_count += 1;
                                    break; // Stop batch
                                }
                                // Fallback succeeded — fall through to size check
                            }
                            Err(e) => {
                                let _ = app_clone.emit("log", format!("  ERROR: Could not launch fallback: {e}"));
                                log_lines.push(format!("  ERROR: Fallback launch failed: {e}"));
                                {
                                    let mut q = state_clone.queue.lock().await;
                                    if idx < q.len() {
                                        q[idx].status = QueueItemStatus::Failed;
                                    }
                                }
                                let _ = app_clone.emit("queue-item-updated", (idx, "Failed"));
                                fail_count += 1;
                                break;
                            }
                        }
                    } else {
                        // User said no to fallback — stop batch
                        let _ = app_clone.emit("log", "  Stopping batch due to encoder failure");
                        log_lines.push("  Batch stopped (encoder failure)".into());
                        if output_file.exists() {
                            let _ = std::fs::remove_file(&output_file);
                        }
                        {
                            let mut q = state_clone.queue.lock().await;
                            if idx < q.len() {
                                q[idx].status = QueueItemStatus::Failed;
                            }
                        }
                        let _ = app_clone.emit("queue-item-updated", (idx, "Failed"));
                        fail_count += 1;
                        break;
                    }
                }
            } else if exit_code != 0 {
                let err_msg = format!("  ERROR: ffmpeg exited with code {}", exit_code);
                let _ = app_clone.emit("log", &err_msg);
                log_lines.push(err_msg);
                if output_file.exists() {
                    let _ = std::fs::remove_file(&output_file);
                    let _ = app_clone.emit("log", "  Failed output removed");
                }
                {
                    let mut q = state_clone.queue.lock().await;
                    if idx < q.len() {
                        q[idx].status = QueueItemStatus::Failed;
                    }
                }
                let _ = app_clone.emit("queue-item-updated", (idx, "Failed"));
                fail_count += 1;
                continue;
            }

            // Post-encode size check (§10.7)
            if output_file.exists() {
                let src_size = std::fs::metadata(&item.full_path)
                    .map(|m| m.len())
                    .unwrap_or(0);
                let dst_size = std::fs::metadata(&output_file)
                    .map(|m| m.len())
                    .unwrap_or(0);

                if dst_size > src_size && src_size > 0 {
                    let _ = app_clone.emit(
                        "log",
                        format!(
                            "  WARNING: Output ({:.1}MB) larger than source ({:.1}MB) — remuxing source instead",
                            dst_size as f64 / 1_000_000.0,
                            src_size as f64 / 1_000_000.0
                        ),
                    );
                    log_lines.push("  Output larger than source — remuxing".into());
                    let _ = std::fs::remove_file(&output_file);

                    // Remux: copy all streams
                    let remux_status = ffbin::ffmpeg_command()
                        .args([
                            "-y",
                            "-i", &item.full_path,
                            "-map", "0",
                            "-c", "copy",
                            &output_str,
                        ])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .await;

                    match remux_status {
                        Ok(s) if s.success() => {
                            let _ = app_clone.emit(
                                "log",
                                format!("  Remuxed source to {} → {}", ext.to_uppercase(), output_str),
                            );
                            log_lines.push(format!("  Remuxed to {}", ext.to_uppercase()));
                        }
                        _ => {
                            let _ = app_clone.emit("log", "  ERROR: Remux failed");
                            log_lines.push("  ERROR: Remux failed".into());
                            if output_file.exists() {
                                let _ = std::fs::remove_file(&output_file);
                            }
                            {
                                let mut q = state_clone.queue.lock().await;
                                if idx < q.len() {
                                    q[idx].status = QueueItemStatus::Failed;
                                }
                            }
                            let _ = app_clone.emit("queue-item-updated", (idx, "Failed"));
                            fail_count += 1;
                            continue;
                        }
                    }
                } else {
                    let _ = app_clone.emit(
                        "log",
                        format!(
                            "  Done → {} ({:.1}MB from {:.1}MB)",
                            output_str,
                            dst_size as f64 / 1_000_000.0,
                            src_size as f64 / 1_000_000.0
                        ),
                    );
                    log_lines.push(format!(
                        "  Done: {:.1}MB from {:.1}MB",
                        dst_size as f64 / 1_000_000.0,
                        src_size as f64 / 1_000_000.0
                    ));
                }

                // Delete source (§10.8)
                if delete_source && output_file.exists() {
                    match std::fs::remove_file(&item.full_path) {
                        Ok(_) => {
                            let _ = app_clone.emit("log", "  Source file deleted");
                            log_lines.push("  Source deleted".into());
                        }
                        Err(e) => {
                            let warn = format!("  WARNING: Could not delete source: {e}");
                            let _ = app_clone.emit("log", &warn);
                            log_lines.push(warn);
                        }
                    }
                }

                {
                    let mut q = state_clone.queue.lock().await;
                    if idx < q.len() {
                        q[idx].status = QueueItemStatus::Done;
                    }
                }
                let _ = app_clone.emit("queue-item-updated", (idx, "Done"));
                done_count += 1;
            } else {
                let _ = app_clone.emit("log", "  ERROR: Output file not found after encode");
                log_lines.push("  ERROR: Output file not found".into());
                {
                    let mut q = state_clone.queue.lock().await;
                    if idx < q.len() {
                        q[idx].status = QueueItemStatus::Failed;
                    }
                }
                let _ = app_clone.emit("queue-item-updated", (idx, "Failed"));
                fail_count += 1;
            }
        }

        // Batch completion (§10.9)
        let batch_duration = batch_start.elapsed();
        let dur_string = format!(
            "{:02}:{:02}:{:02}",
            batch_duration.as_secs() / 3600,
            (batch_duration.as_secs() % 3600) / 60,
            batch_duration.as_secs() % 60
        );

        let cancel_all = {
            let b = state_clone.batch.lock().await;
            b.cancel_all
        };
        let status_msg = if cancel_all { "cancelled" } else { "done" };

        let summary = format!(
            "Batch {}. Done: {}, Failed: {}, Skipped: {}. Duration: {}",
            status_msg, done_count, fail_count, skip_count, dur_string
        );
        let _ = app_clone.emit("log", "");
        let _ = app_clone.emit("log", &summary);
        log_lines.push(String::new());
        log_lines.push(summary.clone());

        // Save log (§7.5)
        if save_log {
            let log_filename = format!(
                "encode_log_{}.txt",
                chrono::Local::now().format("%Y%m%d_%H%M%S")
            );
            let log_path = Path::new(&output_folder).join(log_filename);
            match std::fs::write(&log_path, log_lines.join("\n")) {
                Ok(_) => {
                    let _ = app_clone.emit(
                        "log",
                        format!("  Log saved to {}", log_path.display()),
                    );
                }
                Err(e) => {
                    let _ = app_clone.emit(
                        "log",
                        format!("  WARNING: Could not save log: {e}"),
                    );
                }
            }
        }

        // Toast notification (§7.7)
        if show_toast {
            let _ = app_clone.emit(
                "toast",
                format!(
                    "Done: {}  Failed: {}  Skipped: {}  Duration: {}",
                    done_count, fail_count, skip_count, dur_string
                ),
            );
        }

        // Post-batch action (§7.6)
        if post_action != "None" {
            let _ = app_clone.emit(
                "post-batch",
                serde_json::json!({
                    "action": post_action,
                    "countdown": post_countdown,
                }),
            );
        }

        // Mark batch as finished
        {
            let mut b = state_clone.batch.lock().await;
            b.running = false;
        }
        let _ = app_clone.emit("batch-finished", serde_json::json!({
            "done": done_count,
            "failed": fail_count,
            "skipped": skip_count,
            "duration": dur_string,
        }));
    });

    Ok(())
}

/// Build per-stream audio arguments from pre-probed audio stream data (§6.2).
/// This avoids spawning a separate ffprobe during encoding — audio metadata
/// was already collected during the initial probe and stored on QueueItem.
fn build_audio_args_from_probe(
    audio_streams: &[crate::queue::AudioStreamInfo],
    audio_encoder: &str,
    audio_cap: u32,
    app: &AppHandle,
) -> Vec<String> {
    if audio_encoder == "copy" {
        let _ = app.emit("log", "  Audio: copying all streams");
        return vec!["-c:a".into(), "copy".into()];
    }

    if audio_streams.is_empty() {
        let _ = app.emit("log", "  WARNING: No audio streams found");
        return Vec::new();
    }

    let mut args = Vec::new();

    for stream in audio_streams {
        let should_copy =
            (stream.codec == audio_encoder || stream.codec == "copy")
            && stream.bitrate_kbps < audio_cap;

        if should_copy {
            args.extend(vec![
                format!("-c:a:{}", stream.index),
                "copy".into(),
            ]);
            let _ = app.emit(
                "log",
                format!(
                    "  Audio {} : {} @ {}kbps — copying",
                    stream.index, stream.codec, stream.bitrate_kbps
                ),
            );
        } else {
            let target_br = stream.bitrate_kbps.min(audio_cap);
            args.extend(vec![
                format!("-c:a:{}", stream.index),
                audio_encoder.to_string(),
                format!("-b:a:{}", stream.index),
                format!("{}k", target_br),
            ]);
            let _ = app.emit(
                "log",
                format!(
                    "  Audio {} : {} @ {}kbps — encoding to {} {}kbps",
                    stream.index, stream.codec, stream.bitrate_kbps,
                    audio_encoder, target_br
                ),
            );
        }
    }

    args
}

/// Wait for a response field to be set in BatchState.
async fn wait_for_response<F>(state: &Arc<AppState>, extractor: F) -> String
where
    F: Fn(&mut crate::queue::BatchState) -> Option<String>,
{
    loop {
        {
            let mut b = state.batch.lock().await;
            if let Some(response) = extractor(&mut b) {
                return response;
            }
            // Also check for cancel_all to avoid deadlock
            if b.cancel_all {
                return "cancel".to_string();
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
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