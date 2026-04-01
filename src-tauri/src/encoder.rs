#[cfg(feature = "dovi")]
use crate::dovi_pipeline;
use crate::events::{BatchControl, EventSink};
use crate::ffmpeg as ffbin;
#[cfg(feature = "dovi")]
use crate::hdr10plus_pipeline;
use crate::queue::{QueueItem, QueueItemStatus};
use crate::staging::{StagingContext, WaveItem};
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
#[cfg(target_os = "windows")]
const AV1_PRIORITY: &[&str] = &["av1_amf", "av1_nvenc", "av1_qsv", "libsvtav1"];

#[cfg(target_os = "macos")]
const HEVC_PRIORITY: &[&str] = &["hevc_videotoolbox", "libx265"];
#[cfg(target_os = "macos")]
const H264_PRIORITY: &[&str] = &["h264_videotoolbox", "libx264"];
#[cfg(target_os = "macos")]
const AV1_PRIORITY: &[&str] = &["av1_videotoolbox", "libsvtav1"];

#[cfg(target_os = "linux")]
const HEVC_PRIORITY: &[&str] = &["hevc_vaapi", "hevc_nvenc", "hevc_qsv", "libx265"];
#[cfg(target_os = "linux")]
const H264_PRIORITY: &[&str] = &["h264_vaapi", "h264_nvenc", "h264_qsv", "libx264"];
#[cfg(target_os = "linux")]
const AV1_PRIORITY: &[&str] = &["av1_vaapi", "av1_nvenc", "av1_qsv", "libsvtav1"];

// Fallback for other platforms (compilation only)
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const HEVC_PRIORITY: &[&str] = &["libx265"];
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const H264_PRIORITY: &[&str] = &["libx264"];
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
const AV1_PRIORITY: &[&str] = &["libsvtav1"];

const AUDIO_ENCODERS: &[&str] = &["ac3", "eac3", "aac"];

/// Fixed tonemap filter chain for HDR→SDR conversion (#1).
/// Defined as a static str to avoid per-file heap allocation.
const TONEMAP_HABLE: &str =
    "zscale=t=linear,tonemap=hable:desat=0,zscale=t=bt709:p=bt709:m=bt709:r=tv,format=yuv420p";

/// Files within this fraction above the threshold that are already in the
/// target codec are stream-copied rather than re-encoded. Avoids wasting
/// time on marginal bitrate gains (e.g. 4.5 Mbps vs 4.0 Mbps target).
const COPY_HYSTERESIS: f64 = 1.15;

/// Minimum file duration (seconds) required for CRF viability probing.
/// Files shorter than this skip the probe — not worth the overhead.
const MIN_PROBE_DURATION_SECS: f64 = 120.0;

/// Fractional seek points for CRF viability probe samples (25%, 50%, 75%).
const PROBE_SEEK_POINTS: [f64; 3] = [0.25, 0.50, 0.75];

/// Duration of each CRF probe sample in seconds.
const PROBE_SAMPLE_DURATION_SECS: f64 = 10.0;

/// Per-encoder flag mapping for VBR mode.
/// AV1 hardware encoders use the same flags as their H.264/HEVC counterparts.
pub fn vbr_flags(encoder: &str, target: &str, peak: &str) -> Vec<String> {
    match encoder {
        "hevc_amf" | "h264_amf" | "av1_amf" => vec![
            "-quality".into(),
            "quality".into(),
            "-rc".into(),
            "vbr_peak".into(),
            "-b:v".into(),
            target.into(),
            "-maxrate".into(),
            peak.into(),
        ],
        "hevc_nvenc" | "h264_nvenc" | "av1_nvenc" => vec![
            "-preset".into(),
            "p7".into(),
            "-rc".into(),
            "vbr".into(),
            "-b:v".into(),
            target.into(),
            "-maxrate".into(),
            peak.into(),
        ],
        "hevc_qsv" | "h264_qsv" | "av1_qsv" => vec![
            "-preset".into(),
            "veryslow".into(),
            "-b:v".into(),
            target.into(),
            "-maxrate".into(),
            peak.into(),
        ],
        "hevc_videotoolbox" | "h264_videotoolbox" | "av1_videotoolbox" => {
            vec!["-b:v".into(), target.into(), "-maxrate".into(), peak.into()]
        }
        "hevc_vaapi" | "h264_vaapi" | "av1_vaapi" => vec![
            "-rc_mode".into(),
            "VBR".into(),
            "-b:v".into(),
            target.into(),
            "-maxrate".into(),
            peak.into(),
        ],
        "libx265" | "libx264" => vec![
            "-preset".into(),
            "slow".into(),
            "-b:v".into(),
            target.into(),
            "-maxrate".into(),
            peak.into(),
        ],
        "libsvtav1" => vec![
            "-preset".into(),
            "6".into(),
            "-b:v".into(),
            target.into(),
            "-maxrate".into(),
            peak.into(),
        ],
        _ => vec!["-b:v".into(), target.into(), "-maxrate".into(), peak.into()],
    }
}

/// Per-encoder flag mapping for CQP mode.
/// AV1 hardware encoders use the same flags as their H.264/HEVC counterparts.
pub fn cqp_flags(encoder: &str, qi: &str, qp: &str) -> Vec<String> {
    match encoder {
        "hevc_amf" | "h264_amf" | "av1_amf" => vec![
            "-quality".into(),
            "quality".into(),
            "-rc".into(),
            "cqp".into(),
            "-qp_i".into(),
            qi.into(),
            "-qp_p".into(),
            qp.into(),
        ],
        "hevc_nvenc" | "h264_nvenc" | "av1_nvenc" => vec![
            "-preset".into(),
            "p7".into(),
            "-rc".into(),
            "constqp".into(),
            "-qp".into(),
            qi.into(),
        ],
        "hevc_qsv" | "h264_qsv" | "av1_qsv" => vec![
            "-preset".into(),
            "veryslow".into(),
            "-global_quality".into(),
            qi.into(),
        ],
        "hevc_videotoolbox" | "h264_videotoolbox" | "av1_videotoolbox" => {
            vec!["-q:v".into(), qi.into()]
        }
        "hevc_vaapi" | "h264_vaapi" | "av1_vaapi" => {
            vec!["-rc_mode".into(), "CQP".into(), "-qp".into(), qi.into()]
        }
        "libx265" | "libx264" => vec!["-preset".into(), "slow".into(), "-qp".into(), qi.into()],
        "libsvtav1" => vec![
            "-preset".into(),
            "6".into(),
            "-rc".into(),
            "0".into(),
            "-qp".into(),
            qi.into(),
        ],
        _ => vec!["-qp".into(), qi.into()],
    }
}

/// Per-encoder flag mapping for CRF mode - only valid for software encoders.
/// Hardware encoders do not support CRF; fall back to CQP for them.
/// Accepts pre-formatted strings to avoid re-allocating per file.
pub fn crf_flags(encoder: &str, crf_str: &str, qi: &str, qp: &str) -> Vec<String> {
    match encoder {
        "libx265" | "libx264" => vec![
            "-preset".into(),
            "slow".into(),
            "-crf".into(),
            crf_str.into(),
        ],
        "libsvtav1" => vec!["-preset".into(), "6".into(), "-crf".into(), crf_str.into()],
        // HW encoders don't support CRF - fall back to CQP
        _ => cqp_flags(encoder, qi, qp),
    }
}

/// Software fallback encoder for a given codec family.
pub fn software_fallback(codec_family: &str) -> &'static str {
    match codec_family {
        "h264" => "libx264",
        "av1" => "libsvtav1",
        _ => "libx265",
    }
}

/// Human-readable codec family name for log messages.
pub fn display_codec_family(family: &str) -> &'static str {
    match family {
        "h264" => "H.264",
        "av1" => "AV1",
        _ => "HEVC",
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
    Crf {
        crf: u32,
        qi_fallback: u32,
        qp_fallback: u32,
    },
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
    let is_copyable = !matches!(video_codec, "gif" | "apng" | "mjpeg" | "webp");
    let is_already_target = video_codec == target_codec;

    if bitrate_mbps <= threshold || bitrate_mbps <= 0.0 {
        // Below threshold with a valid bitrate: copy as-is. The file is
        // already small enough - transcoding to a different codec doesn't
        // make it smaller and risks making it bigger.
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
    } else if is_already_target && is_copyable && bitrate_mbps <= threshold * COPY_HYSTERESIS {
        EncodeDecision::Copy
    } else {
        let target_bps = (threshold * 1_000_000.0) as u64;
        let mult = if peak_multiplier > 1.0 {
            peak_multiplier
        } else {
            1.5
        };
        let peak_bps = (target_bps as f64 * mult) as u64;
        EncodeDecision::Vbr {
            target_bps,
            peak_bps,
        }
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
                display_codec_family(codec_family),
                item_bitrate_mbps
            )
        }
        EncodeDecision::Vbr { peak_bps, .. } => {
            let peak_mbps = *peak_bps as f64 / 1_000_000.0;
            format!(
                "{:.2}Mbps [{}] {}x{} - VBR target {}Mbps peak {:.2}Mbps",
                item_bitrate_mbps, item_codec, item_width, item_height, threshold, peak_mbps
            )
        }
        EncodeDecision::Cqp { qi, qp } => {
            format!(
                "{:.2}Mbps [{}] {}x{} - CQP ({}/{})",
                item_bitrate_mbps, item_codec, item_width, item_height, qi, qp
            )
        }
        EncodeDecision::Crf { crf, .. } => {
            format!(
                "{:.2}Mbps [{}] {}x{} - CRF {}",
                item_bitrate_mbps, item_codec, item_width, item_height, crf
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
    let available: HashSet<&str> = stdout
        .lines()
        .flat_map(|line| line.split_whitespace())
        .collect();

    // Check each video encoder in priority order
    for &enc_name in HEVC_PRIORITY
        .iter()
        .chain(H264_PRIORITY.iter())
        .chain(AV1_PRIORITY.iter())
    {
        if !available.contains(enc_name) {
            sink.log(&format!("[detect] {enc_name} not listed in ffmpeg"));
            continue;
        }

        let family = if enc_name.starts_with("hevc") || enc_name == "libx265" {
            "hevc"
        } else if enc_name.starts_with("av1") || enc_name == "libsvtav1" {
            "av1"
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
            sink.log(&format!(
                "[detect] {enc_name} (HW) - not available (test encode failed)"
            ));
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
    ensure_fallback(&mut video_encoders, "libsvtav1", "av1");
    if !audio_encoders.iter().any(|e| e == "ac3") {
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
    // Use a temp file - some hardware encoders fail with -f null.
    // NamedTempFile auto-deletes on drop.
    let tmp_file = match tempfile::Builder::new()
        .prefix(&format!("histv_test_{}_", encoder_name))
        .suffix(".mp4")
        .tempfile()
    {
        Ok(f) => f,
        Err(_) => return false,
    };
    let tmp_str = tmp_file.path().to_string_lossy().to_string();

    // 256x256 minimum - some HW encoders reject smaller resolutions
    // nv12 pixel format - required by most hardware encoders
    let result = ffbin::ffmpeg_command()
        .args([
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=black:s=256x256:d=0.04:r=25",
            "-frames:v",
            "1",
            "-pix_fmt",
            "nv12",
            "-c:v",
            encoder_name,
            &tmp_str,
        ])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn();

    match result {
        Ok(child) => child
            .wait_with_output()
            .await
            .map(|o| o.status.success())
            .unwrap_or(false),
        Err(_) => false,
    }
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
            EncoderInfo {
                name: "libsvtav1".into(),
                codec_family: "av1".into(),
                is_hardware: false,
            },
        ],
        vec!["ac3".into()],
    )
}

// ── Per-file codec/container/audio resolution (§2.4.0) ──────────

/// Audio handling strategy for a file.
#[derive(Debug, Clone)]
pub enum AudioStrategy {
    /// Default: copy if below cap; re-encode to same codec at cap;
    /// codecs with no ffmpeg encoder fall back to EAC3.
    CopyCapped { cap_kbps: u32 },
    /// Compatibility: copy if below cap and already AC3;
    /// otherwise re-encode to AC3 at cap.
    CompatCapped { cap_kbps: u32 },
}

/// Resolved per-file encoding settings.
#[derive(Debug, Clone)]
pub struct ResolvedFileSettings {
    pub codec_family: String,
    pub encoder_name: String,
    pub container_ext: String,
    pub audio_strategy: AudioStrategy,
}

/// Resolve the output container extension for a single file.
///
/// Three layers of logic:
/// 1. **Auto resolution** - when `container_setting` is "auto", derive from source extension.
/// 2. **DV override** - when `is_dovi_tier1` is true, force MP4 (DV requires MP4 container).
/// 3. **Explicit** - when "mkv" or "mp4", use that directly.
///
/// Deterministic, no I/O - safe to call from display code.
pub fn resolve_container(
    source_path: &str,
    container_setting: &str,
    is_dovi_tier1: bool,
) -> &'static str {
    // DV Tier 1 always forces MP4
    if is_dovi_tier1 {
        return "mp4";
    }

    match container_setting {
        "mp4" => "mp4",
        "mkv" => "mkv",
        _ => {
            // "auto" or any other value: derive from source extension
            let ext = std::path::Path::new(source_path)
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            match ext.as_str() {
                "mp4" | "m4v" => "mp4",
                "mkv" => "mkv",
                _ => "mkv", // AVI, TS, WMV, MOV, WebM, FLV, VOB, etc. → MKV
            }
        }
    }
}

/// Determine codec, encoder, container, and audio strategy for a single file
/// based on its source properties and the user's batch settings.
///
/// `source_video_codec` is the lowercase ffmpeg codec name (e.g. "hevc", "h264", "av1", "mpeg2video").
/// `source_container` is the file extension without the dot (e.g. "mkv", "mp4", "avi").
pub fn resolve_file_settings(
    source_video_codec: &str,
    source_container: &str,
    settings: &BatchSettings,
    detected_encoders: &[EncoderInfo],
) -> ResolvedFileSettings {
    // 1. Codec family
    let codec_family = if settings.compatibility_mode {
        "h264"
    } else if source_video_codec == "h264" {
        "h264"
    } else if source_video_codec == "hevc" || source_video_codec == "h265" {
        "hevc"
    } else if settings.preserve_av1 && source_video_codec == "av1" {
        "av1"
    } else {
        "hevc" // default conversion target
    };

    // 2. Encoder
    let encoder_name = if settings.precision_mode {
        software_fallback(codec_family).to_string()
    } else {
        detected_encoders
            .iter()
            .find(|e| e.codec_family == codec_family)
            .map(|e| e.name.clone())
            .unwrap_or_else(|| software_fallback(codec_family).to_string())
    };

    // 3. Container - use the shared resolver
    let container_ext = if settings.compatibility_mode {
        "mp4".to_string()
    } else {
        // source_container is just the extension; build a fake path for resolve_container
        let fake_path = format!("file.{}", source_container);
        resolve_container(&fake_path, &settings.output_container, false).to_string()
    };

    // Legacy: the CLI encode loop still reads these directly.
    // Remove once the CLI uses resolve_file_settings() per-file (v2.5).
    // 4. Audio strategy
    let audio_strategy = if settings.compatibility_mode {
        AudioStrategy::CompatCapped { cap_kbps: 640 }
    } else {
        AudioStrategy::CopyCapped { cap_kbps: 640 }
    };

    ResolvedFileSettings {
        codec_family: codec_family.to_string(),
        encoder_name,
        container_ext,
        audio_strategy,
    }
}

// ── Batch encoding (§10) ────────────────────────────────────────

// ── Pre-flight check (Phase DV) ────────────────────────────────

/// Warning about a file that won't get its best-possible encode.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PreflightWarning {
    pub file_name: String,
    pub source_type: String,
    pub best_possible: String,
    pub actual_outcome: String,
    pub missing_tool: Option<String>,
}

/// Scan the queue and report files that will be degraded due to missing tools.
pub fn preflight_scan(queue: &[QueueItem]) -> Vec<PreflightWarning> {
    let caps = crate::dovi_tools::capabilities();
    let mut warnings = Vec::new();

    for item in queue {
        if item.status != QueueItemStatus::Pending {
            continue;
        }

        // Dolby Vision files without MP4Box
        if let Some(profile) = item.probe.dovi_profile {
            if !caps.can_package_dovi_mp4 {
                let source_type = format!("Dolby Vision Profile {}", profile);
                let actual = if profile == 5 {
                    "HDR10 fallback (Profile 5 → 8.1 conversion requires MP4Box)".to_string()
                } else {
                    "HDR10 fallback (DV tools not found)".to_string()
                };
                let best = "Full DV preservation".to_string();
                warnings.push(PreflightWarning {
                    file_name: item.file_name.clone(),
                    source_type,
                    best_possible: best,
                    actual_outcome: actual,
                    missing_tool: Some("MP4Box".to_string()),
                });
            }
            // Profile 5 with tools available: auto-converted to 8.1, no warning needed
        }

        // HDR10+ without the crate (only if feature not compiled)
        if item.probe.has_hdr10plus && !caps.can_process_hdr10plus {
            warnings.push(PreflightWarning {
                file_name: item.file_name.clone(),
                source_type: "HDR10+".to_string(),
                best_possible: "Full dynamic metadata preservation".to_string(),
                actual_outcome: "Static HDR10 only (hdr10plus support not available)".to_string(),
                missing_tool: None,
            });
        }
    }

    warnings
}

/// Typed deserialization target for the frontend's `startBatch` JSON payload.
///
/// Field names use `camelCase` to match the frontend keys.  Each field has a
/// serde default that mirrors the old `unwrap_or()` fallbacks so that missing
/// keys are handled identically.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BatchRequest {
    #[serde(default = "batch_req_default_output_folder")]
    pub output_folder: String,
    #[serde(default = "batch_req_default_output_mode")]
    pub output_mode: String,
    #[serde(default = "batch_req_default_target_bitrate")]
    pub target_bitrate: f64,
    #[serde(default = "batch_req_default_qp_i")]
    pub qp_i: u32,
    #[serde(default = "batch_req_default_qp_p")]
    pub qp_p: u32,
    #[serde(default = "batch_req_default_crf")]
    pub crf: u32,
    #[serde(default = "batch_req_default_rate_control_mode")]
    pub rate_control_mode: String,
    #[serde(default = "batch_req_default_pix_fmt")]
    pub pix_fmt: String,
    #[serde(default)]
    pub delete_source: bool,
    #[serde(default)]
    pub save_log: bool,
    #[serde(default = "batch_req_default_peak_multiplier")]
    pub peak_multiplier: f64,
    #[serde(default)]
    pub threads: u32,
    #[serde(default)]
    pub low_priority: bool,
    #[serde(default)]
    pub precision_mode: bool,
    #[serde(default)]
    pub compatibility_mode: bool,
    #[serde(default)]
    pub preserve_av1: bool,
    #[serde(default)]
    pub force_local: bool,
    // GUI-only post-batch fields (not part of BatchSettings)
    #[serde(default)]
    pub show_toast: bool,
    #[serde(default = "batch_req_default_post_action")]
    pub post_action: String,
    #[serde(default)]
    pub post_countdown: u32,
    #[serde(default)]
    pub overwrite: bool,
}

fn batch_req_default_output_folder() -> String {
    "output".to_string()
}
fn batch_req_default_output_mode() -> String {
    "folder".to_string()
}
fn batch_req_default_target_bitrate() -> f64 {
    5.0
}
fn batch_req_default_qp_i() -> u32 {
    20
}
fn batch_req_default_qp_p() -> u32 {
    22
}
fn batch_req_default_crf() -> u32 {
    20
}
fn batch_req_default_rate_control_mode() -> String {
    "QP".to_string()
}
fn batch_req_default_pix_fmt() -> String {
    "yuv420p".to_string()
}
fn batch_req_default_peak_multiplier() -> f64 {
    1.5
}
fn batch_req_default_post_action() -> String {
    "None".to_string()
}

impl BatchRequest {
    /// Convert the validated frontend request into the internal `BatchSettings`
    /// used by the encode loop, applying any clamping rules.
    pub fn into_batch_settings(self) -> BatchSettings {
        BatchSettings {
            output_folder: self.output_folder,
            output_container: "auto".to_string(),
            output_mode: self.output_mode,
            threshold: self.target_bitrate,
            qp_i: self.qp_i.min(51),
            qp_p: self.qp_p.min(51),
            crf_val: self.crf.min(51),
            rate_control_mode: self.rate_control_mode,
            video_encoder: "auto".to_string(),
            codec_family: "hevc".to_string(),
            audio_encoder: "ac3".to_string(),
            audio_cap: 640,
            pix_fmt: self.pix_fmt,
            delete_source: self.delete_source,
            save_log: self.save_log,
            post_command: None,
            peak_multiplier: self.peak_multiplier,
            threads: self.threads.min(64),
            low_priority: self.low_priority,
            precision_mode: self.precision_mode,
            compatibility_mode: self.compatibility_mode,
            preserve_av1: self.preserve_av1,
            force_local: self.force_local,
        }
    }
}

/// Batch encoding settings, extracted from either CLI args or GUI settings JSON.
#[derive(Debug, Clone)]
pub struct BatchSettings {
    pub output_folder: String,
    pub output_mode: String, // "folder" | "beside" | "replace"
    pub threshold: f64,
    pub qp_i: u32,
    pub qp_p: u32,
    pub crf_val: u32,
    pub rate_control_mode: String,
    pub pix_fmt: String,
    pub delete_source: bool,
    pub save_log: bool,
    pub post_command: Option<String>,
    pub peak_multiplier: f64,
    pub threads: u32,
    pub low_priority: bool,
    pub precision_mode: bool,
    pub compatibility_mode: bool,
    pub preserve_av1: bool,
    pub force_local: bool,
    // Legacy fields kept for CLI compatibility until 2.5 refactor completes.
    // The encode loop still reads these; resolve_file_settings will replace them.
    pub video_encoder: String,
    pub codec_family: String,
    pub audio_encoder: String,
    pub audio_cap: u32,
    pub output_container: String,
}

/// Result of encoding a single file, returned by `encode_single_file`.
/// The caller accumulates counters and decides whether to break the outer loop.
enum EncodeFileResult {
    /// Encode completed successfully.
    Done,
    /// Encode failed (ffmpeg error, missing output, etc.).
    Failed,
    /// File was skipped (output exists and user chose to skip).
    Skipped,
    /// User cancelled this file only (cancel-current).
    CancelledCurrent,
    /// User cancelled the entire batch (cancel-all / break 'outer).
    CancelledAll,
}

/// A work unit in the wave-aware encode loop: either a single local file
/// or a wave of remote files to stage together.
struct WaveWork {
    indices: Vec<usize>,
    /// Staging directory for this wave (None = local, no staging)
    staging_dir: Option<std::path::PathBuf>,
    total_stage_bytes: u64,
    wave_number: u32, // 0 for local items
    total_waves: u32,
}

/// Post-encode processing: DV/HDR10+ metadata injection, size check, MKV tag
/// patching, and source replacement/deletion.
///
/// Returns `true` if the file was completed successfully, `false` on failure.
/// On failure the queue item is marked as `Failed`.
#[allow(clippy::too_many_arguments)]
async fn handle_post_encode(
    idx: usize,
    queue: &mut [QueueItem],
    decision: &EncodeDecision,
    temp_output_file: &std::path::Path,
    output_file: &std::path::Path,
    output_str: &str,
    final_output_str: &str,
    ext: &str,
    item_full_path: &str,
    is_image_source: bool,
    is_replace_mode: bool,
    item_duration_secs: f64,
    final_frame_count: Option<u64>,
    #[cfg(feature = "dovi")] extracted_rpus: &Option<dovi_pipeline::ExtractedRpus>,
    #[cfg(not(feature = "dovi"))] _extracted_rpus: &Option<()>,
    #[cfg(feature = "dovi")] extracted_hdr10plus: &Option<hdr10plus_pipeline::ExtractedHdr10Plus>,
    #[cfg(not(feature = "dovi"))] _extracted_hdr10plus: &Option<()>,
    settings: &BatchSettings,
    log_writer: &mut Option<std::io::BufWriter<std::fs::File>>,
    write_log: &(dyn Fn(&mut Option<std::io::BufWriter<std::fs::File>>, &str) + Send + Sync),
    sink: &dyn EventSink,
) -> bool {
    // ── Post-encode: DV RPU injection + MP4Box packaging (Tier 1) ──
    #[cfg(feature = "dovi")]
    if let Some(ref rpus) = extracted_rpus {
        if temp_output_file.exists() {
            // Stage DV output to a separate temp file so the downstream
            // logic (size check, replace rename) works unchanged.
            let dv_staging = temp_output_file.with_extension("histv-dv.mp4");

            match dovi_pipeline::inject_and_package(
                temp_output_file,
                temp_output_file, // audio source: use encoded audio, not original
                &dv_staging,
                rpus,
                sink,
            )
            .await
            {
                Ok(result) if result.success => {
                    sink.log(&format!("  {}", result.message));
                    write_log(log_writer, &format!("  {}", result.message));
                    // Swap: remove the non-DV encode, put the DV MP4 in its place
                    let _ = std::fs::remove_file(temp_output_file);
                    if let Err(e) = std::fs::rename(&dv_staging, temp_output_file) {
                        sink.log(&format!("  WARNING: Could not rename DV output: {e}"));
                        // Try copy as fallback (cross-filesystem)
                        let _ = std::fs::copy(&dv_staging, temp_output_file);
                        let _ = std::fs::remove_file(&dv_staging);
                    }
                }
                Ok(result) => {
                    // MP4Box failed — fall through with original encode (HDR10 fallback)
                    sink.log(&format!("  {}", result.message));
                    write_log(log_writer, &format!("  {}", result.message));
                    let _ = std::fs::remove_file(&dv_staging);
                }
                Err(e) => {
                    sink.log(&format!(
                        "  DV injection failed: {e} - output has HDR10 only"
                    ));
                    write_log(log_writer, &format!("  DV injection failed: {e}"));
                    let _ = std::fs::remove_file(&dv_staging);
                }
            }
        }
    }

    // ── Post-encode: HDR10+ metadata injection (Tier 2) ──────
    #[cfg(feature = "dovi")]
    if let Some(ref meta) = extracted_hdr10plus {
        if temp_output_file.exists() {
            let injected_output = temp_output_file.with_extension(format!("hdr10p.{}", ext));

            match hdr10plus_pipeline::inject_hdr10plus(
                temp_output_file,
                &injected_output,
                meta,
                sink,
            )
            .await
            {
                Ok(result) if result.success => {
                    sink.log(&format!("  {}", result.message));
                    write_log(log_writer, &format!("  {}", result.message));
                    // Replace the encoded file with the injected version
                    if injected_output.exists() {
                        let _ = std::fs::remove_file(temp_output_file);
                        if let Err(e) = std::fs::rename(&injected_output, temp_output_file) {
                            sink.log(&format!("  WARNING: Could not rename injected file: {e}"));
                        }
                    }
                }
                Ok(result) => {
                    sink.log(&format!("  {}", result.message));
                    write_log(log_writer, &format!("  {}", result.message));
                    let _ = std::fs::remove_file(&injected_output);
                }
                Err(e) => {
                    sink.log(&format!(
                        "  HDR10+ injection failed: {e} - output has static HDR10 only"
                    ));
                    write_log(log_writer, &format!("  HDR10+ injection failed: {e}"));
                    let _ = std::fs::remove_file(&injected_output);
                }
            }
        }
    }

    // Post-encode size check
    // In replace mode, ffmpeg wrote to temp_output_file; in other modes temp == final.
    if temp_output_file.exists() {
        let src_size = std::fs::metadata(item_full_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let dst_size = std::fs::metadata(temp_output_file)
            .map(|m| m.len())
            .unwrap_or(0);

        if dst_size > src_size && src_size > 0 && !is_image_source {
            sink.log(&format!(
                "  WARNING: Output ({:.1}MB) larger than source ({:.1}MB) - remuxing source instead",
                dst_size as f64 / 1_000_000.0, src_size as f64 / 1_000_000.0,
            ));
            write_log(log_writer, "  Output larger than source - remuxing");
            let _ = std::fs::remove_file(temp_output_file);

            let remux_output = ffbin::ffmpeg_command()
                .args([
                    "-y",
                    "-i",
                    item_full_path,
                    "-map",
                    "0",
                    "-c",
                    "copy",
                    output_str,
                ])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .map(|child| child.wait_with_output())
                .ok();
            let remux_status = match remux_output {
                Some(fut) => fut.await.ok(),
                None => None,
            };

            match remux_status {
                Some(ref o) if o.status.success() => {
                    sink.log(&format!(
                        "  Remuxed source to {} → {}",
                        ext.to_uppercase(),
                        output_str
                    ));
                    write_log(log_writer, &format!("  Remuxed to {}", ext.to_uppercase()));
                }
                _ => {
                    if let Some(ref o) = remux_status {
                        let stderr = String::from_utf8_lossy(&o.stderr);
                        sink.log(&format!("  Remux stderr: {stderr}"));
                        write_log(log_writer, &format!("  Remux stderr: {stderr}"));
                    }
                    sink.log("  ERROR: Remux failed");
                    write_log(log_writer, "  ERROR: Remux failed");
                    if temp_output_file.exists() {
                        let _ = std::fs::remove_file(temp_output_file);
                    }
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    return false;
                }
            }
        } else {
            sink.log(&format!(
                "  Done → {} ({:.1}MB from {:.1}MB)",
                final_output_str,
                dst_size as f64 / 1_000_000.0,
                src_size as f64 / 1_000_000.0,
            ));
            write_log(
                log_writer,
                &format!(
                    "  Done: {:.1}MB from {:.1}MB",
                    dst_size as f64 / 1_000_000.0,
                    src_size as f64 / 1_000_000.0,
                ),
            );
        }

        // Patch stale MKV stream statistics tags with the actual values.
        // Only needed for MKV encodes (not copies, not MP4). Source muxers
        // write BPS/NUMBER_OF_BYTES/DURATION tags that ffmpeg copies
        // verbatim; after re-encoding these are wrong and mislead
        // subsequent probes.
        if ext == "mkv" && !matches!(decision, EncodeDecision::Copy) {
            let audio_total_bps: u64 = queue[idx]
                .probe
                .audio_streams
                .iter()
                .map(|s| s.bitrate_kbps as u64 * 1000)
                .sum();

            let frames = final_frame_count.filter(|&f| f > 0);

            match crate::mkv_tags::lightweight_repair(
                temp_output_file,
                dst_size,
                item_duration_secs,
                audio_total_bps,
                frames,
            ) {
                Ok((n, bps)) if n > 0 => {
                    let mbps = bps as f64 / 1_000_000.0;
                    sink.log(&format!(
                        "  Updated {} MKV tag{} (video: {:.2}Mbps)",
                        n,
                        if n == 1 { "" } else { "s" },
                        mbps
                    ));
                }
                Ok(_) => {
                    sink.log("  No MKV statistics tags to update");
                }
                Err(e) => {
                    sink.log(&format!("  WARNING: Could not update MKV tags: {e}"));
                }
            }
        }

        // Replace mode: delete source, rename temp to final path
        if is_replace_mode && temp_output_file.exists() {
            // Delete the original source file
            if let Err(e) = std::fs::remove_file(item_full_path) {
                let warn = format!("  WARNING: Could not delete source for replacement: {e}");
                sink.log(&warn);
                write_log(log_writer, &warn);
                // Don't fail - the encode succeeded, we just can't replace
            }
            // Rename temp to final
            if temp_output_file != output_file {
                if let Err(e) = std::fs::rename(temp_output_file, output_file) {
                    let warn = format!("  WARNING: Could not rename temp to final path: {e}");
                    sink.log(&warn);
                    write_log(log_writer, &warn);
                } else {
                    let msg = format!("  Replaced source → {}", final_output_str);
                    sink.log(&msg);
                    write_log(log_writer, &msg);
                }
            }
        } else if settings.delete_source && temp_output_file.exists() {
            // Normal delete-source mode
            match std::fs::remove_file(item_full_path) {
                Ok(_) => {
                    sink.log("  Source file deleted");
                    write_log(log_writer, "  Source deleted");
                }
                Err(e) => {
                    let warn = format!("  WARNING: Could not delete source: {e}");
                    sink.log(&warn);
                    write_log(log_writer, &warn);
                }
            }
        }

        queue[idx].status = QueueItemStatus::Done;
        sink.queue_item_updated(idx, "Done");
        true
    } else {
        sink.log("  ERROR: Output file not found after encode");
        write_log(log_writer, "  ERROR: Output file not found");
        queue[idx].status = QueueItemStatus::Failed;
        sink.queue_item_updated(idx, "Failed");
        false
    }
}

/// Encode a single file from the queue. Handles field extraction, MKV tag
/// repair, encode decision, DV/HDR10+ metadata extraction, FFmpeg arg
/// assembly, precision mode CRF probe, two-pass or single-pass encode,
/// hardware fallback on failure, and post-encode processing.
#[allow(clippy::too_many_arguments)]
async fn encode_single_file(
    idx: usize,
    queue: &mut [QueueItem],
    settings: &BatchSettings,
    detected_encoders: &[EncoderInfo],
    preserve_hdr: bool,
    cached_ram_gb: u64,
    qi_str: &str,
    qp_str: &str,
    crf_str: &str,
    file_counter: u32,
    total: usize,
    log_writer: &mut Option<std::io::BufWriter<std::fs::File>>,
    write_log: &(dyn Fn(&mut Option<std::io::BufWriter<std::fs::File>>, &str) + Send + Sync),
    sink: &dyn EventSink,
    batch_control: &dyn BatchControl,
    batch_stderr_log: Option<SharedStderrLog>,
) -> EncodeFileResult {
    // Extract read-only data from the queue item. Cloning individual
    // fields avoids a full QueueItem clone (which includes audio_streams
    // Vec, all Strings, and fields unused in the loop body).
    let item_full_path = queue[idx].full_path.clone();
    let item_file_name = queue[idx].file_name.clone();
    let item_base_name = queue[idx].base_name.clone();
    let item_video_codec = queue[idx].probe.video_codec.clone();
    let item_video_width = queue[idx].probe.video_width;
    let item_video_height = queue[idx].probe.video_height;
    let item_video_bitrate_mbps = queue[idx].probe.video_bitrate_mbps;
    let item_duration_secs = queue[idx].probe.duration_secs;
    let item_is_hdr = queue[idx].probe.is_hdr;
    let is_image_source = matches!(item_video_codec.as_str(), "gif" | "apng" | "mjpeg" | "webp");
    let is_animated_webp =
        item_video_codec == "webp" && item_full_path.to_lowercase().ends_with(".webp");

    // ── DV/HDR10+ tier determination (Phase DV) ──────────────
    let item_dovi_profile = queue[idx].probe.dovi_profile;
    let item_dovi_bl_compat_id = queue[idx].probe.dovi_bl_compat_id;
    let item_has_hdr10plus = queue[idx].probe.has_hdr10plus;
    let caps = crate::dovi_tools::capabilities();

    // Tier 1: DV source + MP4Box available → full DV preservation
    let is_dovi_tier1 = item_dovi_profile.is_some()
        && caps.can_process_dovi
        && caps.can_package_dovi_mp4
        && preserve_hdr;

    // Tier 2: HDR10+ source + crate available → full metadata preservation
    let is_hdr10plus_tier2 =
        item_has_hdr10plus && caps.can_process_hdr10plus && preserve_hdr && !is_dovi_tier1; // DV takes priority if both are present

    // Per-file codec/encoder/container resolution (§2.4.0)
    let source_ext = std::path::Path::new(&item_full_path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let resolved =
        resolve_file_settings(&item_video_codec, &source_ext, settings, detected_encoders);
    let file_encoder = &resolved.encoder_name;
    let file_codec_family = &resolved.codec_family;
    // Override container for DV Tier 1. DV requires MP4 container for
    // proper signaling. For copies this is typically a no-op (DV sources
    // are already MP4). For re-encodes, MP4Box will set the DV flags.
    let file_ext = if is_dovi_tier1 {
        let resolved_ext = resolve_container(&item_full_path, &settings.output_container, true);
        if resolved_ext != resolved.container_ext {
            sink.log("  Dolby Vision requires MP4 container - output set to MP4 for this file");
        }
        resolved_ext.to_string()
    } else {
        resolved.container_ext.clone()
    };
    let sw_fallback = software_fallback(file_codec_family).to_string();
    let is_sw = file_encoder.starts_with("lib");
    let target_codec = file_codec_family.as_str();

    // Per-file pixel format and optional tonemap filter (#1, #2).
    // preserve_hdr is hoisted above the loop; tonemap uses a static str.
    let (file_pix_fmt, tonemap_filter): (&str, Option<&'static str>) =
        if item_is_hdr && !preserve_hdr {
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

    let ext = file_ext.as_str();
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
                    write_log(log_writer, &err_msg);
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    return EncodeFileResult::Failed;
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
            // "folder" - default: use the configured output folder
            let out = std::path::Path::new(&settings.output_folder)
                .join(format!("{}.{}", item_base_name, ext));
            (out.clone(), out)
        }
    };
    let output_str = temp_output_file.to_string_lossy().to_string();
    let final_output_str = output_file.to_string_lossy().to_string();

    let log_msg = format!("[{}/{}] {}", file_counter, total, item_file_name);
    sink.log(&log_msg);
    write_log(log_writer, &log_msg);

    // Overwrite check - in replace mode, we're replacing the source so
    // no overwrite prompt is needed. In other modes, check the final output.
    if !is_replace_mode && output_file.exists() {
        if !batch_control.overwrite_always() {
            let response = batch_control.overwrite_prompt(&final_output_str);
            match response.as_str() {
                "no" | "skip" => {
                    sink.log("  Skipped (output exists)");
                    write_log(log_writer, "  Skipped (output exists)");
                    queue[idx].status = QueueItemStatus::Skipped;
                    sink.queue_item_updated(idx, "Skipped");
                    return EncodeFileResult::Skipped;
                }
                "always" => {
                    batch_control.set_overwrite_always();
                }
                "cancel" => {
                    return EncodeFileResult::CancelledAll;
                }
                _ => {} // "yes" - proceed
            }
        }
    }

    // Pre-decision MKV tag repair: fix stale statistics tags on input
    // files so the encoding decision uses the actual bitrate. This is
    // lightweight (no I/O beyond a stat + in-place tag write) and runs
    // automatically before every encode.
    let mut item_video_bitrate_mbps = item_video_bitrate_mbps;
    if let Some(bps) = crate::mkv_tags::repair_after_probe(
        &item_full_path,
        item_duration_secs,
        &queue[idx].probe.audio_streams,
    ) {
        let corrected_mbps = bps as f64 / 1_000_000.0;
        if (corrected_mbps - item_video_bitrate_mbps).abs() > 0.1 {
            sink.log(&format!(
                "  Tag repair: {:.2}Mbps → {:.2}Mbps",
                item_video_bitrate_mbps, corrected_mbps
            ));
            item_video_bitrate_mbps = corrected_mbps;
            queue[idx].probe.video_bitrate_mbps = corrected_mbps;
            queue[idx].probe.video_bitrate_bps = bps as f64;
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

    // ── Pre-encode: DV/HDR10+ metadata extraction ────────────
    // Only extract when actually re-encoding (not copy). Extraction is
    // expensive (demux + full bitstream parse) so we skip it for copies
    // where the original bitstream (and its DV/HDR10+ data) is preserved.
    let is_copy = matches!(decision, EncodeDecision::Copy);

    #[cfg(feature = "dovi")]
    let extracted_rpus = if is_dovi_tier1 && !is_copy {
        let profile = item_dovi_profile.unwrap();
        sink.log(&format!(
            "  Dolby Vision Profile {} detected - extracting RPU data for preservation",
            profile,
        ));
        match dovi_pipeline::extract_rpus(
            std::path::Path::new(&item_full_path),
            profile,
            item_dovi_bl_compat_id,
            sink,
        )
        .await
        {
            Ok(rpus) => Some(rpus),
            Err(e) => {
                sink.log(&format!(
                    "  DV extraction failed: {e} - falling back to HDR10"
                ));
                write_log(log_writer, &format!("  DV extraction failed: {e}"));
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(feature = "dovi"))]
    let extracted_rpus: Option<()> = None;

    #[cfg(feature = "dovi")]
    let extracted_hdr10plus = if is_hdr10plus_tier2 && !is_copy {
        sink.log("  HDR10+ detected - extracting dynamic metadata for preservation");
        match hdr10plus_pipeline::extract_hdr10plus(std::path::Path::new(&item_full_path), sink)
            .await
        {
            Ok(meta) => Some(meta),
            Err(e) => {
                sink.log(&format!(
                    "  HDR10+ extraction failed: {e} - falling back to static HDR10"
                ));
                write_log(log_writer, &format!("  HDR10+ extraction failed: {e}"));
                None
            }
        }
    } else {
        None
    };
    #[cfg(not(feature = "dovi"))]
    let extracted_hdr10plus: Option<()> = None;

    // Build video args from the decision
    let mut video_args: Vec<String> = if matches!(decision, EncodeDecision::Copy) {
        vec!["-c:v".into(), "copy".into()]
    } else {
        vec!["-c:v".into(), file_encoder.to_string()]
    };
    let mut mode_desc;

    match &decision {
        EncodeDecision::Copy => {
            mode_desc = format!(
                "  Already {} at {:.2}Mbps (at/below target) - copying video",
                display_codec_family(file_codec_family),
                item_video_bitrate_mbps
            );
        }
        EncodeDecision::Vbr {
            target_bps,
            peak_bps,
        } => {
            let target_str = target_bps.to_string();
            let peak_str = peak_bps.to_string();
            video_args.extend(vbr_flags(file_encoder, &target_str, &peak_str));
            let peak_mbps = *peak_bps as f64 / 1_000_000.0;
            mode_desc = format!(
                "  Video: {:.2}Mbps [{}] {}x{} - VBR target {}Mbps peak {:.2}Mbps",
                item_video_bitrate_mbps,
                item_video_codec,
                item_video_width,
                item_video_height,
                settings.threshold,
                peak_mbps
            );
        }
        EncodeDecision::Cqp { .. } => {
            video_args.extend(cqp_flags(file_encoder, qi_str, qp_str));
            mode_desc = format!(
                "  Video: {:.2}Mbps [{}] {}x{} - CQP ({}/{})",
                item_video_bitrate_mbps,
                item_video_codec,
                item_video_width,
                item_video_height,
                settings.qp_i,
                settings.qp_p
            );
        }
        EncodeDecision::Crf { crf, .. } => {
            video_args.extend(crf_flags(file_encoder, crf_str, qi_str, qp_str));
            mode_desc = format!(
                "  Video: {:.2}Mbps [{}] {}x{} - CRF {}",
                item_video_bitrate_mbps, item_video_codec, item_video_width, item_video_height, crf
            );
        }
    }

    // ── Precision mode: CRF viability probe + lookahead + maxrate ──
    // Only applies to CRF decisions with software encoders.
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
            batch_stderr_log.clone(),
            sink,
            batch_control,
        )
        .await;

        // Check for cancellation during probe
        if batch_control.should_cancel_all() {
            queue[idx].status = QueueItemStatus::Cancelled;
            sink.queue_item_updated(idx, "Cancelled");
            return EncodeFileResult::CancelledAll;
        }
        if batch_control.should_cancel_current() {
            queue[idx].status = QueueItemStatus::Cancelled;
            sink.queue_item_updated(idx, "Cancelled");
            return EncodeFileResult::CancelledCurrent;
        }

        match probe_result {
            Some(avg_mbps)
                if item_video_bitrate_mbps > 0.0 && avg_mbps > item_video_bitrate_mbps =>
            {
                // CRF would produce a larger file than source - fall back to CQP
                sink.log(&format!(
                    "  Precision mode: CRF estimate {:.2}Mbps exceeds source {:.2}Mbps - falling back to CQP",
                    avg_mbps, item_video_bitrate_mbps
                ));
                write_log(
                    log_writer,
                    &format!(
                        "  Precision: CRF {:.2}Mbps > source {:.2}Mbps - CQP fallback",
                        avg_mbps, item_video_bitrate_mbps
                    ),
                );
                // Rebuild video args as CQP
                video_args = vec!["-c:v".into(), file_encoder.clone()];
                video_args.extend(cqp_flags(file_encoder, qi_str, qp_str));
                mode_desc = format!(
                    "  Video: {:.2}Mbps [{}] {}x{} - CQP ({}/{}) (precision fallback)",
                    item_video_bitrate_mbps,
                    item_video_codec,
                    item_video_width,
                    item_video_height,
                    settings.qp_i,
                    settings.qp_p
                );
                sink.log(&mode_desc);
                write_log(log_writer, &mode_desc);
            }
            Some(_) | None => {
                // CRF is viable (or file was too short to probe) - enhance
                // based on available system RAM (#3 - use cached value).
                let use_lookahead = !precision_needs_two_pass_with_ram(cached_ram_gb);
                let lookahead = if use_lookahead {
                    lookahead_for_ram_with_cache(cached_ram_gb)
                } else {
                    0
                };

                if lookahead > 0 {
                    video_args.extend(vec!["-rc-lookahead".into(), lookahead.to_string()]);
                    sink.log(&format!(
                        "  Precision mode: rc-lookahead {} ({}GB RAM)",
                        lookahead, cached_ram_gb
                    ));
                } else if use_lookahead {
                    sink.log(&format!(
                        "  Precision mode: default lookahead ({}GB RAM)",
                        cached_ram_gb
                    ));
                } else {
                    sink.log(&format!("  Precision mode: two-pass ({}GB RAM - insufficient for extended lookahead)", cached_ram_gb));
                }

                // Cap with maxrate based on the user's target and peak multiplier
                if settings.threshold > 0.0 {
                    let maxrate_bps =
                        (settings.threshold * 1_000_000.0 * settings.peak_multiplier) as u64;
                    video_args.extend(vec![
                        "-maxrate".into(),
                        maxrate_bps.to_string(),
                        "-bufsize".into(),
                        (maxrate_bps * 2).to_string(),
                    ]);
                    sink.log(&format!(
                        "  Precision mode: maxrate {:.1}Mbps ({}Mbps x {}x)",
                        maxrate_bps as f64 / 1_000_000.0,
                        settings.threshold,
                        settings.peak_multiplier
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
                write_log(log_writer, &mode_desc);
            }
        }
    } else {
        sink.log(&mode_desc);
        write_log(log_writer, &mode_desc);
    }

    // Build audio args (skipped for image sources like GIF)
    let (audio_map_args, audio_codec_args) = if is_image_source {
        (Vec::new(), Vec::new())
    } else {
        build_audio_args_from_probe(
            &queue[idx].probe.audio_streams,
            &resolved.audio_strategy,
            sink,
        )
    };

    let exit_code: i32;
    let mut final_frame_count: Option<u64>;

    // ── Animated WebP: dedicated decode + encode pipeline ──
    if is_animated_webp {
        sink.log("  Animated WebP: using RIFF decode pipeline");
        let webp_result = crate::webp_decode::transcode_animated_webp(
            &item_full_path,
            &output_str,
            &video_args,
            settings.threads,
            settings.low_priority,
            sink,
            batch_control,
        )
        .await;

        match webp_result {
            Ok(r) if r.was_cancelled => {
                if batch_control.should_cancel_all() {
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    return EncodeFileResult::CancelledAll;
                }
                queue[idx].status = QueueItemStatus::Cancelled;
                sink.queue_item_updated(idx, "Cancelled");
                return EncodeFileResult::CancelledCurrent;
            }
            Ok(r) if r.exit_code != 0 => {
                let err_msg = format!("  ERROR: WebP encode failed (exit code {})", r.exit_code);
                sink.log(&err_msg);
                write_log(log_writer, &err_msg);
                if temp_output_file.exists() {
                    let _ = std::fs::remove_file(&temp_output_file);
                }
                queue[idx].status = QueueItemStatus::Failed;
                sink.queue_item_updated(idx, "Failed");
                return EncodeFileResult::Failed;
            }
            Ok(r) => {
                final_frame_count = Some(r.frame_count);
            }

            Err(e) => {
                sink.log(&format!("  ERROR: {e}"));
                write_log(log_writer, &format!("  ERROR: {e}"));
                queue[idx].status = QueueItemStatus::Failed;
                sink.queue_item_updated(idx, "Failed");
                return EncodeFileResult::Failed;
            }
        }

        // Skip the normal ffmpeg path - jump to post-encode
    } else {
        // Assemble ffmpeg command
        let video_arg_refs: Vec<&str> = video_args.iter().map(|s| s.as_str()).collect();
        let ffmpeg_args = assemble_ffmpeg_args(
            &item_full_path,
            &video_arg_refs,
            file_pix_fmt,
            &audio_map_args,
            &audio_codec_args,
            &output_str,
            is_image_source,
            settings.threads,
            tonemap_filter,
        );

        let cmd_line = format!("ffmpeg {}", ffmpeg_args.join(" "));
        sink.batch_command(&cmd_line);
        let cmd_log = format!("  CMD: {cmd_line}");
        sink.log(&cmd_log);
        write_log(log_writer, &cmd_log);

        // Spawn ffmpeg - optionally as a two-pass encode.
        // Precision mode on low-RAM systems (<8GB) uses two-pass CRF instead
        // of extended lookahead, since the lookahead would consume too much memory.
        let precision_two_pass = settings.precision_mode
            && is_sw
            && is_crf_decision
            && precision_needs_two_pass_with_ram(cached_ram_gb);
        let use_two_pass = precision_two_pass;

        let proc_start = std::time::Instant::now();

        if use_two_pass {
            // ── Two-pass VBR (software encoders only) (#5) ──
            // TempDir auto-cleans passlog files on drop (including break 'outer).
            let passlog_dir = tempfile::Builder::new()
                .prefix("histv_2pass_")
                .tempdir()
                .map_err(|e| format!("Could not create passlog temp dir: {e}"));
            let passlog_prefix = match passlog_dir {
                Ok(ref d) => d.path().join("passlog").to_string_lossy().to_string(),
                Err(ref e) => {
                    sink.log(&format!("  ERROR: {e}"));
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    return EncodeFileResult::Failed;
                }
            };

            let fmt = if ext == "mp4" { "mp4" } else { "matroska" };

            // Pass 1: analysis only - build args fresh from components
            let pass1_args = build_two_pass_args(&ffmpeg_args, 1, &passlog_prefix, fmt);

            let pass1_cmd = format!("ffmpeg {}", pass1_args.join(" "));
            sink.batch_command(&pass1_cmd);
            write_log(log_writer, &format!("  CMD (pass 1): {pass1_cmd}"));

            match run_ffmpeg_with_progress(
                &pass1_args,
                item_duration_secs,
                Some((1, 2)),
                settings.low_priority,
                batch_stderr_log.clone(),
                &item_file_name,
                sink,
                batch_control,
            )
            .await
            {
                Ok(r) if r.was_cancelled_all => {
                    sink.log("  Cancelled (batch cancel)");
                    write_log(log_writer, "  Cancelled (batch cancel)");
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    return EncodeFileResult::CancelledAll;
                }
                Ok(r) if r.was_cancelled_current => {
                    sink.log("  Cancelled current file");
                    write_log(log_writer, "  Cancelled");
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    return EncodeFileResult::CancelledCurrent;
                }
                Ok(r) if r.exit_code != 0 => {
                    sink.log(&format!(
                        "  ERROR: Pass 1 failed (exit code {})",
                        r.exit_code
                    ));
                    write_log(log_writer, "  ERROR: Pass 1 failed");
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    return EncodeFileResult::Failed;
                }
                Err(e) => {
                    sink.log(&format!("  ERROR: {e}"));
                    write_log(log_writer, &format!("  ERROR: {e}"));
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    return EncodeFileResult::Failed;
                }
                _ => {} // pass 1 OK
            }

            // Pass 2: actual encode with stats from pass 1
            let pass2_args = build_two_pass_args(&ffmpeg_args, 2, &passlog_prefix, fmt);

            let pass2_cmd = format!("ffmpeg {}", pass2_args.join(" "));
            sink.batch_command(&pass2_cmd);
            write_log(log_writer, &format!("  CMD (pass 2): {pass2_cmd}"));

            match run_ffmpeg_with_progress(
                &pass2_args,
                item_duration_secs,
                Some((2, 2)),
                settings.low_priority,
                batch_stderr_log.clone(),
                &item_file_name,
                sink,
                batch_control,
            )
            .await
            {
                Ok(r) if r.was_cancelled_all => {
                    sink.log("  Cancelled (batch cancel)");
                    write_log(log_writer, "  Cancelled (batch cancel)");
                    if temp_output_file.exists() {
                        let _ = std::fs::remove_file(&temp_output_file);
                    }
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    return EncodeFileResult::CancelledAll;
                }
                Ok(r) if r.was_cancelled_current => {
                    sink.log("  Cancelled current file");
                    write_log(log_writer, "  Cancelled");
                    if temp_output_file.exists() {
                        let _ = std::fs::remove_file(&temp_output_file);
                        sink.log("  Partial output removed");
                    }
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    return EncodeFileResult::CancelledCurrent;
                }
                Ok(r) => {
                    exit_code = r.exit_code;
                    final_frame_count = Some(r.frame_count);
                }
                Err(e) => {
                    sink.log(&format!("  ERROR: {e}"));
                    write_log(log_writer, &format!("  ERROR: {e}"));
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    return EncodeFileResult::Failed;
                }
            }
            // passlog_dir dropped here — auto-cleans passlog files
        } else {
            // ── Single-pass encode ──
            match run_ffmpeg_with_progress(
                &ffmpeg_args,
                item_duration_secs,
                None,
                settings.low_priority,
                batch_stderr_log.clone(),
                &item_file_name,
                sink,
                batch_control,
            )
            .await
            {
                Ok(r) if r.was_cancelled_all => {
                    sink.log("  Cancelled (batch cancel)");
                    write_log(log_writer, "  Cancelled (batch cancel)");
                    if temp_output_file.exists() {
                        let _ = std::fs::remove_file(&temp_output_file);
                    }
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    return EncodeFileResult::CancelledAll;
                }
                Ok(r) if r.was_cancelled_current => {
                    sink.log("  Cancelled current file");
                    write_log(log_writer, "  Cancelled");
                    if temp_output_file.exists() {
                        let _ = std::fs::remove_file(&temp_output_file);
                        sink.log("  Partial output removed");
                    }
                    queue[idx].status = QueueItemStatus::Cancelled;
                    sink.queue_item_updated(idx, "Cancelled");
                    return EncodeFileResult::CancelledCurrent;
                }
                Ok(r) => {
                    exit_code = r.exit_code;
                    final_frame_count = Some(r.frame_count);
                }
                Err(e) => {
                    sink.log(&format!("  ERROR: {e}"));
                    write_log(log_writer, &format!("  ERROR: {e}"));
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    return EncodeFileResult::Failed;
                }
            }
        }

        let proc_duration = proc_start.elapsed();
        // Encoder failure fallback (#6 - use run_ffmpeg_with_progress)
        if exit_code != 0
            && proc_duration.as_secs() < 30
            && !matches!(decision, EncodeDecision::Copy)
            && *file_encoder != sw_fallback
        {
            if !batch_control.hw_fallback_offered() {
                batch_control.set_hw_fallback_offered();
                sink.log(&format!("  HW encoder failed for {}", item_file_name));
                let response = batch_control.fallback_prompt(&item_file_name);

                if response == "yes" {
                    sink.log(&format!(
                        "  Falling back to software encoder ({})...",
                        sw_fallback
                    ));
                    write_log(log_writer, &format!("  Fallback to {}", sw_fallback));

                    if temp_output_file.exists() {
                        let _ = std::fs::remove_file(&temp_output_file);
                    }

                    // Rebuild with software encoder
                    let mut sw_video_args: Vec<String>;
                    match &decision {
                        EncodeDecision::Vbr {
                            target_bps,
                            peak_bps,
                        } => {
                            sw_video_args = vec!["-c:v".into(), sw_fallback.clone()];
                            sw_video_args.extend(vbr_flags(
                                &sw_fallback,
                                &target_bps.to_string(),
                                &peak_bps.to_string(),
                            ));
                        }
                        _ => {
                            sw_video_args = vec!["-c:v".into(), sw_fallback.clone()];
                            sw_video_args.extend(cqp_flags(&sw_fallback, qi_str, qp_str));
                        }
                    }

                    // Reuse audio args from the primary encode - inputs haven't changed
                    let sw_ref: Vec<&str> = sw_video_args.iter().map(|s| s.as_str()).collect();
                    let sw_args = assemble_ffmpeg_args(
                        &item_full_path,
                        &sw_ref,
                        file_pix_fmt,
                        &audio_map_args,
                        &audio_codec_args,
                        &output_str,
                        is_image_source,
                        settings.threads,
                        tonemap_filter,
                    );

                    let sw_cmd = format!("ffmpeg {}", sw_args.join(" "));
                    sink.batch_command(&sw_cmd);
                    let sw_cmd_log = format!("  CMD (fallback): {sw_cmd}");
                    sink.log(&sw_cmd_log);
                    write_log(log_writer, &sw_cmd_log);

                    // Use shared helper for progress, pause/cancel, low-priority (#6)
                    match run_ffmpeg_with_progress(
                        &sw_args,
                        item_duration_secs,
                        None,
                        settings.low_priority,
                        batch_stderr_log.clone(),
                        &item_file_name,
                        sink,
                        batch_control,
                    )
                    .await
                    {
                        Ok(r) if r.was_cancelled_all => {
                            sink.log("  Cancelled (batch cancel during fallback)");
                            write_log(log_writer, "  Cancelled (batch cancel during fallback)");
                            if temp_output_file.exists() {
                                let _ = std::fs::remove_file(&temp_output_file);
                            }
                            queue[idx].status = QueueItemStatus::Cancelled;
                            sink.queue_item_updated(idx, "Cancelled");
                            return EncodeFileResult::CancelledAll;
                        }
                        Ok(r) if r.was_cancelled_current => {
                            sink.log("  Cancelled current file (during fallback)");
                            write_log(log_writer, "  Cancelled (during fallback)");
                            if temp_output_file.exists() {
                                let _ = std::fs::remove_file(&temp_output_file);
                                sink.log("  Partial output removed");
                            }
                            queue[idx].status = QueueItemStatus::Cancelled;
                            sink.queue_item_updated(idx, "Cancelled");
                            return EncodeFileResult::CancelledCurrent;
                        }
                        Ok(r) if r.exit_code != 0 => {
                            sink.log("  ERROR: Software encoder also failed - stopping batch");
                            write_log(log_writer, "  ERROR: Software encoder also failed");
                            if temp_output_file.exists() {
                                let _ = std::fs::remove_file(&temp_output_file);
                            }
                            queue[idx].status = QueueItemStatus::Failed;
                            sink.queue_item_updated(idx, "Failed");
                            // Original code: break 'outer after SW fallback failure.
                            // Return CancelledAll to trigger the same outer break.
                            return EncodeFileResult::CancelledAll;
                        }
                        Err(e) => {
                            sink.log(&format!("  ERROR: Could not launch fallback: {e}"));
                            write_log(log_writer, &format!("  ERROR: Fallback launch failed: {e}"));
                            queue[idx].status = QueueItemStatus::Failed;
                            sink.queue_item_updated(idx, "Failed");
                            // Original code: break 'outer after fallback launch failure.
                            return EncodeFileResult::CancelledAll;
                        }
                        Ok(r) => {
                            final_frame_count = Some(r.frame_count);
                        } // fallback encode OK
                    }
                } else {
                    sink.log("  Stopping batch due to encoder failure");
                    write_log(log_writer, "  Batch stopped (encoder failure)");
                    if temp_output_file.exists() {
                        let _ = std::fs::remove_file(&temp_output_file);
                    }
                    queue[idx].status = QueueItemStatus::Failed;
                    sink.queue_item_updated(idx, "Failed");
                    // Original code: break 'outer when user declines fallback.
                    return EncodeFileResult::CancelledAll;
                }
            }
        } else if exit_code != 0 {
            if batch_control.hw_fallback_offered() && *file_encoder != sw_fallback {
                sink.log("  HW encoder failed (fallback already offered for this batch)");
                write_log(log_writer, "  HW encoder failed (fallback already offered)");
            }
            let err_msg = format!("  ERROR: ffmpeg exited with code {}", exit_code);
            sink.log(&err_msg);
            write_log(log_writer, &err_msg);
            if temp_output_file.exists() {
                let _ = std::fs::remove_file(&temp_output_file);
                sink.log("  Failed output removed");
            }
            queue[idx].status = QueueItemStatus::Failed;
            sink.queue_item_updated(idx, "Failed");
            return EncodeFileResult::Failed;
        }
    }

    // ── Post-encode processing ──
    let success = handle_post_encode(
        idx,
        queue,
        &decision,
        &temp_output_file,
        &output_file,
        &output_str,
        &final_output_str,
        ext,
        &item_full_path,
        is_image_source,
        is_replace_mode,
        item_duration_secs,
        final_frame_count,
        &extracted_rpus,
        &extracted_hdr10plus,
        settings,
        log_writer,
        write_log,
        sink,
    )
    .await;

    if success {
        EncodeFileResult::Done
    } else {
        EncodeFileResult::Failed
    }
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
    detected_encoders: &[EncoderInfo],
    wave_plan: Option<Vec<WaveItem>>,
) -> (u32, u32, u32, bool) {
    let qi_str = settings.qp_i.to_string();
    let qp_str = settings.qp_p.to_string();
    let crf_str = settings.crf_val.to_string();

    // ── Pre-loop invariant computation (#2, #3, #4) ──
    let preserve_hdr = settings.pix_fmt == "p010le";
    let cached_ram_gb = get_system_ram_gb();

    // Collect pending indices
    let pending_indices: Vec<usize> = queue
        .iter()
        .enumerate()
        .filter(|(_, item)| item.status == QueueItemStatus::Pending)
        .map(|(i, _)| i)
        .collect();

    let total = pending_indices.len();
    let mut done_count: u32 = 0;
    let mut fail_count: u32 = 0;
    let mut skip_count: u32 = 0;
    let save_log = settings.save_log;
    let output_dir = std::path::Path::new(&settings.output_folder);
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

    // ── Per-batch ffmpeg stderr log ──
    // One shared log file for all ffmpeg invocations in this batch.
    let batch_stderr_log: Option<SharedStderrLog> = open_stderr_log(output_dir);
    // Remember the log filename so we can check if it's empty after the batch
    let stderr_log_filename: Option<std::path::PathBuf> = {
        let log_dir = output_dir.join("ffmpeg_logs");
        // The file was just created by open_stderr_log — find the newest .log
        std::fs::read_dir(&log_dir).ok().and_then(|entries| {
            entries
                .filter_map(|e| e.ok())
                .filter(|e| {
                    e.path()
                        .extension()
                        .map(|ext| ext == "log")
                        .unwrap_or(false)
                })
                .max_by_key(|e| {
                    e.metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH)
                })
                .map(|e| e.path())
        })
    };

    sink.batch_started();

    // ── Wave-aware iteration (Phase 3) ────────────────────────
    //
    // Build work units from the wave plan. Each unit is either a single
    // local file or a wave of remote files to stage together.
    let work_units: Vec<WaveWork> = if let Some(plan) = wave_plan {
        let total_waves = plan
            .iter()
            .filter(|item| matches!(item, WaveItem::Wave { .. }))
            .count() as u32;
        let mut wave_num: u32 = 0;
        plan.into_iter()
            .map(|item| match item {
                WaveItem::Local { queue_index } => WaveWork {
                    indices: vec![queue_index],
                    staging_dir: None,
                    total_stage_bytes: 0,
                    wave_number: 0,
                    total_waves,
                },
                WaveItem::Wave {
                    indices,
                    total_stage_bytes,
                } => {
                    wave_num += 1;
                    WaveWork {
                        indices,
                        staging_dir: Some(crate::staging::resolve_staging_dir(None)),
                        total_stage_bytes,
                        wave_number: wave_num,
                        total_waves,
                    }
                }
            })
            .collect()
    } else {
        // No wave plan: flat iteration (backward compat for GUI)
        pending_indices
            .iter()
            .map(|&idx| WaveWork {
                indices: vec![idx],
                staging_dir: None,
                total_stage_bytes: 0,
                wave_number: 0,
                total_waves: 0,
            })
            .collect()
    };

    // Declared outside the loop so cleanup runs even if `break 'outer` exits early.
    let mut wave_staging_contexts: Vec<(usize, StagingContext, String)> = Vec::new();

    'outer: for work in &work_units {
        // ── Wave staging: stage all files before encoding ──────
        wave_staging_contexts.clear();

        if let Some(ref staging_dir) = work.staging_dir {
            let wave_size = work.indices.len();
            sink.wave_status(&format!(
                "Staging wave {}/{} ({} file{}, {})",
                work.wave_number,
                work.total_waves,
                wave_size,
                if wave_size == 1 { "" } else { "s" },
                crate::disk_monitor::format_bytes(work.total_stage_bytes),
            ));

            for (file_in_wave, &idx) in work.indices.iter().enumerate() {
                if batch_control.should_cancel_all() {
                    was_cancelled = true;
                    break 'outer;
                }

                sink.wave_progress(
                    work.wave_number,
                    work.total_waves,
                    (file_in_wave + 1) as u32,
                    wave_size as u32,
                );

                let original_path = queue[idx].full_path.clone();
                if let Some(ctx) = StagingContext::stage_file(
                    std::path::Path::new(&queue[idx].full_path),
                    staging_dir,
                    idx,
                    sink,
                ) {
                    // Rewrite queue item path to the staged local copy
                    queue[idx].full_path = ctx.local_path().to_string_lossy().to_string();
                    wave_staging_contexts.push((idx, ctx, original_path));
                }
                // If staging fails, the file keeps its remote path and will be encoded in-place
            }
        }

        // ── Encode each file in this work unit ────────────────
        for &idx in &work.indices {
            // Pause / cancel check - wait while paused before starting next file
            while batch_control.is_paused() {
                if batch_control.should_cancel_all() {
                    break;
                }
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
            if batch_control.should_cancel_all() {
                was_cancelled = true;
                break 'outer;
            }
            batch_control.clear_cancel_current();

            file_counter += 1;
            sink.batch_progress(file_counter, total);

            let result = encode_single_file(
                idx,
                queue,
                settings,
                detected_encoders,
                preserve_hdr,
                cached_ram_gb,
                &qi_str,
                &qp_str,
                &crf_str,
                file_counter,
                total,
                &mut log_writer,
                &write_log,
                sink,
                batch_control,
                batch_stderr_log.clone(),
            )
            .await;

            match result {
                EncodeFileResult::Done => done_count += 1,
                EncodeFileResult::Failed => fail_count += 1,
                EncodeFileResult::Skipped => skip_count += 1,
                EncodeFileResult::CancelledCurrent => {}
                EncodeFileResult::CancelledAll => {
                    was_cancelled = true;
                    break 'outer;
                }
            }
        } // end per-file loop

        // ── Wave cleanup: restore original paths and clean up staged files ──
        let is_replace_or_beside =
            settings.output_mode == "replace" || settings.output_mode == "beside";
        for (idx, mut ctx, original_path) in wave_staging_contexts.drain(..) {
            // Copy output back to remote mount for replace/beside modes
            if is_replace_or_beside && queue[idx].status == QueueItemStatus::Done {
                let remote = std::path::Path::new(&original_path);
                let remote_dir = remote.parent().unwrap_or(std::path::Path::new("."));
                let base_name = remote.file_stem().unwrap_or_default().to_string_lossy();
                let ext = resolve_container(&original_path, &settings.output_container, false);
                let is_replace = settings.output_mode == "replace";

                let (local_output, final_remote) = if is_replace {
                    let staging_dir = work.staging_dir.as_ref().unwrap();
                    let local = staging_dir.join(format!("{}.{}", base_name, ext));
                    let remote_dest = remote_dir.join(format!("{}.{}", base_name, ext));
                    (local, remote_dest)
                } else {
                    let staging_dir = work.staging_dir.as_ref().unwrap();
                    let local = staging_dir
                        .join("output")
                        .join(format!("{}.{}", base_name, ext));
                    let remote_dest_dir = remote_dir.join("output");
                    let remote_dest = remote_dest_dir.join(format!("{}.{}", base_name, ext));
                    let _ = std::fs::create_dir_all(&remote_dest_dir);
                    (local, remote_dest)
                };

                if local_output.exists() {
                    sink.log(&format!(
                        "  Moving staged output to remote: {} → {}",
                        local_output.display(),
                        final_remote.display()
                    ));

                    if is_replace {
                        let temp_remote =
                            remote_dir.join(format!("{}.histv-tmp.{}", base_name, ext));
                        match std::fs::copy(&local_output, &temp_remote) {
                            Ok(_) => {
                                if remote.exists() {
                                    let _ = std::fs::remove_file(remote);
                                }
                                if let Err(e) = std::fs::rename(&temp_remote, &final_remote) {
                                    sink.log(&format!(
                                        "  WARNING: Could not rename temp to final on remote: {e}"
                                    ));
                                } else {
                                    sink.log(&format!(
                                        "  Replaced remote source → {}",
                                        final_remote.display()
                                    ));
                                }
                                let _ = std::fs::remove_file(&local_output);
                            }
                            Err(e) => {
                                sink.log(&format!(
                                    "  ERROR: Could not copy output to remote mount: {e}"
                                ));
                                let _ = std::fs::remove_file(&temp_remote);
                                sink.log(&format!(
                                    "  Encoded file preserved at: {}",
                                    local_output.display()
                                ));
                            }
                        }
                    } else {
                        match std::fs::copy(&local_output, &final_remote) {
                            Ok(_) => {
                                let _ = std::fs::remove_file(&local_output);
                                sink.log(&format!(
                                    "  Copied output to remote → {}",
                                    final_remote.display()
                                ));
                            }
                            Err(e) => {
                                sink.log(&format!(
                                    "  ERROR: Could not copy output to remote mount: {e}"
                                ));
                                sink.log(&format!(
                                    "  Encoded file preserved at: {}",
                                    local_output.display()
                                ));
                            }
                        }
                    }
                }
            }

            // Restore original remote path
            queue[idx].full_path = original_path;
            // Clean up the staged input copy
            ctx.cleanup(sink);
        }

        if was_cancelled {
            break;
        }
    } // end 'outer wave loop

    // Clean up any wave staging contexts orphaned by `break 'outer`.
    // On normal loop exit these are already drained, so this is a no-op.
    for (idx, mut ctx, original_path) in wave_staging_contexts.drain(..) {
        queue[idx].full_path = original_path;
        ctx.cleanup(sink);
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

    // ── Stderr log cleanup ──
    // Drop the shared handle so the file is fully flushed/closed.
    drop(batch_stderr_log);
    // Delete if empty, and enforce the 10-file cap.
    cleanup_stderr_logs(output_dir, stderr_log_filename.as_deref(), 10);

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
    frame_count: u64,
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
        "-pass".into(),
        pass_num.to_string(),
        "-passlogfile".into(),
        passlog_prefix.to_string(),
    ]);

    if pass_num == 1 {
        // Pass 1: analysis only - no audio/subs, output to null device
        args.extend([
            "-an".into(),
            "-sn".into(),
            "-f".into(),
            container_fmt.into(),
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
///
/// `stderr_log` is an optional shared batch log file. When provided, all
/// ffmpeg stderr output is appended there. `stderr_label` is written into
/// the separator header (typically the filename being encoded).
async fn run_ffmpeg_with_progress(
    args: &[String],
    file_duration: f64,
    pass: Option<(u8, u8)>,
    low_priority: bool,
    stderr_log: Option<SharedStderrLog>,
    stderr_label: &str,
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

    let mut child = cmd
        .spawn()
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

    // Stream stderr for progress using a blocking thread, with log capture
    let progress = FfmpegProgress::new();
    let stderr_thread = child
        .stderr
        .take()
        .map(|stderr| spawn_stderr_reader(stderr, &progress, stderr_log, stderr_label));

    // Poll for cancellation/pause while waiting for ffmpeg, and emit progress
    let mut last_progress_emit = std::time::Instant::now() - std::time::Duration::from_millis(500);
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
            let graceful =
                tokio::time::timeout(std::time::Duration::from_secs(5), child.wait()).await;
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
                let secs = progress.secs();
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
    let final_frames = progress.frames();

    Ok(FfmpegRunResult {
        exit_code,
        was_cancelled_current: batch_control.should_cancel_current(),
        was_cancelled_all: batch_control.should_cancel_all(),
        frame_count: final_frames,
    })
}

// ── Precision mode helpers ─────────────────────────────────────

/// Detect total system RAM in gigabytes (approximate, zero-dependency).
/// Result is cached after the first call to avoid spawning subprocesses
/// (macOS uses `sysctl`) on every invocation.
pub fn get_system_ram_gb() -> u64 {
    static CACHE: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *CACHE.get_or_init(get_system_ram_gb_inner)
}

fn get_system_ram_gb_inner() -> u64 {
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
                    let kb: u64 = line
                        .split_whitespace()
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
    {
        0
    }
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
    stderr_log: Option<SharedStderrLog>,
    sink: &dyn EventSink,
    batch_control: &dyn BatchControl,
) -> Option<f64> {
    if duration_secs < MIN_PROBE_DURATION_SECS {
        sink.log("  Precision probe: file too short, skipping viability check");
        return None;
    }

    let sample_duration = PROBE_SAMPLE_DURATION_SECS;
    let seek_points = PROBE_SEEK_POINTS;
    let mut total_bits: f64 = 0.0;
    let mut total_sample_secs: f64 = 0.0;

    for (i, &fraction) in seek_points.iter().enumerate() {
        if batch_control.should_cancel_current() || batch_control.should_cancel_all() {
            return None;
        }

        let seek_secs = duration_secs * fraction;
        let sample_num = i + 1;
        sink.batch_status(&format!(
            "Probing CRF viability ({}/{})",
            sample_num,
            seek_points.len()
        ));
        sink.file_progress(
            (sample_num as f64 - 1.0) / seek_points.len() as f64 * 100.0,
            0.0,
            0.0,
            Some((sample_num as u8, seek_points.len() as u8)),
        );

        // Build sample encode args: seek, duration limit, video only, temp output.
        // NamedTempFile auto-deletes on drop; we keep it alive for stat then let it drop.
        let tmp_file = match tempfile::Builder::new()
            .prefix("histv_crf_probe_")
            .suffix(".mkv")
            .tempfile()
        {
            Ok(f) => f,
            Err(e) => {
                sink.log(&format!("  Precision probe: temp file error: {e}"));
                return None;
            }
        };
        let tmp_str = tmp_file.path().to_string_lossy().to_string();

        let mut args: Vec<String> = vec![
            "-y".into(),
            "-ss".into(),
            format!("{:.2}", seek_secs),
            "-t".into(),
            format!("{:.2}", sample_duration),
            "-i".into(),
            input_path.to_string(),
            "-map".into(),
            "0:v:0".into(),
            "-an".into(),
            "-sn".into(),
        ];
        if threads > 0 {
            args.extend(vec!["-threads".into(), threads.to_string()]);
        }
        args.extend(video_args.iter().cloned());
        args.extend(vec!["-pix_fmt".into(), pix_fmt.to_string()]);
        args.push(tmp_str.clone());

        let result = run_ffmpeg_with_progress(
            &args,
            sample_duration,
            Some((sample_num as u8, seek_points.len() as u8)),
            low_priority,
            stderr_log.clone(),
            &format!("probe_{}", sample_num),
            sink,
            batch_control,
        )
        .await;

        match result {
            Ok(r) if r.was_cancelled_current || r.was_cancelled_all => {
                return None;
            }
            Ok(r) if r.exit_code != 0 => {
                sink.log(&format!(
                    "  Precision probe: sample {} failed (exit code {})",
                    sample_num, r.exit_code
                ));
                return None;
            }
            Err(e) => {
                sink.log(&format!(
                    "  Precision probe: sample {} error: {}",
                    sample_num, e
                ));
                return None;
            }
            _ => {}
        }

        // Stat the sample output to get its size (file auto-cleaned on drop)
        let sample_size = std::fs::metadata(tmp_file.path())
            .map(|m| m.len())
            .unwrap_or(0);

        if sample_size == 0 {
            sink.log(&format!(
                "  Precision probe: sample {} produced empty output",
                sample_num
            ));
            return None;
        }

        total_bits += sample_size as f64 * 8.0;
        total_sample_secs += sample_duration;
    }

    sink.file_progress(100.0, 0.0, 0.0, None);

    if total_sample_secs > 0.0 {
        let avg_bps = total_bits / total_sample_secs;
        let avg_mbps = avg_bps / 1_000_000.0;
        sink.log(&format!(
            "  Precision probe: estimated CRF bitrate {:.2}Mbps",
            avg_mbps
        ));
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
        "-err_detect".into(),
        "ignore_err".into(),
        "-probesize".into(),
        "100M".into(),
        "-analyzeduration".into(),
        "100M".into(),
        "-y".into(),
        "-i".into(),
        input_path.to_string(),
        "-map".into(),
        "0:v:0".into(),
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
            "-vf".into(),
            "pad=ceil(iw/2)*2:ceil(ih/2)*2".into(),
            "-pix_fmt".into(),
            "yuv420p".into(),
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
            "-c:s".into(),
            "copy".into(),
            "-disposition:s:0".into(),
            "default".into(),
        ]);
    }

    args.push(output_path.to_string());
    args
}

/// Returns true if ffmpeg has an encoder for the given audio codec name.
/// Codecs like DTS and TrueHD are decode-only in ffmpeg.
fn has_ffmpeg_encoder(audio_codec: &str) -> bool {
    matches!(
        audio_codec,
        "ac3"
            | "eac3"
            | "aac"
            | "opus"
            | "vorbis"
            | "flac"
            | "mp3"
            | "mp2"
            | "pcm_s16le"
            | "pcm_s24le"
            | "pcm_s32le"
            | "pcm_f32le"
            | "pcm_f64le"
            | "pcm_s16be"
            | "pcm_s24be"
            | "pcm_s32be"
            | "pcm_mulaw"
            | "pcm_alaw"
    )
}

/// Map a codec name to its ffmpeg encoder name and optional max bitrate (kbps).
/// Returns (encoder_name, max_kbps) where max_kbps is None for unlimited.
fn ffmpeg_encoder_for_codec(codec: &str) -> (String, Option<u32>) {
    match codec {
        "opus" => ("libopus".to_string(), Some(512)),
        "vorbis" => ("libvorbis".to_string(), Some(500)),
        other => (other.to_string(), None),
    }
}

/// Build per-stream audio arguments from pre-probed audio stream data (§6.2).
/// Uses AudioStrategy to determine copy/re-encode behaviour per stream.
fn build_audio_args_from_probe(
    audio_streams: &[crate::queue::AudioStreamInfo],
    strategy: &AudioStrategy,
    sink: &dyn EventSink,
) -> (Vec<String>, Vec<String>) {
    if audio_streams.is_empty() {
        sink.log("  WARNING: No audio streams found");
        return (Vec::new(), Vec::new());
    }

    let mut map_args = Vec::new();
    let mut codec_args = Vec::new();
    let mut output_idx: u32 = 0;

    for stream in audio_streams {
        // Unknown codecs can't be decoded or muxed - skip entirely.
        // Do NOT increment output_idx for skipped streams.
        if stream.codec == "unknown" {
            sink.log(&format!(
                "  WARNING: Audio {} has an unrecognised codec and will be excluded from the output",
                stream.index
            ));
            continue;
        }

        map_args.extend(["-map".into(), format!("0:a:{}", stream.index)]);

        match strategy {
            AudioStrategy::CopyCapped { cap_kbps } => {
                if stream.bitrate_kbps <= *cap_kbps {
                    codec_args.extend([format!("-c:a:{output_idx}"), "copy".into()]);
                    sink.log(&format!(
                        "  Audio {} : {} @ {}kbps - copying",
                        stream.index, stream.codec, stream.bitrate_kbps
                    ));
                } else {
                    let (target_codec, target_br) = if has_ffmpeg_encoder(&stream.codec) {
                        let (enc_name, max_kbps) = ffmpeg_encoder_for_codec(&stream.codec);
                        let br = match max_kbps {
                            Some(max) if *cap_kbps > max => max,
                            _ => *cap_kbps,
                        };
                        (enc_name, br)
                    } else {
                        sink.log(&format!(
                            "  Audio {} : {} has no ffmpeg encoder, falling back to EAC3",
                            stream.index, stream.codec
                        ));
                        ("eac3".to_string(), *cap_kbps)
                    };
                    codec_args.extend([
                        format!("-c:a:{output_idx}"),
                        target_codec.clone(),
                        format!("-b:a:{output_idx}"),
                        format!("{target_br}k"),
                    ]);
                    sink.log(&format!(
                        "  Audio {} : {} @ {}kbps - encoding to {} @ {}kbps",
                        stream.index, stream.codec, stream.bitrate_kbps, target_codec, target_br
                    ));
                }
            }
            AudioStrategy::CompatCapped { cap_kbps } => {
                if stream.codec == "ac3" && stream.bitrate_kbps <= *cap_kbps {
                    codec_args.extend([format!("-c:a:{output_idx}"), "copy".into()]);
                    sink.log(&format!(
                        "  Audio {} : AC3 @ {}kbps - copying",
                        stream.index, stream.bitrate_kbps
                    ));
                } else {
                    let target_br = stream.bitrate_kbps.min(*cap_kbps);
                    codec_args.extend([
                        format!("-c:a:{output_idx}"),
                        "ac3".to_string(),
                        format!("-b:a:{output_idx}"),
                        format!("{target_br}k"),
                    ]);
                    sink.log(&format!(
                        "  Audio {} : {} @ {}kbps - encoding to AC3 @ {}kbps",
                        stream.index, stream.codec, stream.bitrate_kbps, target_br
                    ));
                }
            }
        }

        // Only increment after a stream is actually mapped (not skipped)
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
        "Shutdown" => (
            "osascript",
            vec!["-e", "tell app \"System Events\" to shut down"],
        ),
        #[cfg(target_os = "macos")]
        "Sleep" => ("pmset", vec!["sleepnow"]),
        #[cfg(target_os = "macos")]
        "Log Out" => (
            "osascript",
            vec!["-e", "tell app \"System Events\" to log out"],
        ),

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

// ── Shared ffmpeg stderr progress parser ──────────────────────

/// Atomic counters updated by the stderr reader thread.
/// `progress_secs` stores the bits of an f64 (via `f64::to_bits`).
/// `frame_count` stores the raw frame count.
pub struct FfmpegProgress {
    pub progress_secs: Arc<std::sync::atomic::AtomicU64>,
    pub frame_count: Arc<std::sync::atomic::AtomicU64>,
}

impl FfmpegProgress {
    pub fn new() -> Self {
        Self {
            progress_secs: Arc::new(std::sync::atomic::AtomicU64::new(0)),
            frame_count: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        }
    }

    /// Read the current progress as seconds.
    pub fn secs(&self) -> f64 {
        f64::from_bits(
            self.progress_secs
                .load(std::sync::atomic::Ordering::Relaxed),
        )
    }

    /// Read the current frame count.
    pub fn frames(&self) -> u64 {
        self.frame_count.load(std::sync::atomic::Ordering::Relaxed)
    }
}

/// Convert a tokio async stderr to a std blocking stderr.
fn tokio_stderr_to_std(tokio_stderr: tokio::process::ChildStderr) -> std::process::ChildStderr {
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
}

/// Shared type for the per-batch stderr log that multiple stderr reader
/// threads can write to concurrently.
pub type SharedStderrLog = Arc<std::sync::Mutex<std::io::BufWriter<std::fs::File>>>;

/// Open (or create) a single timestamped ffmpeg stderr log file in
/// `output_dir/ffmpeg_logs/`. Intended to be called once per batch and
/// shared across all ffmpeg invocations in that batch via `Arc<Mutex<…>>`.
///
/// Returns `None` if the directory can't be created or the file can't be
/// opened.
pub fn open_stderr_log(output_dir: &std::path::Path) -> Option<SharedStderrLog> {
    let log_dir = output_dir.join("ffmpeg_logs");
    let _ = std::fs::create_dir_all(&log_dir);
    let filename = format!(
        "ffmpeg_stderr_{}.log",
        chrono::Local::now().format("%Y%m%d_%H%M%S")
    );
    let file = std::fs::File::create(log_dir.join(filename)).ok()?;
    Some(Arc::new(std::sync::Mutex::new(std::io::BufWriter::new(
        file,
    ))))
}

/// Clean up the `ffmpeg_logs` directory under `output_dir`:
///   1. Delete the file at `log_path` if it is empty (no stderr was captured).
///   2. Keep at most `max_logs` log files — oldest are deleted first.
pub fn cleanup_stderr_logs(
    output_dir: &std::path::Path,
    log_path: Option<&std::path::Path>,
    max_logs: usize,
) {
    // Remove the file if it is empty
    if let Some(p) = log_path {
        if let Ok(meta) = std::fs::metadata(p) {
            if meta.len() == 0 {
                let _ = std::fs::remove_file(p);
            }
        }
    }

    let log_dir = output_dir.join("ffmpeg_logs");
    if !log_dir.is_dir() {
        return;
    }

    // Collect .log files sorted oldest-first by modified time
    let mut entries: Vec<(std::time::SystemTime, std::path::PathBuf)> = std::fs::read_dir(&log_dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .map(|ext| ext == "log")
                .unwrap_or(false)
        })
        .filter_map(|e| {
            let modified = e.metadata().ok()?.modified().ok()?;
            Some((modified, e.path()))
        })
        .collect();
    entries.sort_by_key(|(t, _)| *t);

    // Delete oldest until at most max_logs remain
    while entries.len() > max_logs {
        let (_, path) = entries.remove(0);
        let _ = std::fs::remove_file(path);
    }
}

/// Spawn a blocking thread that reads ffmpeg stderr, updates the
/// progress counters, and appends all output to the shared batch log
/// file. A separator header is written before the first byte so that
/// each invocation's output is distinguishable in the combined log.
///
/// `label` is included in the separator (typically the filename being
/// encoded).
pub fn spawn_stderr_reader(
    tokio_stderr: tokio::process::ChildStderr,
    progress: &FfmpegProgress,
    stderr_log: Option<SharedStderrLog>,
    label: &str,
) -> std::thread::JoinHandle<()> {
    let std_stderr = tokio_stderr_to_std(tokio_stderr);
    let progress_secs = Arc::clone(&progress.progress_secs);
    let frames = Arc::clone(&progress.frame_count);
    let label = label.to_owned();

    std::thread::spawn(move || {
        use std::io::{Read, Write};
        let mut reader = std::io::BufReader::new(std_stderr);
        let mut buf = [0u8; 4096];
        let mut line_buf = Vec::<u8>::with_capacity(256);

        // Write separator header into shared log
        if let Some(ref log) = stderr_log {
            if let Ok(mut w) = log.lock() {
                let _ = writeln!(
                    w,
                    "\n=== ffmpeg stderr [{}] [{}] ===",
                    label,
                    chrono::Local::now().format("%Y-%m-%d %H:%M:%S"),
                );
            }
        }

        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Write raw bytes to shared log file
                    if let Some(ref log) = stderr_log {
                        if let Ok(mut w) = log.lock() {
                            let _ = w.write_all(&buf[..n]);
                        }
                    }
                    for &byte in &buf[..n] {
                        if byte == b'\r' || byte == b'\n' {
                            if !line_buf.is_empty() {
                                if let Some(pos) = line_buf.windows(5).position(|w| w == b"time=") {
                                    let start = pos + 5;
                                    let end = line_buf[start..]
                                        .iter()
                                        .position(|&b| b == b' ' || b == b'\t')
                                        .map(|p| start + p)
                                        .unwrap_or(line_buf.len());
                                    if let Ok(s) = std::str::from_utf8(&line_buf[start..end]) {
                                        if let Some(secs) = parse_ffmpeg_time(s) {
                                            progress_secs.store(
                                                secs.to_bits(),
                                                std::sync::atomic::Ordering::Relaxed,
                                            );
                                        }
                                    }
                                }
                                if let Some(pos) = line_buf.windows(6).position(|w| w == b"frame=")
                                {
                                    let start = pos + 6;
                                    let end = line_buf[start..]
                                        .iter()
                                        .position(|&b| b == b' ' || b == b'\t')
                                        .map(|p| start + p)
                                        .unwrap_or(line_buf.len());
                                    if let Ok(s) = std::str::from_utf8(&line_buf[start..end]) {
                                        if let Ok(fc) = s.trim().parse::<u64>() {
                                            frames.store(fc, std::sync::atomic::Ordering::Relaxed);
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
        // Flush on exit
        if let Some(ref log) = stderr_log {
            if let Ok(mut w) = log.lock() {
                let _ = w.flush();
            }
        }
    })
}

/// Parse ffmpeg's time= format "HH:MM:SS.ms" or "HH:MM:SS" into seconds.
pub fn parse_ffmpeg_time(s: &str) -> Option<f64> {
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
