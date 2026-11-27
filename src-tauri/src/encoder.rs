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

// ── Rate-control parameters (options struct) ───────────────────

/// Rate control parameters for the encoding decision.
pub struct RateControlParams<'a> {
    pub mode: &'a str,
    pub qp_i: u32,
    pub qp_p: u32,
    pub crf_val: u32,
}

// ── Encoder info & abstraction layer ────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncoderInfo {
    pub name: String,
    pub codec_family: String, // "hevc", "h264", or "av1"
    pub is_hardware: bool,
}

/// Encoder priority tables per platform (§8.2). Detection tests hardware
/// encoders in this order and `pick_encoder` takes the first that works, so
/// vendor APIs come before the generic D3D12 and Vulkan paths.
///
/// `av1_vulkan` is left out of the tables, so it is never chosen
/// automatically: in ffmpeg n8.1.3 it writes the aligned surface size as the
/// AV1 frame size with no render-size crop (1920x1080 becomes 1920x1088 on
/// RADV). It still works when forced with `--encoder av1_vulkan`.
#[cfg(target_os = "windows")]
const HEVC_PRIORITY: &[&str] = &[
    "hevc_amf",
    "hevc_nvenc",
    "hevc_qsv",
    "hevc_d3d12va",
    "hevc_vulkan",
    "libx265",
];
#[cfg(target_os = "windows")]
const H264_PRIORITY: &[&str] = &[
    "h264_amf",
    "h264_nvenc",
    "h264_qsv",
    "h264_d3d12va",
    "h264_vulkan",
    "libx264",
];
#[cfg(target_os = "windows")]
const AV1_PRIORITY: &[&str] = &[
    "av1_amf",
    "av1_nvenc",
    "av1_qsv",
    "av1_d3d12va",
    "libsvtav1",
];

#[cfg(target_os = "macos")]
const HEVC_PRIORITY: &[&str] = &["hevc_videotoolbox", "libx265"];
#[cfg(target_os = "macos")]
const H264_PRIORITY: &[&str] = &["h264_videotoolbox", "libx264"];
#[cfg(target_os = "macos")]
const AV1_PRIORITY: &[&str] = &["av1_videotoolbox", "libsvtav1"];

#[cfg(target_os = "linux")]
const HEVC_PRIORITY: &[&str] = &[
    "hevc_vaapi",
    "hevc_nvenc",
    "hevc_qsv",
    "hevc_vulkan",
    "libx265",
];
#[cfg(target_os = "linux")]
const H264_PRIORITY: &[&str] = &[
    "h264_vaapi",
    "h264_nvenc",
    "h264_qsv",
    "h264_vulkan",
    "libx264",
];
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
        // Vulkan: no -rc_mode. In auto mode a bitrate selects VBR when the
        // driver offers it and CBR otherwise; an explicit mode the driver
        // lacks is a hard error.
        "hevc_vulkan" | "h264_vulkan" | "av1_vulkan" => {
            vec!["-b:v".into(), target.into(), "-maxrate".into(), peak.into()]
        }
        // D3D12: explicit mode, because auto picks QVBR when both a bitrate
        // and a quality are set.
        "hevc_d3d12va" | "h264_d3d12va" | "av1_d3d12va" => vec![
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
        // SVT-AV1 rejects -maxrate in VBR mode (rate_control_mode=1);
        // it only accepts max_bit_rate with CRF (capped-CRF).
        "libsvtav1" => vec!["-preset".into(), "6".into(), "-b:v".into(), target.into()],
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
        "hevc_vaapi" | "h264_vaapi" => {
            vec!["-rc_mode".into(), "CQP".into(), "-qp".into(), qi.into()]
        }
        // av1_vaapi has no private -qp (ffmpeg ignores it: "has not been
        // used"); the shared VAAPI code takes the q-index from
        // -global_quality (vaapi_encode.c:1421).
        "av1_vaapi" => vec![
            "-rc_mode".into(),
            "CQP".into(),
            "-global_quality".into(),
            av1_qindex_str(qi),
        ],
        "hevc_vulkan" | "h264_vulkan" => vec!["-qp".into(), qi.into()],
        "av1_vulkan" => vec!["-qp".into(), av1_qindex_str(qi)],
        "hevc_d3d12va" | "h264_d3d12va" => {
            vec!["-rc_mode".into(), "CQP".into(), "-qp".into(), qi.into()]
        }
        // av1_d3d12va caps -qp at 52 (d3d12va_encode_av1.c:1114) but uses the
        // value directly as the 0-255 base q-index (:761, :784), so -qp
        // reaches only the near-lossless end. -global_quality has the full
        // range (d3d12va_encode.c:1139-1143).
        "av1_d3d12va" => vec![
            "-rc_mode".into(),
            "CQP".into(),
            "-global_quality".into(),
            av1_qindex_str(qi),
        ],
        "libx265" | "libx264" => vec!["-preset".into(), "slow".into(), "-qp".into(), qi.into()],
        // libsvtav1 selects constant QP itself when -qp is set; it has no
        // -rc option (ffmpeg reports "-rc ... has not been used").
        "libsvtav1" => vec!["-preset".into(), "6".into(), "-qp".into(), qi.into()],
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

/// Whether the encoder name refers to a VAAPI hardware encoder.
pub fn is_vaapi_encoder(name: &str) -> bool {
    name.ends_with("_vaapi")
}

/// Whether the encoder name refers to a QSV hardware encoder.
pub fn is_qsv_encoder(name: &str) -> bool {
    name.ends_with("_qsv")
}

/// Whether the encoder name refers to a Vulkan Video hardware encoder.
pub fn is_vulkan_encoder(name: &str) -> bool {
    name.ends_with("_vulkan")
}

/// Whether the encoder name refers to a Direct3D 12 hardware encoder.
pub fn is_d3d12_encoder(name: &str) -> bool {
    name.ends_with("_d3d12va")
}

/// Map the app's QP (the H.264/HEVC scale, 0-51) onto the AV1 q-index
/// scale (0-255) used by `av1_vaapi` and `av1_vulkan`. Linear, 51 maps to
/// 255; the defaults 20 and 22 become 100 and 110. Not calibrated for equal
/// perceptual quality against the HEVC encoders.
pub fn av1_qindex(qp: u32) -> u32 {
    qp.saturating_mul(5).clamp(1, 255)
}

/// `av1_qindex` for a QP that arrives pre-formatted. A value that does not
/// parse is passed through so ffmpeg reports it.
fn av1_qindex_str(qp: &str) -> String {
    qp.parse::<u32>()
        .map(|q| av1_qindex(q).to_string())
        .unwrap_or_else(|_| qp.to_string())
}

/// Choose the encoder for a codec family in auto mode: the first working
/// hardware encoder in detection order (which is priority-table order),
/// else the software encoder. Every auto-selection site uses this, so the
/// backend, the GUI summary and the CLI plan agree.
pub fn pick_encoder(family: &str, detected: &[EncoderInfo]) -> String {
    detected
        .iter()
        .find(|e| e.codec_family == family && e.is_hardware)
        .map(|e| e.name.clone())
        .unwrap_or_else(|| software_fallback(family).to_string())
}

/// Operating system whose hardware-device arguments to build. Passed
/// explicitly so the Windows arms can be unit-tested on Linux.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOs {
    Linux,
    Windows,
    Other,
}

impl TargetOs {
    pub const fn current() -> Self {
        if cfg!(target_os = "linux") {
            TargetOs::Linux
        } else if cfg!(target_os = "windows") {
            TargetOs::Windows
        } else {
            TargetOs::Other
        }
    }
}

/// Hardware set-up arguments for one encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HwArgs {
    /// Device arguments that must precede `-i`.
    pub pre_input: Vec<String>,
    /// Filter that moves frames onto the device (chained after any tonemap).
    pub upload_filter: Option<String>,
    /// Whether `-pix_fmt` should be emitted for the encode.
    pub emit_pix_fmt: bool,
    /// True when the input is decoded on the VAAPI device (10-bit VAAPI).
    pub hwaccel_decode: bool,
}

/// Hardware device and upload arguments for `enc`. Pure: the render node
/// and OS are passed in. Used for real encodes and for detection test
/// encodes, so both exercise the same arguments.
///
/// - VAAPI (Linux): 10-bit without tonemap decodes on the device
///   (`-hwaccel vaapi` plus `scale_vaapi=format=p010`), because software
///   P010 `hwupload` gives corrupt frames on AMD; otherwise `-vaapi_device`
///   plus `format=nv12,hwupload`.
/// - QSV (Linux): `qsv=qs` device and `hwupload=extra_hw_frames=64`.
/// - Vulkan: on Linux derived from the DRM render node so it lands on the
///   same GPU as VAAPI; on Windows the default device. No `-pix_fmt`,
///   because the encoder accepts only Vulkan frames.
/// - D3D12 (Windows): `d3d12va` device, same upload, no `-pix_fmt`.
///
/// A missing render node gives a sentinel device path so ffmpeg fails
/// with a clear message instead of picking an arbitrary device.
pub fn hw_device_args(
    enc: &str,
    pix_fmt: &str,
    has_tonemap: bool,
    render_node: Option<&str>,
    os: TargetOs,
) -> HwArgs {
    hw_device_args_with(enc, pix_fmt, has_tonemap, render_node, os, true)
}

/// `hw_device_args`, with `device_decode` false keeping VAAPI 10-bit
/// decode in software (`vaapi_device_decode_ok`).
pub fn hw_device_args_with(
    enc: &str,
    pix_fmt: &str,
    has_tonemap: bool,
    render_node: Option<&str>,
    os: TargetOs,
    device_decode: bool,
) -> HwArgs {
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<String>>();
    let ten_bit = pix_fmt == "p010le";
    let mut hw = HwArgs {
        pre_input: Vec::new(),
        upload_filter: None,
        emit_pix_fmt: true,
        hwaccel_decode: false,
    };

    if os == TargetOs::Linux && is_vaapi_encoder(enc) {
        match render_node {
            Some(node) if ten_bit && !has_tonemap && device_decode => {
                hw.pre_input = s(&[
                    "-hwaccel",
                    "vaapi",
                    "-hwaccel_device",
                    node,
                    "-hwaccel_output_format",
                    "vaapi",
                ]);
                hw.upload_filter = Some("scale_vaapi=format=p010".into());
                hw.hwaccel_decode = true;
                hw.emit_pix_fmt = false;
            }
            Some(node) => {
                hw.pre_input = s(&["-vaapi_device", node]);
                hw.upload_filter = Some(if ten_bit && !has_tonemap {
                    "format=p010,hwupload".into()
                } else {
                    "format=nv12,hwupload".into()
                });
            }
            None => {
                hw.pre_input = s(&["-vaapi_device", "/dev/dri/renderD_missing"]);
            }
        }
    } else if os == TargetOs::Linux && is_qsv_encoder(enc) {
        hw.pre_input = match render_node {
            Some(node) => s(&[
                "-init_hw_device",
                "qsv=qs",
                "-filter_hw_device",
                "qs",
                "-qsv_device",
                node,
            ]),
            None => s(&["-init_hw_device", "qsv=qs_child_device_missing"]),
        };
        hw.upload_filter = Some(if ten_bit {
            "format=p010le,hwupload=extra_hw_frames=64".into()
        } else {
            "format=nv12,hwupload=extra_hw_frames=64".into()
        });
    } else if is_vulkan_encoder(enc) && os != TargetOs::Other {
        hw.pre_input = if os == TargetOs::Linux {
            let drm = format!(
                "drm=dr:{}",
                render_node.unwrap_or("/dev/dri/renderD_missing")
            );
            vec![
                "-init_hw_device".into(),
                drm,
                "-init_hw_device".into(),
                "vulkan=vk@dr".into(),
                "-filter_hw_device".into(),
                "vk".into(),
            ]
        } else {
            s(&["-init_hw_device", "vulkan=vk", "-filter_hw_device", "vk"])
        };
        hw.upload_filter = Some(upload_filter_for(ten_bit));
        hw.emit_pix_fmt = false;
    } else if is_d3d12_encoder(enc) && os == TargetOs::Windows {
        hw.pre_input = s(&[
            "-init_hw_device",
            "d3d12va=d3d12",
            "-filter_hw_device",
            "d3d12",
        ]);
        hw.upload_filter = Some(upload_filter_for(ten_bit));
        hw.emit_pix_fmt = false;
    }
    hw
}

fn upload_filter_for(ten_bit: bool) -> String {
    if ten_bit {
        "format=p010le,hwupload".into()
    } else {
        "format=nv12,hwupload".into()
    }
}

/// Arguments for the single-frame detection encode of `encoder_name`, or
/// `None` when the encoder cannot work at all (VAAPI or QSV on Linux with no
/// render node). Uses `hw_device_args` so detection and real encodes set up
/// the device the same way.
fn test_encode_args(
    encoder_name: &str,
    render_node: Option<&str>,
    os: TargetOs,
    out_path: &str,
) -> Option<Vec<String>> {
    if os == TargetOs::Linux
        && render_node.is_none()
        && (is_vaapi_encoder(encoder_name) || is_qsv_encoder(encoder_name))
    {
        return None;
    }
    let hw = hw_device_args(encoder_name, "nv12", false, render_node, os);
    let mut args = hw.pre_input;
    // 256x256 minimum - some HW encoders reject smaller resolutions
    args.extend(
        [
            "-y",
            "-f",
            "lavfi",
            "-i",
            "color=black:s=256x256:d=0.04:r=25",
            "-frames:v",
            "1",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    match hw.upload_filter {
        Some(f) => args.extend(["-vf".into(), f]),
        // nv12 - required by most hardware encoders. VAAPI and QSV have
        // never had it here (they get no upload filter off Linux).
        None if !is_vaapi_encoder(encoder_name) && !is_qsv_encoder(encoder_name) => {
            args.extend(["-pix_fmt".into(), "nv12".into()])
        }
        None => {}
    }
    args.extend(["-c:v".into(), encoder_name.into(), out_path.into()]);
    Some(args)
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
    rc: &RateControlParams,
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
        } else if rc.mode == "CRF" || rc.mode == "crf" {
            EncodeDecision::Crf {
                crf: rc.crf_val,
                qi_fallback: rc.qp_i,
                qp_fallback: rc.qp_p,
            }
        } else {
            EncodeDecision::Cqp {
                qi: rc.qp_i,
                qp: rc.qp_p,
            }
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

// ── ffmpeg capability probe ─────────────────────────────────────

/// What the resolved ffmpeg can do beyond listing encoders. Set by
/// `detect_encoders`; all false until detection has run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FfmpegCaps {
    /// libx265 has the `dolbyvision` option (native RPU coding).
    pub dovi_x265: bool,
    /// libsvtav1 has the `dolbyvision` option.
    pub dovi_svtav1: bool,
    /// libx265 was built with HDR10_PLUS, so `dhdr10-info` works.
    pub x265_hdr10plus: bool,
    /// The `hevc_metadata` BSF has `width` and `height` (absent in 6.1).
    pub hevc_metadata_size: bool,
    /// ffmpeg predates `-fps_mode` (before 5.1), so `-vsync` is used. Only
    /// set when the option is reported as unrecognised; a failed probe
    /// leaves the modern spelling in place.
    pub no_fps_mode: bool,
    /// Major release number from `ffmpeg -version`. `None` when it cannot be
    /// read (git and date-stamped builds, a failed probe); nothing is gated
    /// on an unknown version.
    pub ffmpeg_major: Option<u32>,
}

static FFMPEG_CAPS: std::sync::RwLock<FfmpegCaps> = std::sync::RwLock::new(FfmpegCaps {
    dovi_x265: false,
    dovi_svtav1: false,
    x265_hdr10plus: false,
    hevc_metadata_size: false,
    no_fps_mode: false,
    ffmpeg_major: None,
});

/// Major release number from `ffmpeg -version` output ("ffmpeg version
/// n8.1.3 ..."), or from a bare version string. `None` for builds without a
/// release number ("N-12345-g...", "git-...", "2024-05-01-git-...").
pub fn parse_ffmpeg_major(version_output: &str) -> Option<u32> {
    let token = version_output
        .lines()
        .find_map(|l| l.strip_prefix("ffmpeg version "))
        .unwrap_or(version_output)
        .split_whitespace()
        .next()?;
    let token = token.strip_prefix('n').unwrap_or(token);
    let (major, _) = token.split_once('.')?;
    if major.is_empty() || !major.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    major.parse().ok()
}

/// Whether ffmpeg's stderr says `-fps_mode` is not an option (before 5.1).
pub fn fps_mode_unrecognised(stderr: &str) -> bool {
    stderr.contains("Unrecognized option 'fps_mode'")
}

/// The capabilities found by the last `detect_encoders` run.
pub fn ffmpeg_caps() -> FfmpegCaps {
    FFMPEG_CAPS.read().map(|c| *c).unwrap_or_default()
}

/// Whether `ffmpeg -h encoder=X` output lists the `dolbyvision` option.
pub fn help_has_dolbyvision(help_stdout: &str) -> bool {
    help_stdout
        .lines()
        .any(|l| l.trim_start().starts_with("-dolbyvision"))
}

/// Whether `ffmpeg -h bsf=hevc_metadata` output lists `width` and `height`.
pub fn bsf_help_has_size(help_stdout: &str) -> bool {
    let has = |opt: &str| {
        help_stdout
            .lines()
            .any(|l| l.split_whitespace().next() == Some(opt))
    };
    has("-width") && has("-height")
}

/// Whether an x265 encode's stderr says `dhdr10-info` was ignored because
/// x265 was built without HDR10_PLUS. x265 still exits 0 in that case.
pub fn dhdr10_disabled(stderr: &str) -> bool {
    stderr.contains("dhdr10-info disabled")
}

/// Smallest valid HDR10+ JSON (one frame), for the `dhdr10-info` probe.
const HDR10PLUS_PROBE_JSON: &str = r#"{"JSONInfo":{"HDR10plusProfile":"A","Version":"1.0"},"SceneInfo":[{"BezierCurveData":{"Anchors":[256,256,256,256,256,256,256,256,256],"KneePointX":0,"KneePointY":0},"LuminanceParameters":{"AverageRGB":0,"LuminanceDistributions":{"DistributionIndex":[1,5,10,25,50,75,90,95,99],"DistributionValues":[0,0,0,0,0,0,0,0,0]},"MaxScl":[0,0,0]},"NumberOfWindows":1,"TargetedSystemDisplayMaximumLuminance":400}],"SceneInfoSummary":{"SceneFirstFrameIndex":[0],"SceneFrameNumbers":[1]},"ToolInfo":{"Tool":"histv","Version":"probe"}}"#;

/// Time limit for one detection-time ffmpeg run (capability probes and the
/// per-encoder test encode). A hung GPU driver must not hang detection.
const DETECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Run `cmd` with a null stdin and collect its output, or `None` if it
/// cannot start or outlives `limit` (the child is then killed).
pub(crate) async fn output_with_timeout(
    mut cmd: tokio::process::Command,
    limit: std::time::Duration,
) -> Option<std::process::Output> {
    let child = cmd
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .ok()?;
    tokio::time::timeout(limit, child.wait_with_output())
        .await
        .ok()?
        .ok()
}

async fn ffmpeg_stdout_stderr(args: &[&str]) -> Option<(String, String)> {
    let mut cmd = ffbin::ffmpeg_command();
    cmd.args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let out = output_with_timeout(cmd, DETECT_TIMEOUT).await?;
    Some((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ))
}

async fn probe_ffmpeg_caps(has_x265: bool, has_svtav1: bool) -> FfmpegCaps {
    let mut caps = FfmpegCaps::default();
    if let Some((out, _)) = ffmpeg_stdout_stderr(&["-version"]).await {
        caps.ffmpeg_major = parse_ffmpeg_major(&out);
    }
    if let Some((out, _)) = ffmpeg_stdout_stderr(&["-hide_banner", "-h", "bsf=hevc_metadata"]).await
    {
        caps.hevc_metadata_size = bsf_help_has_size(&out);
    }
    if let Some((_, err)) = ffmpeg_stdout_stderr(&[
        "-hide_banner",
        "-f",
        "lavfi",
        "-i",
        "color=black:s=16x16:d=0.04",
        "-frames:v",
        "1",
        "-fps_mode",
        "passthrough",
        "-f",
        "null",
        "-",
    ])
    .await
    {
        caps.no_fps_mode = fps_mode_unrecognised(&err);
    }
    if has_x265 {
        if let Some((out, _)) =
            ffmpeg_stdout_stderr(&["-hide_banner", "-h", "encoder=libx265"]).await
        {
            caps.dovi_x265 = help_has_dolbyvision(&out);
        }
        if let Ok(mut json) = tempfile::Builder::new()
            .prefix("histv_hdr10p_probe_")
            .suffix(".json")
            .tempfile()
        {
            use std::io::Write;
            if json.write_all(HDR10PLUS_PROBE_JSON.as_bytes()).is_ok() {
                let param = format!(
                    "dhdr10-info={}",
                    x265_param_path_escape(&json.path().to_string_lossy())
                );
                if let Some((_, err)) = ffmpeg_stdout_stderr(&[
                    "-hide_banner",
                    "-f",
                    "lavfi",
                    "-i",
                    "color=black:s=256x256:d=0.04",
                    "-frames:v",
                    "1",
                    "-c:v",
                    "libx265",
                    "-pix_fmt",
                    "yuv420p10le",
                    "-x265-params",
                    &param,
                    "-f",
                    "null",
                    "-",
                ])
                .await
                {
                    caps.x265_hdr10plus = !dhdr10_disabled(&err) && !err.contains("Error");
                }
            }
        }
    }
    if has_svtav1 {
        if let Some((out, _)) =
            ffmpeg_stdout_stderr(&["-hide_banner", "-h", "encoder=libsvtav1"]).await
        {
            caps.dovi_svtav1 = help_has_dolbyvision(&out);
        }
    }
    caps
}

/// Escape a path for use as a value inside `-x265-params`.
///
/// ffmpeg parses the option with `av_dict_parse_string(.., "=", ":", 0)`,
/// whose tokens (`av_get_token`, libavutil/avstring.c) end at `:`, treat
/// `\` as an escape and `'` as a quote, and lose leading and trailing
/// whitespace. Backslash-escaping those characters (and `=`) hands x265 the
/// path unchanged; any other character, non-ASCII included, passes as is.
pub fn x265_param_path_escape(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 8);
    for c in path.chars() {
        if matches!(c, '\\' | '\'' | ':' | '=') || c.is_ascii_whitespace() {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Add `key=value` to the `-x265-params` argument, creating it if absent.
pub fn add_x265_param(args: &mut Vec<String>, param: &str) {
    if let Some(i) = args.iter().position(|a| a == "-x265-params") {
        if let Some(v) = args.get_mut(i + 1) {
            v.push(':');
            v.push_str(param);
            return;
        }
    }
    args.push("-x265-params".into());
    args.push(param.into());
}

// ── HDR10+ routing ──────────────────────────────────────────────

/// How an HDR10+ source keeps its dynamic metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hdr10PlusRoute {
    /// libx265 codes the metadata from a JSON file (`dhdr10-info`).
    X265Native,
    /// The Rust injector writes SEIs into the encoded HEVC afterwards.
    Injector,
}

/// Why HDR10+ dynamic metadata is lost, worded for "Static HDR10 only (..)".
pub mod hdr10plus_loss {
    pub const NO_CRATE: &str = "hdr10plus support not available";
    pub const NOT_HEVC: &str = "HDR10+ is only kept in HEVC output";
    pub const DOVI_KEPT: &str = "Dolby Vision is kept instead";
    pub const X265_NO_SUPPORT: &str = "this ffmpeg's libx265 cannot write HDR10+";
    pub const X265_PROFILE: &str =
        "the HDR10+ metadata is neither profile A nor B, which libx265 needs";
    pub const VAAPI_OLD_FFMPEG: &str =
        "this ffmpeg's demuxer mis-orders the stream's parameter sets";
}

/// The oldest ffmpeg major version whose `hevc_mp4toannexb` puts the
/// container's parameter sets ahead of the in-band ones. Older releases put
/// them after, and `hevc_vaapi` (AMD/Mesa) writes a different PPS in-band
/// than in the container header, so the decoder picks the stale one.
const VAAPI_MIN_FFMPEG_MAJOR: u32 = 7;

/// HDR10+ routing for one file, decided before any extraction.
///
/// - `Ok(None)`: nothing to keep (no HDR10+, or HDR preservation off).
/// - `Ok(Some(route))`: libx265 codes the metadata from a JSON file
///   (`dhdr10-info`), or a hardware HEVC encoder runs without B-frames and
///   the injector writes the SEIs afterwards.
/// - `Err(reason)`: the metadata cannot be kept with this encoder. libx265
///   always reorders pictures, so it has no injector fallback.
///
/// `profile` is the source's HDR10+ profile from the probe ("A", "B",
/// "N/A", or empty when unknown). With Dolby Vision kept, only native DV
/// through libx265 can carry HDR10+ as well.
#[allow(clippy::too_many_arguments)]
pub fn hdr10plus_route(
    has_hdr10plus: bool,
    can_process: bool,
    preserve_hdr: bool,
    encoder: &str,
    family: &str,
    dovi: Option<DoviRoute>,
    caps: &FfmpegCaps,
    profile: &str,
) -> Result<Option<Hdr10PlusRoute>, &'static str> {
    use hdr10plus_loss::*;
    if !has_hdr10plus || !preserve_hdr {
        return Ok(None);
    }
    if !can_process {
        return Err(NO_CRATE);
    }
    if family != "hevc" {
        return Err(NOT_HEVC);
    }
    let dovi_kept = dovi.is_some_and(DoviRoute::keeps_dovi);
    if dovi_kept && !matches!(dovi, Some(DoviRoute::Native { .. })) {
        return Err(DOVI_KEPT);
    }
    if encoder == "libx265" {
        return if !caps.x265_hdr10plus {
            Err(X265_NO_SUPPORT)
        } else if profile == "N/A" {
            Err(X265_PROFILE)
        } else {
            Ok(Some(Hdr10PlusRoute::X265Native))
        };
    }
    if dovi_kept {
        return Err(DOVI_KEPT);
    }
    if encoder == "hevc_vaapi"
        && caps
            .ffmpeg_major
            .is_some_and(|m| m < VAAPI_MIN_FFMPEG_MAJOR)
    {
        return Err(VAAPI_OLD_FFMPEG);
    }
    Ok(Some(Hdr10PlusRoute::Injector))
}

/// What the software fallback needs to decide HDR10+ again, once the
/// hardware encoder has failed. Passed only when the dynamic metadata was
/// extracted for the hardware encode.
pub struct FallbackHdr10Plus<'a> {
    pub can_process: bool,
    pub family: &'a str,
    pub dovi: Option<DoviRoute>,
    pub caps: &'a FfmpegCaps,
    pub profile: &'a str,
}

/// The software fallback encode: its video arguments and whether the
/// extracted HDR10+ metadata is still to be injected afterwards.
#[derive(Debug, PartialEq, Eq)]
pub struct FallbackPlan {
    pub video_args: Vec<String>,
    /// Inject the extracted metadata after the encode (injector route only).
    pub keep_injection: bool,
    /// Log lines, in order.
    pub notes: Vec<String>,
}

/// Video arguments for the software fallback encode, with HDR10+ decided
/// again for `sw_encoder`.
///
/// The route chosen for the failed hardware encoder does not carry over: the
/// injector needs a stream without B-frames, which the software arguments do
/// not produce. libx265 codes the metadata itself (`dhdr10-info`), so the
/// extracted metadata goes to `write_json`, which returns the JSON path; no
/// injection follows. Where the metadata cannot be kept, the output is
/// static HDR10 and no injection is attempted. `hdr10` is `None` when no
/// metadata was extracted.
pub fn fallback_sw_args(
    decision: &EncodeDecision,
    sw_encoder: &str,
    qi: &str,
    qp: &str,
    hdr10: Option<&FallbackHdr10Plus>,
    write_json: &mut dyn FnMut() -> Result<String, String>,
) -> FallbackPlan {
    let mut video_args: Vec<String> = vec!["-c:v".into(), sw_encoder.to_string()];
    match decision {
        EncodeDecision::Vbr {
            target_bps,
            peak_bps,
        } => video_args.extend(vbr_flags(
            sw_encoder,
            &target_bps.to_string(),
            &peak_bps.to_string(),
        )),
        _ => video_args.extend(cqp_flags(sw_encoder, qi, qp)),
    }
    let mut notes = Vec::new();
    let mut keep_injection = false;
    if let Some(h) = hdr10 {
        match hdr10plus_route(
            true,
            h.can_process,
            true,
            sw_encoder,
            h.family,
            h.dovi,
            h.caps,
            h.profile,
        ) {
            Ok(Some(Hdr10PlusRoute::X265Native)) => match write_json() {
                Ok(path) => {
                    let param = format!("dhdr10-info={}", x265_param_path_escape(&path));
                    notes.push(format!("  HDR10+: coded by {sw_encoder} ({param})"));
                    add_x265_param(&mut video_args, &param);
                }
                Err(e) => notes.push(format!("  HDR10+: {e} - output has static HDR10 only")),
            },
            Ok(Some(Hdr10PlusRoute::Injector)) => {
                notes.push(
                    "  HDR10+: B-frames off so the metadata can be injected after the encode"
                        .to_string(),
                );
                video_args.extend(["-bf".to_string(), "0".to_string()]);
                keep_injection = true;
            }
            Ok(None) => {}
            Err(reason) => notes.push(format!("  HDR10+: Static HDR10 only ({reason})")),
        }
    }
    FallbackPlan {
        video_args,
        keep_injection,
        notes,
    }
}

/// Codec family of an encoder name ("hevc", "h264" or "av1").
pub fn encoder_family(encoder: &str) -> &'static str {
    match encoder {
        "libx265" => "hevc",
        "libx264" => "h264",
        "libsvtav1" => "av1",
        e if e.starts_with("hevc_") => "hevc",
        e if e.starts_with("h264_") => "h264",
        e if e.starts_with("av1_") => "av1",
        _ => "",
    }
}

/// Why an HDR10+ source loses its dynamic metadata when encoded with
/// `encoder`, if it does. Used by the GUI preflight and the CLI dry run.
pub fn hdr10plus_preflight_issue(
    probe: &crate::probe::ProbeResult,
    encoder: &str,
) -> Option<&'static str> {
    let caps = crate::dovi_tools::capabilities();
    let ff_caps = ffmpeg_caps();
    let family = encoder_family(encoder);
    let dovi = probe.dovi_profile.map(|p| {
        dovi_route_for_encoder(
            p,
            encoder,
            family,
            &[],
            &ff_caps,
            caps.can_process_dovi && caps.can_package_dovi_mp4,
            true,
            vaapi_dv_env_allowed(),
        )
    });
    let (encoder, family) = match dovi {
        Some(DoviRoute::Native { encoder, family }) => (encoder, family),
        _ => (encoder, family),
    };
    hdr10plus_route(
        probe.has_hdr10plus,
        caps.can_process_hdr10plus,
        true,
        encoder,
        family,
        dovi,
        &ff_caps,
        &probe.hdr10plus_profile,
    )
    .err()
}

// ── Dolby Vision routing ────────────────────────────────────────

/// How a Dolby Vision source keeps (or loses) its DV metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DoviRoute {
    /// ffmpeg codes the RPUs itself (`-dolbyvision 1`). Forces this encoder
    /// and codec family; RPUs stay attached to their frames.
    Native {
        encoder: &'static str,
        family: &'static str,
    },
    /// Rust extract/inject plus MP4Box packaging (profile 7, or an ffmpeg
    /// without native DV coding). HEVC into MP4 only.
    RpuPipeline,
    /// DV is dropped: the HDR10 base layer is kept, or tonemapped when HDR
    /// preservation is off.
    Hdr10,
}

impl DoviRoute {
    /// Whether the output keeps Dolby Vision.
    pub fn keeps_dovi(self) -> bool {
        !matches!(self, DoviRoute::Hdr10)
    }
}

/// The single routing decision for a Dolby Vision source.
///
/// - HDR preservation off: `Hdr10`.
/// - A hardware encoder (chosen by the user or by auto) with profile 5, 7
///   or 8 and the RPU pipeline available: `RpuPipeline`, which keeps the
///   hardware encoder and forces MP4, as before native coding existed.
/// - Profile 5, 8 or 10 with native libx265 DV: `Native{libx265}`. Covers
///   software encoders, and hardware encoders when the RPU pipeline cannot
///   run (no MP4Box); an H.264 target cannot carry DV, so it switches too.
/// - Profile 8 or 10 with an AV1 target and native libsvtav1 DV:
///   `Native{libsvtav1}`. Profile 5 has no AV1 form (libsvtav1 rejects it),
///   so it stays HEVC.
/// - Profile 7 (dual layer; native coding errors on FEL and guesses MEL
///   as profile 8), or no native capability: `RpuPipeline` for HEVC-based
///   profiles when the Rust pipeline and MP4Box are available, else `Hdr10`.
pub fn dovi_route(
    profile: u8,
    target_family: &str,
    hardware_encoder: bool,
    caps: &FfmpegCaps,
    rpu_pipeline_ok: bool,
    preserve_hdr: bool,
) -> DoviRoute {
    if !preserve_hdr {
        return DoviRoute::Hdr10;
    }
    if hardware_encoder && rpu_pipeline_ok && matches!(profile, 5 | 7 | 8) {
        return DoviRoute::RpuPipeline;
    }
    if matches!(profile, 8 | 10) && target_family == "av1" && caps.dovi_svtav1 {
        return DoviRoute::Native {
            encoder: "libsvtav1",
            family: "av1",
        };
    }
    if matches!(profile, 5 | 8 | 10) && caps.dovi_x265 {
        return DoviRoute::Native {
            encoder: "libx265",
            family: "hevc",
        };
    }
    if matches!(profile, 5 | 7 | 8) && rpu_pipeline_ok {
        return DoviRoute::RpuPipeline;
    }
    DoviRoute::Hdr10
}

/// Whether `HISTV_ALLOW_VAAPI_DV=1` keeps a VAAPI encoder for Dolby Vision
/// (testing the RPU pipeline on VAAPI).
fn vaapi_dv_env_allowed() -> bool {
    std::env::var("HISTV_ALLOW_VAAPI_DV")
        .map(|v| v == "1")
        .unwrap_or(false)
}

/// Whether a VAAPI HEVC encoder is replaced by libx265 for Dolby Vision.
/// AMD VAAPI 10-bit HEVC produced visible artifacts, so Linux swaps it out
/// unless `allow_vaapi_dv` is set.
fn vaapi_dv_swap(encoder: &str, allow_vaapi_dv: bool) -> bool {
    cfg!(target_os = "linux") && is_vaapi_encoder(encoder) && !allow_vaapi_dv
}

/// `dovi_route` for the encoder `encode_single_file` will actually run.
///
/// The RPU pipeline needs HEVC, and a VAAPI HEVC encoder is swapped for
/// libx265 (`vaapi_dv_swap`). libx265 then codes DV natively where it can,
/// which keeps HDR10+ and the configured container, so the route is decided
/// again for libx265 rather than left as the hardware encoder's
/// `RpuPipeline`. Profile 7 and an ffmpeg without native DV stay on the
/// RPU pipeline.
#[allow(clippy::too_many_arguments)]
pub fn dovi_route_for_encoder(
    profile: u8,
    encoder: &str,
    family: &str,
    detected_encoders: &[EncoderInfo],
    caps: &FfmpegCaps,
    rpu_pipeline_ok: bool,
    preserve_hdr: bool,
    allow_vaapi_dv: bool,
) -> DoviRoute {
    let route = dovi_route(
        profile,
        family,
        is_hardware_encoder_name(encoder),
        caps,
        rpu_pipeline_ok,
        preserve_hdr,
    );
    if route != DoviRoute::RpuPipeline {
        return route;
    }
    let hevc_encoder = if family == "hevc" {
        encoder.to_string()
    } else {
        pick_encoder("hevc", detected_encoders)
    };
    if vaapi_dv_swap(&hevc_encoder, allow_vaapi_dv) {
        return dovi_route(profile, "hevc", false, caps, rpu_pipeline_ok, preserve_hdr);
    }
    route
}

/// Whether a finished ffmpeg output needs its Dolby Vision sample entry
/// restored (`mp4_entry`): a profile 5 stream copy into MP4. ffmpeg cannot
/// write `dvhe`, so the copy of a `dvhe` file comes out as `hev1`. Profile 8
/// and non-DV copies are left as ffmpeg wrote them.
fn copy_needs_dovi_entry(decision: &EncodeDecision, dovi_p5: bool) -> bool {
    dovi_p5 && matches!(decision, EncodeDecision::Copy)
}

/// Whether an encoder name is a hardware encoder (software ones are the
/// `lib*` wrappers: libx265, libx264, libsvtav1).
pub fn is_hardware_encoder_name(name: &str) -> bool {
    !name.is_empty() && !name.starts_with("lib")
}

/// Whether the Rust DV pipeline can run: the `dovi` feature is compiled in
/// and MP4Box supports `:dvp=`.
fn rpu_pipeline_available() -> bool {
    let c = crate::dovi_tools::capabilities();
    c.can_process_dovi && c.can_package_dovi_mp4
}

/// Input bitstream filters for a native Dolby Vision encode of an HEVC
/// source (`dovi_input_bsf`).
///
/// ffmpeg's hevc decoder reads an RPU only when it is the last NAL unit of
/// its packet (`libavcodec/hevc/hevcdec.c:3753` at n8.1.3). Some Dolby
/// Vision files store the next keyframe's VPS, SPS and PPS after the RPU of
/// the sample before it; that frame's RPU is then skipped and the frame is
/// given the RPU of the frame decoded before it. `extract_extradata=remove=1`
/// takes the parameter sets out of each packet and hands them to the decoder
/// as new extradata, so the RPU is last again; it needs Annex B packets,
/// hence `hevc_mp4toannexb` (a no-op on Annex B input).
pub const DOVI_INPUT_BSF: &str = "hevc_mp4toannexb,extract_extradata=remove=1";

/// The input filter for this job's video stream, if it needs one: a native
/// Dolby Vision encode (not a stream copy) of an HEVC source. Profile 10
/// sources are AV1 and are left alone.
pub fn dovi_input_bsf(
    native_route: bool,
    stream_copy: bool,
    source_video_codec: &str,
) -> Option<&'static str> {
    (native_route && !stream_copy && source_video_codec == "hevc").then_some(DOVI_INPUT_BSF)
}

/// Extra video arguments for native DV coding, appended after the
/// rate-control flags in `existing`.
///
/// - Always `-dolbyvision 1`.
/// - libx265 refuses DV without VBV ("Dolby Vision requires VBV settings to
///   enable HRD"), so `-maxrate`/`-bufsize` are added when missing. An
///   existing `-maxrate` (VBR, precision mode) wins and only `-bufsize` is
///   added from it. Otherwise the cap is `threshold x peak multiplier`, or
///   twice the source bitrate when there is no threshold. Under CQP x265
///   ignores VBV with a warning; CRF and VBR honour it.
pub fn dovi_native_args(
    encoder: &str,
    existing: &[String],
    threshold_bps: u64,
    peak_mult: f64,
    source_bps: u64,
) -> Vec<String> {
    let mut args: Vec<String> = vec!["-dolbyvision".into(), "1".into()];
    if encoder != "libx265" {
        return args;
    }
    let value_of = |flag: &str| {
        existing
            .windows(2)
            .find(|w| w[0] == flag)
            .map(|w| w[1].clone())
    };
    let maxrate = value_of("-maxrate");
    let m: u64 = match &maxrate {
        Some(v) => v.parse().unwrap_or(0),
        None if threshold_bps > 0 => (threshold_bps as f64 * peak_mult.max(1.0)) as u64,
        None => source_bps.saturating_mul(2),
    };
    // A last resort for a source with no known bitrate and no threshold.
    let m = if m == 0 { 50_000_000 } else { m };
    if maxrate.is_none() {
        args.extend(["-maxrate".into(), m.to_string()]);
    }
    if value_of("-bufsize").is_none() {
        args.extend(["-bufsize".into(), m.saturating_mul(2).to_string()]);
    }
    args
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

    // Classify each video encoder: software encoders are added immediately,
    // hardware candidates are collected for concurrent verification.
    struct HwCandidate {
        name: &'static str,
        family: &'static str,
    }
    let mut hw_candidates: Vec<HwCandidate> = Vec::new();

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

        sink.log(&format!("[detect] {enc_name} - queued for testing..."));
        hw_candidates.push(HwCandidate {
            name: enc_name,
            family,
        });
    }

    // Test hardware encoder candidates sequentially. Concurrent test
    // encodes can fail on GPUs that don't support multiple simultaneous
    // encoding sessions (e.g. AMD AMF), causing all candidates to be
    // rejected even though they work individually.
    for candidate in &hw_candidates {
        let ok = test_encode(candidate.name).await;
        if ok {
            sink.log(&format!("[detect] {} (HW) - works", candidate.name));
            video_encoders.push(EncoderInfo {
                name: candidate.name.to_string(),
                codec_family: candidate.family.to_string(),
                is_hardware: true,
            });
        } else {
            sink.log(&format!(
                "[detect] {} (HW) - not available (test encode failed)",
                candidate.name
            ));
        }
    }

    let caps = probe_ffmpeg_caps(
        available.contains("libx265"),
        available.contains("libsvtav1"),
    )
    .await;
    sink.log(&format!(
        "[detect] Native Dolby Vision: libx265 {}, libsvtav1 {}; x265 HDR10+ (dhdr10-info) {}; hevc_metadata width/height {}",
        if caps.dovi_x265 { "yes" } else { "no" },
        if caps.dovi_svtav1 { "yes" } else { "no" },
        if caps.x265_hdr10plus { "yes" } else { "no" },
        if caps.hevc_metadata_size { "yes" } else { "no" },
    ));
    if caps.no_fps_mode {
        sink.log("[detect] ffmpeg predates -fps_mode - using -vsync passthrough");
    }
    if let Ok(mut c) = FFMPEG_CAPS.write() {
        *c = caps;
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

    #[cfg(target_os = "linux")]
    let render_node = find_dri_render_node_for(encoder_name);
    #[cfg(not(target_os = "linux"))]
    let render_node: Option<String> = None;
    let Some(args) = test_encode_args(
        encoder_name,
        render_node.as_deref(),
        TargetOs::current(),
        &tmp_str,
    ) else {
        return false;
    };

    let mut cmd = ffbin::ffmpeg_command();
    cmd.args(&args)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped());
    output_with_timeout(cmd, DETECT_TIMEOUT)
        .await
        .is_some_and(|o| o.status.success())
}

#[cfg(target_os = "linux")]
/// Find the lowest-numbered DRI render node in `dir` (e.g. `renderD128`).
/// Returns the full path as a string, or `None` if no render node is found
/// or the directory cannot be read.
///
/// The lowest number is taken rather than the first `read_dir` entry,
/// whose order is arbitrary: on a machine with a discrete GPU at
/// renderD128 and an iGPU at renderD129, directory order picked the iGPU,
/// which has no AV1 VAAPI encode and no Vulkan encode queue.
///
/// Split from the public wrapper so tests can pass a temporary
/// directory instead of requiring `/dev/dri` to exist.
#[cfg(test)]
fn find_dri_render_node_in(dir: &std::path::Path) -> Option<String> {
    find_dri_render_node_for_vendor(dir, std::path::Path::new("/sys/class/drm"), None)
}

/// PCI vendor id of Intel GPUs, the only ones QSV runs on.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const PCI_VENDOR_INTEL: u16 = 0x8086;

/// Render node from `dir`, preferring GPUs from `vendor` (read from
/// `<sysfs>/<node>/device/vendor`), else the lowest-numbered node.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn find_dri_render_node_for_vendor(
    dir: &std::path::Path,
    sysfs: &std::path::Path,
    vendor: Option<u16>,
) -> Option<String> {
    let nodes: Vec<(u32, String, Option<u16>)> = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().to_string_lossy().into_owned();
            let num: u32 = name.strip_prefix("renderD")?.parse().ok()?;
            let vendor_id = std::fs::read_to_string(sysfs.join(&name).join("device/vendor"))
                .ok()
                .and_then(|v| u16::from_str_radix(v.trim().trim_start_matches("0x"), 16).ok());
            Some((num, name, vendor_id))
        })
        .collect();
    select_render_node(&nodes, vendor).map(|name| format!("{}/{name}", dir.display()))
}

/// Pick a render node from `(number, name, vendor id)` entries: the lowest
/// numbered node of `vendor` when one exists, else the lowest numbered node.
/// QSV asks for Intel: on an AMD card at renderD128 plus an Intel iGPU at
/// renderD129, the lowest node alone would hand QSV the AMD card.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn select_render_node(nodes: &[(u32, String, Option<u16>)], vendor: Option<u16>) -> Option<&str> {
    let lowest = |want: Option<u16>| {
        nodes
            .iter()
            .filter(|(_, _, v)| want.is_none() || *v == want)
            .min_by_key(|(n, _, _)| *n)
            .map(|(_, name, _)| name.as_str())
    };
    vendor
        .and_then(|v| lowest(Some(v)))
        .or_else(|| lowest(None))
}

/// Whether `encoder` may decode on the VAAPI device. `hevc_vaapi` without
/// hevc_metadata `width`/`height` needs software frames for the size fix
/// (`hevc_vaapi_size_fix`).
pub fn vaapi_device_decode_ok(encoder: &str, bsf_has_size: bool) -> bool {
    encoder != "hevc_vaapi" || bsf_has_size
}

/// How an `hevc_vaapi` encode is made to decode at the picture size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HevcVaapiSizeFix {
    /// Filter to run on software frames before the hardware upload.
    pub pad: Option<String>,
    /// `-bsf:v` value.
    pub bsf: String,
}

/// Mesa radeonsi (25.2.8, RX 7900 XTX) codes the picture at its own
/// alignment (width to 64, height to 16) and writes that size into the
/// in-band SPS while keeping ffmpeg's crop offsets, so decoders show the
/// padding: 1920x1080 decodes as 1920x1088, 640x360 as 640x368, 1000x562 as
/// 1024x570. ffmpeg's out-of-band SPS (hvcC) has the right size, so any
/// fixed crop is right for one SPS and wrong for the other.
///
/// - `bsf_has_size` (hevc_metadata has `width`/`height`; ffmpeg 6.1 does
///   not): the BSF computes each SPS's crop from its own coded size
///   (libavcodec/bsf/h265_metadata.c:203-258).
/// - Otherwise pad the software frames to 64x64 multiples before the
///   upload, so both SPSs carry the same coded size, then crop the padding.
///   The caller keeps decode in software (`vaapi_device_decode_ok`), as
///   there is no pad filter for device frames in ffmpeg 6.1.
///
/// Both decode at the picture size with the right container size (measured
/// on ffmpeg 6.1.1 and n8.1.3). `width` and `height` are the encoded
/// picture size; odd sizes cannot be expressed in 4:2:0 crop units.
pub fn hevc_vaapi_size_fix(
    encoder: &str,
    width: u32,
    height: u32,
    bsf_has_size: bool,
) -> Option<HevcVaapiSizeFix> {
    if encoder != "hevc_vaapi"
        || width == 0
        || height == 0
        || !width.is_multiple_of(2)
        || !height.is_multiple_of(2)
    {
        return None;
    }
    if bsf_has_size {
        return Some(HevcVaapiSizeFix {
            pad: None,
            bsf: format!("hevc_metadata=width={width}:height={height}"),
        });
    }
    let (pw, ph) = (width.next_multiple_of(64), height.next_multiple_of(64));
    if (pw, ph) == (width, height) {
        return None;
    }
    Some(HevcVaapiSizeFix {
        pad: Some(format!("pad={pw}:{ph}")),
        bsf: format!(
            "hevc_metadata=crop_right={}:crop_bottom={}",
            pw - width,
            ph - height
        ),
    })
}

/// Render node for an encode on this platform (Linux only).
pub(crate) fn render_node_for(encoder: &str) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        find_dri_render_node_for(encoder)
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = encoder;
        None
    }
}

/// Render node for `encoder`: an Intel GPU for QSV, the lowest node for
/// everything else (VAAPI, Vulkan).
#[cfg(target_os = "linux")]
fn find_dri_render_node_for(encoder: &str) -> Option<String> {
    let vendor = is_qsv_encoder(encoder).then_some(PCI_VENDOR_INTEL);
    find_dri_render_node_for_vendor(
        std::path::Path::new("/dev/dri"),
        std::path::Path::new("/sys/class/drm"),
        vendor,
    )
}

/// Find the first available DRI render node (e.g. /dev/dri/renderD128).
/// Returns None if no render node is found.
#[cfg(all(test, target_os = "linux"))]
fn find_dri_render_node() -> Option<String> {
    find_dri_render_node_in(std::path::Path::new("/dev/dri"))
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
    /// Compatibility: copy if below cap and already AAC;
    /// otherwise re-encode to AAC at cap.
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
/// 2. **MP4 override** - when `force_mp4` is true (the Dolby Vision RPU pipeline), force MP4.
/// 3. **Explicit** - when "mkv" or "mp4", use that directly.
///
/// Deterministic, no I/O - safe to call from display code.
pub fn resolve_container(
    source_path: &str,
    container_setting: &str,
    force_mp4: bool,
) -> &'static str {
    if force_mp4 {
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

/// Resolve the final output container extension for a file, applying the exact
/// rules `encode_single_file` uses: the Dolby Vision RPU pipeline (MP4Box)
/// and compatibility mode both force MP4, otherwise the container is derived
/// from the configured setting / source extension. Native DV coding keeps the
/// configured container. Centralised so the encode path and every
/// remote-staging back-copy path resolve the extension identically and cannot
/// drift apart (the back-copy must look for the file the encode actually wrote).
pub fn resolve_output_ext(
    source_path: &str,
    source_video_codec: &str,
    dovi_profile: Option<u8>,
    settings: &BatchSettings,
    preserve_hdr: bool,
    detected_encoders: &[EncoderInfo],
) -> &'static str {
    resolve_output_ext_with(
        source_path,
        source_video_codec,
        dovi_profile,
        settings,
        preserve_hdr,
        detected_encoders,
        &ffmpeg_caps(),
        rpu_pipeline_available(),
        vaapi_dv_env_allowed(),
    )
}

/// `resolve_output_ext` with the capabilities passed in.
#[allow(clippy::too_many_arguments)]
fn resolve_output_ext_with(
    source_path: &str,
    source_video_codec: &str,
    dovi_profile: Option<u8>,
    settings: &BatchSettings,
    preserve_hdr: bool,
    detected_encoders: &[EncoderInfo],
    caps: &FfmpegCaps,
    rpu_pipeline_ok: bool,
    allow_vaapi_dv: bool,
) -> &'static str {
    if settings.compatibility_mode {
        return "mp4";
    }
    if let Some(profile) = dovi_profile {
        let ext = std::path::Path::new(source_path)
            .extension()
            .map(|e| e.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let r = resolve_file_settings(source_video_codec, &ext, settings, detected_encoders);
        if dovi_route_for_encoder(
            profile,
            &r.encoder_name,
            &r.codec_family,
            detected_encoders,
            caps,
            rpu_pipeline_ok,
            preserve_hdr,
            allow_vaapi_dv,
        ) == DoviRoute::RpuPipeline
        {
            return "mp4";
        }
    }
    resolve_container(source_path, &settings.output_container, false)
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
    // 1. Codec family — explicit --codec override takes priority, then
    //    compatibility mode, then per-file detection from the source codec.
    let codec_family = match settings.codec_family.as_str() {
        "h264" => "h264",
        "hevc" => "hevc",
        _ if settings.compatibility_mode => "h264",
        _ if source_video_codec == "h264" => "h264",
        _ if source_video_codec == "hevc" || source_video_codec == "h265" => "hevc",
        _ if settings.preserve_av1 && source_video_codec == "av1" => "av1",
        _ => "hevc", // default conversion target
    };

    // 2. Encoder — explicit --encoder override takes priority, then
    //    precision mode forces software, then auto-detect from detected list.
    let encoder_name = match settings.video_encoder.as_str() {
        "" | "auto" => {
            if settings.precision_mode {
                software_fallback(codec_family).to_string()
            } else {
                pick_encoder(codec_family, detected_encoders)
            }
        }
        forced => forced.to_string(),
    };

    // 3. Container - use the shared resolver
    let container_ext = if settings.compatibility_mode {
        "mp4".to_string()
    } else {
        // source_container is just the extension; build a fake path for resolve_container
        let fake_path = format!("file.{}", source_container);
        resolve_container(&fake_path, &settings.output_container, false).to_string()
    };

    // 4. Audio strategy — compat mode always forces AAC (overrides --audio),
    //    then explicit --audio takes priority, then default copy-capped.
    let cap = settings.audio_cap;
    let audio_strategy = if settings.compatibility_mode {
        AudioStrategy::CompatCapped { cap_kbps: cap }
    } else {
        match settings.audio_encoder.as_str() {
            "copy" => AudioStrategy::CopyCapped { cap_kbps: cap },
            "aac" => AudioStrategy::CompatCapped { cap_kbps: cap },
            // ac3, eac3, or any other codec: use CopyCapped so the
            // fallback codec is resolved per-stream inside
            // build_audio_args_from_probe.
            _ => AudioStrategy::CopyCapped { cap_kbps: cap },
        }
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

/// Why a Dolby Vision source is written as HDR10 (worded for
/// "HDR10 fallback (..)") or as SDR ("SDR conversion (..)"). Shared by the GUI pre-flight, the CLI dry run and
/// the encode log.
pub mod dovi_loss {
    pub const NO_MP4BOX: &str = "MP4Box not found";
    pub const NO_NATIVE: &str = "this ffmpeg has no native Dolby Vision coding";
    /// Profile 5 has no HDR10 base layer, so plain HDR10 output probably has wrong
    /// colours (UNVERIFIED: inferred from `dovi_pipeline::needs_profile_conversion`, not run).
    pub const P5_NO_MP4BOX: &str = "profile 5 has no HDR10 layer, so colours are likely wrong; install MP4Box or use HISTV's ffmpeg";
    pub const P5_NO_NATIVE: &str =
        "profile 5 has no HDR10 layer, so colours are likely wrong; use HISTV's ffmpeg";
    /// Converted to SDR (Compatibility mode, or Preserve HDR off): there is
    /// no base layer to tonemap, so the picture probably keeps the raw
    /// base layer's green and purple cast (UNVERIFIED: inferred from one
    /// output file, not a reference render).
    pub const P5_SDR: &str = "profile 5 has no HDR10 layer, so SDR colours will probably be wrong";
}

/// Why a Dolby Vision source will lose DV, if it will: the reason (an
/// `dovi_loss` string) and the tool whose installation would fix it. `None`
/// when DV is kept. Assumes HDR preservation is on and an HEVC target (the
/// route's outcome does not depend on the family when libx265 has native DV).
pub fn dovi_preflight_issue(
    profile: u8,
    caps: &FfmpegCaps,
    rust_dovi: bool,
    mp4box_dvp: bool,
) -> Option<(&'static str, Option<&'static str>)> {
    use dovi_loss::*;
    let route = |rpu_ok| dovi_route(profile, "hevc", false, caps, rpu_ok, true);
    if route(rust_dovi && mp4box_dvp).keeps_dovi() {
        return None;
    }
    let p5 = profile == 5;
    if rust_dovi && route(true) == DoviRoute::RpuPipeline {
        Some((if p5 { P5_NO_MP4BOX } else { NO_MP4BOX }, Some("MP4Box")))
    } else {
        Some((if p5 { P5_NO_NATIVE } else { NO_NATIVE }, None))
    }
}

/// The encode-time log line for a Dolby Vision source that `dovi_route`
/// sends to plain HDR10. Profile 5 gets the reason and the way out; the rest
/// keep the generic line.
fn dovi_hdr10_log_line(
    profile: u8,
    caps: &FfmpegCaps,
    rust_dovi: bool,
    mp4box_dvp: bool,
) -> String {
    match dovi_preflight_issue(profile, caps, rust_dovi, mp4box_dvp) {
        Some((reason, _)) if profile == 5 => {
            format!("  Dolby Vision cannot be kept - output is HDR10 ({reason})")
        }
        _ => "  Dolby Vision cannot be kept with this ffmpeg and tool set - output is HDR10"
            .to_string(),
    }
}

/// Whether the output keeps HDR, as the user's settings decide it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRange {
    /// Compatibility mode: H.264/MP4 SDR.
    pub compat: bool,
    /// HDR preservation is on (the "Preserve HDR" setting, no `--no-hdr`).
    pub preserve_hdr: bool,
}

impl OutputRange {
    /// Whether HDR sources are converted to SDR.
    pub fn is_sdr(self) -> bool {
        self.compat || !self.preserve_hdr
    }
}

/// Why SDR output of a Dolby Vision source probably has wrong colours, with
/// what to change; `None` when it does not apply. Only profile 5 has no HDR10
/// base layer to tonemap (profiles 7 and 8 keep one, which converts fine).
/// `cli` names the command-line flags instead of the GUI settings.
pub fn dovi_sdr_issue(profile: u8, out: OutputRange, cli: bool) -> Option<String> {
    if profile != 5 || !out.is_sdr() {
        return None;
    }
    let fix = match (out.compat, out.preserve_hdr, cli) {
        (true, true, false) => "turn off Compatibility mode",
        (false, false, false) => "turn on Preserve HDR",
        (_, _, false) => "turn off Compatibility mode and turn on Preserve HDR",
        (true, true, true) => "drop --compat",
        (false, false, true) => "drop --no-hdr",
        (_, _, true) => "drop --compat and --no-hdr",
    };
    Some(format!("{}; {fix}", dovi_loss::P5_SDR))
}

/// The encode-time log line for a profile 5 source converted to SDR. A
/// stream copy converts nothing and keeps Dolby Vision.
fn dovi_sdr_log_line(profile: u8, out: OutputRange, is_copy: bool) -> Option<String> {
    if is_copy {
        return None;
    }
    dovi_sdr_issue(profile, out, false)
        .map(|issue| format!("  Dolby Vision cannot be kept - output is SDR ({issue})"))
}

/// Scan the queue and report files that will be degraded due to missing tools.
pub fn preflight_scan(
    queue: &[QueueItem],
    detected_encoders: &[EncoderInfo],
    out: OutputRange,
) -> Vec<PreflightWarning> {
    let caps = crate::dovi_tools::capabilities();
    let ff_caps = ffmpeg_caps();
    let mut warnings = Vec::new();

    for item in queue {
        if item.status != QueueItemStatus::Pending {
            continue;
        }

        if let Some(profile) = item.probe.dovi_profile {
            // Converted to SDR, profile 5 has no HDR10 layer to tonemap; the
            // HDR10 fallback does not apply to it then.
            if let Some(reason) = dovi_sdr_issue(profile, out, false) {
                warnings.push(PreflightWarning {
                    file_name: item.file_name.clone(),
                    source_type: format!("Dolby Vision Profile {}", profile),
                    best_possible: "Full DV preservation".to_string(),
                    actual_outcome: format!("SDR conversion ({reason})"),
                    missing_tool: None,
                });
            } else if let Some((reason, tool)) = dovi_preflight_issue(
                profile,
                &ff_caps,
                caps.can_process_dovi,
                caps.can_package_dovi_mp4,
            ) {
                warnings.push(PreflightWarning {
                    file_name: item.file_name.clone(),
                    source_type: format!("Dolby Vision Profile {}", profile),
                    best_possible: "Full DV preservation".to_string(),
                    actual_outcome: format!("HDR10 fallback ({reason})"),
                    missing_tool: tool.map(str::to_string),
                });
            }
        }

        // The batch settings are not known here; assume the default HEVC
        // target with the encoder auto mode would pick.
        if let Some(reason) =
            hdr10plus_preflight_issue(&item.probe, &pick_encoder("hevc", detected_encoders))
        {
            warnings.push(PreflightWarning {
                file_name: item.file_name.clone(),
                source_type: "HDR10+".to_string(),
                best_possible: "Full dynamic metadata preservation".to_string(),
                actual_outcome: format!("Static HDR10 only ({reason})"),
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
            codec_family: "auto".to_string(),
            audio_encoder: "auto".to_string(),
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
    audio_strategy: &AudioStrategy,
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

            match dovi_pipeline::inject_and_package(temp_output_file, &dv_staging, rpus, sink).await
            {
                Ok(result) if result.success => {
                    sink.log(&format!("  {}", result.message));
                    write_log(log_writer, &format!("  {}", result.message));
                    // Swap: remove the non-DV encode, put the DV MP4 in its place
                    let _ = std::fs::remove_file(temp_output_file);
                    if let Err(e) = std::fs::rename(&dv_staging, temp_output_file) {
                        sink.log(&format!("  WARNING: Could not rename DV output: {e}"));
                        // Try copy as fallback (cross-filesystem)
                        if let Err(copy_err) = std::fs::copy(&dv_staging, temp_output_file) {
                            sink.log(&format!(
                                "  WARNING: DV output copy also failed: {copy_err}"
                            ));
                        }
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
    // Skip for Copy decisions: the output is just a remux of the source, so
    // a "remux source instead" fallback would produce the same file again.
    let is_copy = matches!(decision, EncodeDecision::Copy);
    if temp_output_file.exists() {
        let src_size = std::fs::metadata(item_full_path)
            .map(|m| m.len())
            .unwrap_or(0);
        let dst_size = std::fs::metadata(temp_output_file)
            .map(|m| m.len())
            .unwrap_or(0);

        // Container overhead deadband: MP4 moov atoms store per-frame index
        // entries (~16 bytes each for stsz + stco + ctts) plus audio sample
        // tables (~500 bytes/sec per stream). Outputs that exceed the source
        // by less than this + 20% margin are within expected container
        // overhead and should not trigger the remux fallback.
        let fps = queue[idx].probe.video_fps.max(1.0);
        let num_audio = queue[idx].probe.audio_streams.len() as f64;
        let video_overhead = item_duration_secs * fps * 16.0;
        let audio_overhead = item_duration_secs * 500.0 * num_audio;
        let deadband = ((video_overhead + audio_overhead + 10_240.0) * 1.2) as u64;

        if dst_size > src_size + deadband && src_size > 0 && !is_image_source && !is_copy {
            sink.log(&format!(
                "  WARNING: Output ({:.1}MB) larger than source ({:.1}MB) - remuxing source instead",
                dst_size as f64 / 1_000_000.0, src_size as f64 / 1_000_000.0,
            ));
            write_log(log_writer, "  Output larger than source - remuxing");
            let _ = std::fs::remove_file(temp_output_file);

            // Remux with audio-strategy-aware stream mapping: use the same
            // audio strategy as the main encode path so compat mode re-encodes
            // incompatible audio (e.g. DTS/TrueHD → AAC for MP4).
            let (audio_map, audio_codec) = build_audio_args_from_probe(
                &queue[idx].probe.audio_streams,
                audio_strategy,
                ext,
                sink,
            );

            let mut remux_args: Vec<String> = vec![
                "-y".into(),
                "-i".into(),
                item_full_path.into(),
                "-map".into(),
                "0:v".into(),
            ];
            remux_args.extend(audio_map);

            if ext == "mp4" {
                // MP4: convert text subs to mov_text; ffmpeg silently
                // skips bitmap subs (PGS, DVB) that can't be converted.
                remux_args.extend([
                    "-map".into(),
                    "0:s?".into(),
                    "-c:v".into(),
                    "copy".into(),
                    "-c:s".into(),
                    "mov_text".into(),
                ]);
            } else {
                remux_args.extend([
                    "-map".into(),
                    "0:s?".into(),
                    "-c:v".into(),
                    "copy".into(),
                    "-c:s".into(),
                    "copy".into(),
                ]);
            }
            remux_args.extend(audio_codec);
            if ext == "mp4" && queue[idx].probe.dovi_profile.is_some() {
                // Keep the source's DV configuration record (movenc drops it otherwise).
                remux_args.extend(["-strict".into(), "unofficial".into()]);
            }
            remux_args.push(output_str.into());

            let remux_refs: Vec<&str> = remux_args.iter().map(|s| s.as_str()).collect();
            let remux_output = ffbin::ffmpeg_command()
                .args(&remux_refs)
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

        // Replace mode: safely swap source with encoded output.
        // Rename source to .bak first so we can recover if the final
        // rename fails (avoids TOCTOU data loss).
        if is_replace_mode && temp_output_file.exists() {
            let backup = std::path::PathBuf::from(format!("{}.histv-bak", item_full_path));
            let source_backed_up = match std::fs::rename(item_full_path, &backup) {
                Ok(_) => true,
                Err(e) => {
                    let warn = format!("  WARNING: Could not back up source for replacement: {e}");
                    sink.log(&warn);
                    write_log(log_writer, &warn);
                    false
                }
            };
            // Rename temp to final
            if temp_output_file != output_file {
                if let Err(e) = std::fs::rename(temp_output_file, output_file) {
                    let warn = format!("  WARNING: Could not rename temp to final path: {e}");
                    sink.log(&warn);
                    write_log(log_writer, &warn);
                    // Restore the backup so the source is not lost
                    if source_backed_up {
                        let _ = std::fs::rename(&backup, item_full_path);
                    }
                } else {
                    // Success — remove the backup
                    if source_backed_up {
                        let _ = std::fs::remove_file(&backup);
                    }
                    let msg = format!("  Replaced source → {}", final_output_str);
                    sink.log(&msg);
                    write_log(log_writer, &msg);
                }
            } else if source_backed_up {
                // temp_output_file == output_file, just clean up backup
                let _ = std::fs::remove_file(&backup);
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
    output_dir_override: Option<&std::path::Path>,
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

    // ── DV/HDR10+ route determination ─────────────────────────
    let item_dovi_profile = queue[idx].probe.dovi_profile;
    let caps = crate::dovi_tools::capabilities();

    // Per-file codec/encoder/container resolution (§2.4.0)
    let source_ext = std::path::Path::new(&item_full_path)
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let mut resolved =
        resolve_file_settings(&item_video_codec, &source_ext, settings, detected_encoders);

    // Dolby Vision: one routing decision (see `dovi_route_for_encoder`, which
    // accounts for the VAAPI swap). Compatibility mode tonemaps to SDR
    // H.264, so it never keeps DV.
    let allow_vaapi_dv = vaapi_dv_env_allowed();
    let dovi_route_kind = item_dovi_profile.map(|profile| {
        dovi_route_for_encoder(
            profile,
            &resolved.encoder_name,
            &resolved.codec_family,
            detected_encoders,
            &ffmpeg_caps(),
            caps.can_process_dovi && caps.can_package_dovi_mp4,
            preserve_hdr && !settings.compatibility_mode,
            allow_vaapi_dv,
        )
    });
    let is_dovi_native = matches!(dovi_route_kind, Some(DoviRoute::Native { .. }));
    let is_dovi_rpu_pipeline = dovi_route_kind == Some(DoviRoute::RpuPipeline);
    let keeps_dovi = dovi_route_kind.is_some_and(DoviRoute::keeps_dovi);
    match dovi_route_kind {
        Some(DoviRoute::Native { encoder, family }) => {
            if resolved.encoder_name != encoder {
                sink.log(&format!(
                    "  Dolby Vision: native RPU coding needs {encoder} - switching from {}",
                    resolved.encoder_name
                ));
            }
            resolved.codec_family = family.to_string();
            resolved.encoder_name = encoder.to_string();
        }
        Some(DoviRoute::RpuPipeline) => {
            // The RPU inject pipeline only works with HEVC bitstreams.
            if resolved.codec_family != "hevc" {
                resolved.codec_family = "hevc".to_string();
                resolved.encoder_name = pick_encoder("hevc", detected_encoders);
                sink.log("  Dolby Vision requires HEVC output - forcing HEVC encoder");
            }
            // AMD VAAPI 10-bit HEVC encoding previously produced visible
            // artifacts. Fall back to libx265 unless HISTV_ALLOW_VAAPI_DV=1
            // is set, which permits VAAPI for the RPU pipeline (testing).
            // Only profile 7 and an ffmpeg without native DV reach this swap:
            // otherwise `dovi_route_for_encoder` has already chosen the
            // native libx265 route.
            if cfg!(target_os = "linux") && is_vaapi_encoder(&resolved.encoder_name) {
                if allow_vaapi_dv {
                    sink.log(
                        "  Dolby Vision: HISTV_ALLOW_VAAPI_DV=1 - keeping VAAPI encoder for DV (test mode)",
                    );
                } else {
                    let sw = software_fallback("hevc").to_string();
                    sink.log(&format!(
                        "  Dolby Vision: VAAPI 10-bit encoding unreliable - falling back to {} (set HISTV_ALLOW_VAAPI_DV=1 to test VAAPI DV)",
                        sw,
                    ));
                    resolved.encoder_name = sw;
                }
            }
        }
        Some(DoviRoute::Hdr10) if preserve_hdr && !settings.compatibility_mode => {
            sink.log(&dovi_hdr10_log_line(
                item_dovi_profile.unwrap_or_default(),
                &ffmpeg_caps(),
                caps.can_process_dovi,
                caps.can_package_dovi_mp4,
            ));
        }
        _ => {}
    }

    // Tier 2: HDR10+ source + crate available → full metadata preservation
    #[cfg(feature = "dovi")]
    let item_dovi_bl_compat_id = queue[idx].probe.dovi_bl_compat_id;
    #[cfg(feature = "dovi")]
    let hdr10p_route = hdr10plus_route(
        queue[idx].probe.has_hdr10plus,
        caps.can_process_hdr10plus,
        preserve_hdr && !settings.compatibility_mode,
        &resolved.encoder_name,
        &resolved.codec_family,
        dovi_route_kind,
        &ffmpeg_caps(),
        &queue[idx].probe.hdr10plus_profile,
    );

    let file_encoder = &resolved.encoder_name;
    let file_codec_family = &resolved.codec_family;
    // The RPU pipeline forces MP4 (MP4Box sets the DV flags); native DV
    // coding keeps the configured container.
    let file_ext = resolve_output_ext(
        &item_full_path,
        &item_video_codec,
        item_dovi_profile,
        settings,
        preserve_hdr,
        detected_encoders,
    )
    .to_string();
    if is_dovi_rpu_pipeline && file_ext != resolved.container_ext {
        sink.log("  Dolby Vision requires MP4 container - output set to MP4 for this file");
    }
    let sw_fallback = software_fallback(file_codec_family).to_string();
    let is_sw = file_encoder.starts_with("lib");
    let target_codec = file_codec_family.as_str();

    // Per-file pixel format and optional tonemap filter (#1, #2).
    // Compatibility mode forces SDR output — H.264 "compat" targets devices
    // that don't support 10-bit/HDR, so tonemap regardless of preserve_hdr.
    // Kept Dolby Vision always requires 10-bit output (Main 10 HEVC for the RPU
    // pipeline; native coding refuses other formats), so override regardless of
    // the probe's colour-transfer detection (some DV containers omit smpte2084
    // from container-level metadata).
    let force_sdr = settings.compatibility_mode;
    let (file_pix_fmt, tonemap_filter): (&str, Option<&'static str>) = if keeps_dovi {
        if !item_is_hdr {
            sink.log("  Dolby Vision requires 10-bit output - forcing p010le (source container omitted HDR colour metadata)");
        }
        ("p010le", None)
    } else if item_is_hdr && (!preserve_hdr || force_sdr) {
        // HDR source → SDR: tonemap via zscale + Hable curve
        ("yuv420p", Some(TONEMAP_HABLE))
    } else if item_is_hdr {
        // HDR source, preserve HDR
        ("p010le", None)
    } else {
        // SDR source
        ("yuv420p", None)
    };

    sink.batch_status(&format!("[{}/{}] {}", file_counter, total, item_file_name));

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
            // "folder" - default: use the configured output folder. When that
            // folder is on a remote mount, the wave loop passes a local override
            // dir so the encode writes locally and is copied back afterwards
            // (mirrors replace/beside: keeps the network to a single burst).
            let folder: &std::path::Path = output_dir_override
                .unwrap_or_else(|| std::path::Path::new(&settings.output_folder));
            let out = folder.join(format!("{}.{}", item_base_name, ext));
            (out.clone(), out)
        }
    };
    // On Windows, prepend the extended-length path prefix when the path
    // exceeds 240 characters to avoid MAX_PATH (260) failures.
    #[cfg(target_os = "windows")]
    let output_str = {
        let raw = temp_output_file.to_string_lossy();
        if raw.len() > 240 && !raw.starts_with("\\\\?\\") {
            format!("\\\\?\\{}", raw)
        } else {
            raw.to_string()
        }
    };
    #[cfg(not(target_os = "windows"))]
    let output_str = temp_output_file.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    let final_output_str = {
        let raw = output_file.to_string_lossy();
        if raw.len() > 240 && !raw.starts_with("\\\\?\\") {
            format!("\\\\?\\{}", raw)
        } else {
            raw.to_string()
        }
    };
    #[cfg(not(target_os = "windows"))]
    let final_output_str = output_file.to_string_lossy().to_string();

    let log_msg = format!("[{}/{}] {}", file_counter, total, item_file_name);
    sink.log(&log_msg);
    write_log(log_writer, &log_msg);

    if tonemap_filter.is_some() {
        sink.log("  HDR → SDR: tonemapping with Hable curve");
    }

    // Overwrite check - in replace mode, we're replacing the source so
    // no overwrite prompt is needed. In other modes, check the final output.
    if !is_replace_mode && output_file.exists() && !batch_control.overwrite_always() {
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

    // Pre-decision bitrate check: derive the MKV's real bitrate from its
    // current size so the encoding decision ignores stale statistics
    // tags. Read-only (a stat); the source file is never written.
    let mut item_video_bitrate_mbps = item_video_bitrate_mbps;
    if let Some(bps) = crate::mkv_tags::estimate_bitrate_after_probe(
        &item_full_path,
        item_duration_secs,
        &queue[idx].probe.audio_streams,
    ) {
        let corrected_mbps = bps as f64 / 1_000_000.0;
        if (corrected_mbps - item_video_bitrate_mbps).abs() > 0.1 {
            sink.log(&format!(
                "  Bitrate from file size: {:.2}Mbps → {:.2}Mbps",
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
        &RateControlParams {
            mode: &settings.rate_control_mode,
            qp_i: settings.qp_i,
            qp_p: settings.qp_p,
            crf_val: settings.crf_val,
        },
        settings.peak_multiplier,
    );

    // Tonemapping requires re-encoding — a stream copy can't apply a
    // `-vf` filter. If the decision is Copy but we've committed to an
    // HDR→SDR tonemap, ffmpeg will reject `-c:v copy -vf zscale=...`
    // with EINVAL (exit 234). Override to a quality-based transcode so
    // the user's SDR request is actually honoured. We use CRF/CQP
    // (not VBR) because the source is already at/below the bitrate
    // threshold — VBR would risk inflating the file.
    let decision = if tonemap_filter.is_some() && matches!(decision, EncodeDecision::Copy) {
        if settings.rate_control_mode.eq_ignore_ascii_case("CRF") {
            EncodeDecision::Crf {
                crf: settings.crf_val,
                qi_fallback: settings.qp_i,
                qp_fallback: settings.qp_p,
            }
        } else {
            EncodeDecision::Cqp {
                qi: settings.qp_i,
                qp: settings.qp_p,
            }
        }
    } else {
        decision
    };

    // Profile 5 has no HDR10 layer, so converting it to SDR (tonemap or not)
    // probably leaves the base layer's colour cast in the file.
    if let Some(line) = item_dovi_profile.and_then(|profile| {
        dovi_sdr_log_line(
            profile,
            OutputRange {
                compat: settings.compatibility_mode,
                preserve_hdr,
            },
            matches!(decision, EncodeDecision::Copy),
        )
    }) {
        sink.log(&line);
        write_log(log_writer, &line);
    }

    // ── Pre-encode: DV/HDR10+ metadata extraction ────────────
    // Only extract when actually re-encoding (not copy). Extraction is
    // expensive (demux + full bitstream parse) so we skip it for copies
    // where the original bitstream (and its DV/HDR10+ data) is preserved.
    #[cfg(feature = "dovi")]
    let is_copy = matches!(decision, EncodeDecision::Copy);

    #[cfg(feature = "dovi")]
    let extracted_rpus = if is_dovi_rpu_pipeline && !is_copy {
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

    // libx265 reads the metadata from a JSON file during the encode; it is
    // deleted when this function returns.
    #[cfg(feature = "dovi")]
    let mut hdr10plus_json: Option<tempfile::TempPath> = None;
    #[cfg(feature = "dovi")]
    #[cfg(feature = "dovi")]
    let hdr10p_keep = match hdr10p_route {
        // A copy keeps the source bitstream, metadata included.
        _ if is_copy => None,
        Ok(route) => route,
        Err(reason) => {
            let msg = format!("  HDR10+: Static HDR10 only ({reason})");
            sink.log(&msg);
            write_log(log_writer, &msg);
            None
        }
    };
    #[cfg(feature = "dovi")]
    let mut extracted_hdr10plus = if let Some(route) = hdr10p_keep {
        sink.log("  HDR10+ detected - extracting dynamic metadata for preservation");
        if keeps_dovi {
            sink.log("  HDR10+ with Dolby Vision: experimental");
        }
        match hdr10plus_pipeline::extract_hdr10plus(std::path::Path::new(&item_full_path), sink)
            .await
        {
            Ok(meta) if route == Hdr10PlusRoute::X265Native => {
                // System temp, not the output folder: a killed process
                // leaves no stray JSON beside the user's files.
                let json = tempfile::Builder::new()
                    .prefix("histv_hdr10p_")
                    .suffix(".json")
                    .tempfile()
                    .map(tempfile::NamedTempFile::into_temp_path)
                    .map_err(|e| format!("Could not create HDR10+ JSON: {e}"))
                    .and_then(|p| hdr10plus_pipeline::write_x265_json(&meta, &p).map(|()| p));
                match json {
                    Ok(p) => hdr10plus_json = Some(p),
                    Err(e) => {
                        let msg = format!("  HDR10+: {e} - output has static HDR10 only");
                        sink.log(&msg);
                        write_log(log_writer, &msg);
                    }
                }
                None
            }
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
            &CrfProbeConfig {
                input_path: &item_full_path,
                duration_secs: item_duration_secs,
                video_args: &video_args,
                pix_fmt: file_pix_fmt,
                threads: settings.threads,
                low_priority: settings.low_priority,
                stderr_log: batch_stderr_log.clone(),
            },
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

    // Native Dolby Vision: RPU coding plus the VBV that x265 requires.
    let is_copy_decision = matches!(decision, EncodeDecision::Copy);
    if is_dovi_native && !is_copy_decision {
        let extra = dovi_native_args(
            file_encoder,
            &video_args,
            (settings.threshold * 1_000_000.0) as u64,
            settings.peak_multiplier,
            queue[idx].probe.video_bitrate_bps as u64,
        );
        sink.log(&format!(
            "  Dolby Vision: native RPU coding ({})",
            extra.join(" ")
        ));
        video_args.extend(extra);
    }
    // The injector remuxes raw HEVC, which cannot time reordered pictures,
    // so the encoder must not use B-frames.
    #[cfg(feature = "dovi")]
    if extracted_hdr10plus.is_some() && !is_copy_decision {
        sink.log("  HDR10+: B-frames off so the metadata can be injected after the encode");
        video_args.extend(["-bf".to_string(), "0".to_string()]);
    }
    #[cfg(feature = "dovi")]
    if let Some(ref json) = hdr10plus_json {
        let param = format!(
            "dhdr10-info={}",
            x265_param_path_escape(&json.to_string_lossy())
        );
        sink.log(&format!("  HDR10+: coded by libx265 ({param})"));
        add_x265_param(&mut video_args, &param);
    }
    // DV reaches the ffmpeg output through native coding or a stream copy
    // (the RPU pipeline adds it later with MP4Box).
    let dovi_in_output = file_ext == "mp4"
        && ((is_dovi_native && !is_copy_decision)
            || (is_copy_decision && item_dovi_profile.is_some()));

    // Build audio args (skipped for image sources like GIF)
    let (audio_map_args, audio_codec_args) = if is_image_source {
        (Vec::new(), Vec::new())
    } else {
        build_audio_args_from_probe(
            &queue[idx].probe.audio_streams,
            &resolved.audio_strategy,
            &file_ext,
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

        // VAAPI hwaccel decode (-hwaccel vaapi) does not propagate colour
        // metadata from the input to the output. For HDR/DV content encoded
        // through that path, explicitly set BT.2020/PQ/limited-range so the
        // output file is tagged correctly. Without this, players interpret
        // 10-bit VAAPI output as BT.709 SDR and display red or green frames.
        //
        // For all other encode paths (software, hwupload, NVENC, AMF),
        // ffmpeg propagates colour metadata from the input automatically.
        // Adding explicit flags there is redundant and can cause haloing
        // around high-contrast text: the flags force a VUI into the SPS that
        // triggers player tone mapping, creating edge overshoot on PQ content
        // that was previously displayed without mapping.
        let uses_vaapi_hwaccel_decode = is_vaapi_encoder(file_encoder)
            && file_pix_fmt == "p010le"
            && tonemap_filter.is_none()
            && vaapi_device_decode_ok(file_encoder, ffmpeg_caps().hevc_metadata_size);
        let colour_metadata = if uses_vaapi_hwaccel_decode
            && (keeps_dovi || (item_is_hdr && preserve_hdr && !force_sdr))
        {
            let colour_primaries = if keeps_dovi {
                "bt2020" // DV always uses BT.2020
            } else {
                match queue[idx].probe.color_transfer.as_str() {
                    "smpte2084" => "bt2020",
                    "arib-std-b67" => "bt2020", // HLG also uses BT.2020
                    _ => "bt709",               // fallback
                }
            };
            let colour_trc = if keeps_dovi {
                "smpte2084" // DV Profile 5→8.1 always uses PQ
            } else {
                match queue[idx].probe.color_transfer.as_str() {
                    "smpte2084" => "smpte2084",
                    "arib-std-b67" => "arib-std-b67",
                    _ => "bt709",
                }
            };
            Some(ColourMetadata {
                primaries: colour_primaries,
                trc: colour_trc,
                matrix: "bt2020nc",
                range: "tv",
            })
        } else {
            None
        };

        // hevc_vaapi: restore the picture size in the in-band SPS, which
        // AMD VCN writes with its padded coded size (green rows at the
        // bottom, extra columns on the right). Also required for DV: the
        // padded size corrupts RPU injection. See `hevc_vaapi_size_fix`.
        let (out_w, out_h) = if is_image_source {
            ((item_video_width + 1) & !1, (item_video_height + 1) & !1)
        } else {
            crate::probe::displayed_size(
                item_video_width,
                item_video_height,
                queue[idx].probe.video_rotation,
            )
        };

        let ffmpeg_args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: &item_full_path,
            output_path: &output_str,
            video_args: &video_arg_refs,
            pix_fmt: file_pix_fmt,
            audio_map_args: &audio_map_args,
            audio_codec_args: &audio_codec_args,
            is_image_source,
            threads: settings.threads,
            tonemap_filter,
            codec_family: target_codec,
            colour_metadata,
            vaapi_hevc_size: Some((out_w, out_h)),
            bsf_has_size: ffmpeg_caps().hevc_metadata_size,
            dovi_in_output,
            dovi_p5: dovi_in_output && item_dovi_profile == Some(5),
            keep_source_tag: is_copy_decision
                && matches!(source_ext.as_str(), "mp4" | "m4v" | "mov"),
            legacy_vsync: ffmpeg_caps().no_fps_mode,
            input_video_bsf: dovi_input_bsf(is_dovi_native, is_copy_decision, &item_video_codec),
        });

        let cmd_line = format!("ffmpeg {}", ffmpeg_args.join(" "));
        sink.batch_command(&cmd_line);
        let cmd_log = format!("  CMD: {cmd_line}");
        sink.log(&cmd_log);
        write_log(log_writer, &cmd_log);

        // Shared context for run_ffmpeg_with_progress calls within this file.
        let ffmpeg_ctx = FfmpegRunContext {
            low_priority: settings.low_priority,
            stderr_log: batch_stderr_log.clone(),
            stderr_label: &item_file_name,
            sink,
            batch_control,
        };

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
                &ffmpeg_ctx,
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
                &ffmpeg_ctx,
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
            match run_ffmpeg_with_progress(&ffmpeg_args, item_duration_secs, None, &ffmpeg_ctx)
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

                    // Rebuild with software encoder; HDR10+ is decided again
                    // for it (see `fallback_sw_args`).
                    #[cfg(feature = "dovi")]
                    let mut _fallback_json: Option<tempfile::TempPath> = None;
                    #[cfg(feature = "dovi")]
                    let ff_caps = ffmpeg_caps();
                    #[cfg(feature = "dovi")]
                    let fallback_hdr10 = extracted_hdr10plus.as_ref().map(|_| FallbackHdr10Plus {
                        can_process: caps.can_process_hdr10plus,
                        family: file_codec_family,
                        dovi: dovi_route_kind,
                        caps: &ff_caps,
                        profile: &queue[idx].probe.hdr10plus_profile,
                    });
                    #[cfg(not(feature = "dovi"))]
                    let fallback_hdr10: Option<FallbackHdr10Plus> = None;
                    let plan = fallback_sw_args(
                        &decision,
                        &sw_fallback,
                        qi_str,
                        qp_str,
                        fallback_hdr10.as_ref(),
                        &mut || {
                            #[cfg(feature = "dovi")]
                            if let Some(ref meta) = extracted_hdr10plus {
                                let p = tempfile::Builder::new()
                                    .prefix("histv_hdr10p_")
                                    .suffix(".json")
                                    .tempfile()
                                    .map(tempfile::NamedTempFile::into_temp_path)
                                    .map_err(|e| format!("Could not create HDR10+ JSON: {e}"))?;
                                hdr10plus_pipeline::write_x265_json(meta, &p)?;
                                let path = p.to_string_lossy().into_owned();
                                _fallback_json = Some(p);
                                return Ok(path);
                            }
                            Err("no HDR10+ metadata".to_string())
                        },
                    );
                    for note in &plan.notes {
                        sink.log(note);
                        write_log(log_writer, note);
                    }
                    // Without injection the SEIs must not be added to the
                    // fallback's output (it has B-frames).
                    #[cfg(feature = "dovi")]
                    if !plan.keep_injection {
                        extracted_hdr10plus = None;
                    }
                    let sw_video_args = plan.video_args;

                    // Reuse audio args from the primary encode - inputs haven't changed
                    let sw_ref: Vec<&str> = sw_video_args.iter().map(|s| s.as_str()).collect();
                    let sw_args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
                        input_path: &item_full_path,
                        output_path: &output_str,
                        video_args: &sw_ref,
                        pix_fmt: file_pix_fmt,
                        audio_map_args: &audio_map_args,
                        audio_codec_args: &audio_codec_args,
                        is_image_source,
                        threads: settings.threads,
                        tonemap_filter,
                        codec_family: target_codec,
                        colour_metadata: None, // SW encode propagates from input
                        vaapi_hevc_size: None, // SW encode doesn't need VAAPI crop fix
                        bsf_has_size: false,
                        // Never native DV here: native routes are software already.
                        dovi_in_output: false,
                        dovi_p5: false,
                        keep_source_tag: false,
                        legacy_vsync: ffmpeg_caps().no_fps_mode,
                        input_video_bsf: None,
                    });

                    let sw_cmd = format!("ffmpeg {}", sw_args.join(" "));
                    sink.batch_command(&sw_cmd);
                    let sw_cmd_log = format!("  CMD (fallback): {sw_cmd}");
                    sink.log(&sw_cmd_log);
                    write_log(log_writer, &sw_cmd_log);

                    // Use shared helper for progress, pause/cancel, low-priority (#6)
                    match run_ffmpeg_with_progress(&sw_args, item_duration_secs, None, &ffmpeg_ctx)
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

    // A copied profile 5 stream keeps its dvcC record, but ffmpeg writes the
    // sample entry as hev1; restore dvhe so players apply Dolby Vision.
    if copy_needs_dovi_entry(&decision, dovi_in_output && item_dovi_profile == Some(5)) {
        match crate::mp4_entry::mark_dolby_vision_file(&temp_output_file) {
            Ok(n) if n > 0 => sink.log("  Dolby Vision: profile 5 sample entry restored"),
            Ok(_) => {}
            Err(e) => {
                let msg = format!(
                    "  WARNING: Dolby Vision profile 5 sample entry not set ({e}); some players may show wrong colours"
                );
                sink.log(&msg);
                write_log(log_writer, &msg);
            }
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
        &resolved.audio_strategy,
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
    queue: &mut [QueueItem],
    settings: &BatchSettings,
    detected_encoders: &[EncoderInfo],
    wave_plan: Option<Vec<WaveItem>>,
    disk_monitor: Option<&crate::disk_monitor::DiskMonitor>,
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

    // ── Folder-mode remote-output staging (politeness) ──
    // When output mode is "folder" and the configured output folder lives on a
    // remote mount, folder-mode encodes inside a staging wave write to a local
    // override dir and are bulk-copied back afterwards, exactly like the
    // replace/beside back-copy. Computed once: a single mount lookup.
    let stage_folder_output = settings.output_mode == "folder" && {
        let mut mc = crate::remote::MountCache::new();
        mc.is_remote(std::path::Path::new(&settings.output_folder))
    };

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
                )
                .await
                {
                    // Rewrite queue item path to the staged local copy
                    queue[idx].full_path = ctx.local_path().to_string_lossy().to_string();
                    wave_staging_contexts.push((idx, ctx, original_path));
                }
                // If staging fails, the file keeps its remote path and will be encoded in-place
            }
        }

        // Local override dir for folder-mode output when the output folder is
        // remote: the encode writes here, then wave cleanup copies it back to
        // the remote output folder. None unless this wave stages and the output
        // folder is remote, so the common path is unchanged.
        let folder_output_override: Option<std::path::PathBuf> =
            match (work.staging_dir.as_ref(), stage_folder_output) {
                (Some(sd), true) => {
                    let d = sd.join("folder-output");
                    let _ = tokio::fs::create_dir_all(&d).await;
                    Some(d)
                }
                _ => None,
            };

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

            // Disk-aware mode: check partition usage and wait if over the limit
            if let Some(dm) = disk_monitor {
                if !dm.check_and_wait(sink, batch_control).await {
                    was_cancelled = true;
                    break 'outer;
                }
            }

            file_counter += 1;
            sink.batch_progress(file_counter, total);

            let result = encode_single_file(
                idx,
                queue,
                settings,
                folder_output_override.as_deref(),
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
                // Resolve the extension with the shared helper so the back-copy
                // looks for exactly the file the encode wrote, including the DV
                // Tier-1 / compatibility-mode MP4 override.
                let ext = resolve_output_ext(
                    &original_path,
                    &queue[idx].probe.video_codec,
                    queue[idx].probe.dovi_profile,
                    settings,
                    preserve_hdr,
                    detected_encoders,
                );
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
                        match tokio::fs::copy(&local_output, &temp_remote).await {
                            Ok(_) => {
                                // Backup-rename-delete pattern (mirrors local replace mode)
                                let backup =
                                    remote_dir.join(format!("{}.histv-bak.{}", base_name, ext));
                                let backed_up = if remote.exists() {
                                    std::fs::rename(remote, &backup).is_ok()
                                } else {
                                    false
                                };
                                if let Err(e) = std::fs::rename(&temp_remote, &final_remote) {
                                    sink.log(&format!(
                                        "  WARNING: Could not rename temp to final on remote: {e}"
                                    ));
                                    // Restore the backup so the original is not lost
                                    if backed_up {
                                        let _ = std::fs::rename(&backup, remote);
                                    }
                                } else {
                                    // Success — remove backup
                                    if backed_up {
                                        let _ = std::fs::remove_file(&backup);
                                    }
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
                        match tokio::fs::copy(&local_output, &final_remote).await {
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

            // Folder mode: copy the locally-staged output back to the remote
            // output folder (the encode wrote to folder_output_override). Uses
            // the same extension-resolution as the beside back-copy above.
            if let Some(ref local_dir) = folder_output_override {
                if queue[idx].status == QueueItemStatus::Done {
                    let remote = std::path::Path::new(&original_path);
                    // Shared helper: identical extension rule to the encode path
                    // and the replace/beside back-copy (DV Tier-1 / compat → MP4).
                    let ext = resolve_output_ext(
                        &original_path,
                        &queue[idx].probe.video_codec,
                        queue[idx].probe.dovi_profile,
                        settings,
                        preserve_hdr,
                        detected_encoders,
                    );
                    let base_name = remote.file_stem().unwrap_or_default().to_string_lossy();
                    let local_output = local_dir.join(format!("{}.{}", base_name, ext));
                    let remote_dir = std::path::Path::new(&settings.output_folder);
                    let remote_dest = remote_dir.join(format!("{}.{}", base_name, ext));
                    if local_output.exists() {
                        let _ = std::fs::create_dir_all(remote_dir);
                        sink.log(&format!(
                            "  Moving staged output to remote: {} → {}",
                            local_output.display(),
                            remote_dest.display()
                        ));
                        match tokio::fs::copy(&local_output, &remote_dest).await {
                            Ok(_) => {
                                let _ = std::fs::remove_file(&local_output);
                                sink.log(&format!(
                                    "  Copied output to remote → {}",
                                    remote_dest.display()
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
                    } else {
                        sink.log(&format!(
                            "  WARNING: expected output {} not found - remote folder not updated",
                            local_output.display()
                        ));
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

    // Best-effort: remove the folder-mode output staging dir. Empty on the
    // success path (outputs are copied back and removed individually); may
    // hold a partial output after a mid-wave cancel. Covers both.
    if stage_folder_output {
        let _ = std::fs::remove_dir_all(
            crate::staging::resolve_staging_dir(None).join("folder-output"),
        );
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

/// Context for a single `run_ffmpeg_with_progress` invocation, grouping
/// the fields that stay constant across passes within one file.
struct FfmpegRunContext<'a> {
    low_priority: bool,
    stderr_log: Option<SharedStderrLog>,
    stderr_label: &'a str,
    sink: &'a dyn EventSink,
    batch_control: &'a dyn BatchControl,
}

/// Spawn ffmpeg with the given args, stream stderr for progress, handle
/// pause/cancel, and return the result. Extracted to avoid duplicating the
/// poll loop for two-pass encoding and software fallback.
///
/// `ctx.stderr_log` is an optional shared batch log file. When provided, all
/// ffmpeg stderr output is appended there. `ctx.stderr_label` is written into
/// the separator header (typically the filename being encoded).
async fn run_ffmpeg_with_progress(
    args: &[String],
    file_duration: f64,
    pass: Option<(u8, u8)>,
    ctx: &FfmpegRunContext<'_>,
) -> Result<FfmpegRunResult, String> {
    let mut cmd = ffbin::ffmpeg_command();
    cmd.args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    // Set below-normal priority on the ffmpeg child process
    if ctx.low_priority {
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
    if ctx.low_priority {
        if let Some(pid) = child.id() {
            let _ = std::process::Command::new("renice")
                .args(["-n", "10", "-p", &pid.to_string()])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
    }

    ctx.sink
        .log(&format!("  ffmpeg PID: {}", child.id().unwrap_or(0)));

    // Stream stderr for progress using a blocking thread, with log capture
    let progress = FfmpegProgress::new();
    let stderr_thread = child.stderr.take().map(|stderr| {
        spawn_stderr_reader(stderr, &progress, ctx.stderr_log.clone(), ctx.stderr_label)
    });

    // Poll for cancellation/pause while waiting for ffmpeg, and emit progress
    let mut last_progress_emit = std::time::Instant::now() - std::time::Duration::from_millis(500);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {}
            Err(_) => break,
        }
        if ctx.batch_control.should_cancel_current() || ctx.batch_control.should_cancel_all() {
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

        while ctx.batch_control.is_paused() {
            if ctx.batch_control.should_cancel_current() || ctx.batch_control.should_cancel_all() {
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
                    ctx.sink.file_progress(pct, secs, file_duration, pass);
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
        ctx.sink
            .file_progress(100.0, file_duration, file_duration, pass);
    }

    let exit_code = exit_status.map(|s| s.code().unwrap_or(-1)).unwrap_or(-1);
    let final_frames = progress.frames();

    Ok(FfmpegRunResult {
        exit_code,
        was_cancelled_current: ctx.batch_control.should_cancel_current(),
        was_cancelled_all: ctx.batch_control.should_cancel_all(),
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
        // SAFETY: `status` is zero-initialised with the correct `dw_length`,
        // and `GlobalMemoryStatusEx` only writes within that fixed-size struct.
        // Called on the main thread with no concurrent mutation of `status`.
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

/// Returns true if the system has insufficient RAM for extended lookahead (#3).
/// Accepts a pre-cached RAM value.
pub fn precision_needs_two_pass_with_ram(ram_gb: u64) -> bool {
    ram_gb < 8
}

/// Configuration for a CRF viability probe.
pub struct CrfProbeConfig<'a> {
    pub input_path: &'a str,
    pub duration_secs: f64,
    pub video_args: &'a [String],
    pub pix_fmt: &'a str,
    pub threads: u32,
    pub low_priority: bool,
    pub stderr_log: Option<SharedStderrLog>,
}

/// Probe CRF viability by encoding three 10-second samples from 25%, 50%,
/// and 75% through the file and returning the average output bitrate in Mbps.
///
/// The sink receives progress updates as "Probing 1/3", "Probing 2/3", etc.
/// via batch_status. Returns None if the file is too short to sample (<120s)
/// or if any sample encode fails.
pub async fn probe_crf_viability(
    config: &CrfProbeConfig<'_>,
    sink: &dyn EventSink,
    batch_control: &dyn BatchControl,
) -> Option<f64> {
    if config.duration_secs < MIN_PROBE_DURATION_SECS {
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

        let seek_secs = config.duration_secs * fraction;
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
            config.input_path.to_string(),
            "-map".into(),
            "0:v:0".into(),
            "-an".into(),
            "-sn".into(),
        ];
        if config.threads > 0 {
            args.extend(vec!["-threads".into(), config.threads.to_string()]);
        }
        args.extend(config.video_args.iter().cloned());
        args.extend(vec!["-pix_fmt".into(), config.pix_fmt.to_string()]);
        args.push(tmp_str.clone());

        let probe_label = format!("probe_{}", sample_num);
        let probe_ctx = FfmpegRunContext {
            low_priority: config.low_priority,
            stderr_log: config.stderr_log.clone(),
            stderr_label: &probe_label,
            sink,
            batch_control,
        };
        let result = run_ffmpeg_with_progress(
            &args,
            sample_duration,
            Some((sample_num as u8, seek_points.len() as u8)),
            &probe_ctx,
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

/// Component parts for assembling an ffmpeg command line.
#[derive(Default)]
struct FfmpegAssemblyParts<'a> {
    input_path: &'a str,
    output_path: &'a str,
    video_args: &'a [&'a str],
    pix_fmt: &'a str,
    audio_map_args: &'a [String],
    audio_codec_args: &'a [String],
    is_image_source: bool,
    threads: u32,
    tonemap_filter: Option<&'a str>,
    codec_family: &'a str,
    /// Explicit colour metadata for VAAPI hwaccel decode which does not
    /// propagate colour primaries/transfer/matrix from input. Only set
    /// for the VAAPI hwaccel decode path; other paths propagate
    /// automatically and explicit flags cause haloing on PQ content.
    colour_metadata: Option<ColourMetadata>,
    /// Output picture size, for the `hevc_vaapi` size fix
    /// (`hevc_vaapi_size_fix`; ignored for other encoders).
    vaapi_hevc_size: Option<(u32, u32)>,
    /// ffmpeg's `hevc_metadata` has `width`/`height` (`FfmpegCaps`).
    bsf_has_size: bool,
    /// The output stream carries Dolby Vision (native coding or a stream
    /// copy). MP4 then needs `-strict unofficial`, without which movenc
    /// drops the dvcC/dvvC record.
    dovi_in_output: bool,
    /// Dolby Vision profile 5 kept in MP4: sample entry `dvh1`, not `hvc1`.
    dovi_p5: bool,
    /// Stream copy from an MP4/MOV source: keep the source's sample entry.
    /// Forcing `hvc1` onto a copied `hev1` stream (parameter sets in-band)
    /// writes a file ffmpeg itself cannot open.
    keep_source_tag: bool,
    /// ffmpeg predates `-fps_mode` (`FfmpegCaps::no_fps_mode`): spell the
    /// frame-rate mode `-vsync`.
    legacy_vsync: bool,
    /// Bitstream filters applied to the mapped video stream before it is
    /// decoded (`dovi_input_bsf`).
    input_video_bsf: Option<&'a str>,
}

/// Colour metadata to embed in the ffmpeg output. Only needed for
/// VAAPI hwaccel decode, which does not propagate colour information
/// from the input. For all other paths, ffmpeg propagates automatically
/// and explicit flags cause haloing on PQ content.
#[derive(Clone, Copy)]
struct ColourMetadata {
    primaries: &'static str,
    trc: &'static str,
    matrix: &'static str,
    range: &'static str,
}

/// Assemble the full ffmpeg argument list from its component parts.
/// Used by both the primary encode and the software-fallback path
/// to avoid duplicating the invocation pattern.
fn assemble_ffmpeg_args(parts: &FfmpegAssemblyParts<'_>) -> Vec<String> {
    let render_node = render_node_for(parts.video_args.get(1).copied().unwrap_or_default());
    assemble_ffmpeg_args_for(parts, render_node.as_deref(), TargetOs::current())
}

/// `assemble_ffmpeg_args` with the render node and OS passed in, so the
/// hardware arms are testable without a GPU or on another platform.
fn assemble_ffmpeg_args_for(
    parts: &FfmpegAssemblyParts<'_>,
    render_node: Option<&str>,
    os: TargetOs,
) -> Vec<String> {
    // A tonemap filter requires an encode — ffmpeg cannot apply `-vf`
    // when the video stream is being copied (`-c:v copy -vf ...` fails
    // with EINVAL, exit 234). The caller must rewrite the decision to
    // a transcode before assembling. Guarded with debug_assert so
    // future call sites can't reintroduce the contradiction silently.
    debug_assert!(
        !(parts.tonemap_filter.is_some()
            && parts
                .video_args
                .windows(2)
                .any(|w| w[0] == "-c:v" && w[1] == "copy")),
        "assemble_ffmpeg_args: tonemap_filter is incompatible with -c:v copy"
    );

    // Detect encoder type from video_args for hardware device init.
    // video_args layout: ["-c:v", "<encoder_name>", ...]
    let encoder_name = parts
        .video_args
        .windows(2)
        .find(|w| w[0] == "-c:v")
        .map(|w| w[1]);

    // Image sources are decoded to RGB and converted to 8-bit 4:2:0 below.
    let hw = hw_device_args_with(
        encoder_name.unwrap_or(""),
        if parts.is_image_source {
            "yuv420p"
        } else {
            parts.pix_fmt
        },
        parts.tonemap_filter.is_some(),
        render_node,
        os,
        vaapi_device_decode_ok(encoder_name.unwrap_or(""), parts.bsf_has_size),
    );
    let size_fix = parts.vaapi_hevc_size.and_then(|(w, h)| {
        hevc_vaapi_size_fix(encoder_name.unwrap_or(""), w, h, parts.bsf_has_size)
    });
    let hw_device_init = hw.pre_input;
    // The size-fix pad runs on software frames, just before the upload.
    let hw_upload_filter = match (
        size_fix.as_ref().and_then(|f| f.pad.as_deref()),
        hw.upload_filter,
    ) {
        (Some(pad), Some(up)) => Some(format!("{pad},{up}")),
        (Some(pad), None) => Some(pad.to_string()),
        (None, up) => up,
    };

    let mut args: Vec<String> = vec![
        "-err_detect".into(),
        "ignore_err".into(),
        "-probesize".into(),
        "100M".into(),
        "-analyzeduration".into(),
        "100M".into(),
        "-y".into(),
    ];

    // Hardware device init must come before -i
    args.extend(hw_device_init);

    // An input option: it must come before -i.
    if let Some(bsf) = parts.input_video_bsf {
        args.extend(vec!["-bsf:v:0".into(), bsf.into()]);
    }

    args.extend(vec![
        "-i".into(),
        parts.input_path.to_string(),
        "-map".into(),
        "0:v:0".into(),
    ]);

    // Thread limit (0 = ffmpeg default / auto)
    if parts.threads > 0 {
        args.extend(vec!["-threads".into(), parts.threads.to_string()]);
    }

    if parts.is_image_source {
        // GIF/APNG: no audio or subtitle streams, pad odd dimensions,
        // force yuv420p (GIF decodes to rgb24/rgba). Hardware encoders get
        // the same upload filter as video sources.
        args.extend(vec!["-an".into(), "-sn".into()]);
        args.extend(parts.video_args.iter().map(|s| s.to_string()));
        let pad = "pad=ceil(iw/2)*2:ceil(ih/2)*2";
        let vf = match &hw_upload_filter {
            Some(up) => format!("{pad},{up}"),
            None => pad.to_string(),
        };
        args.extend(vec!["-vf".into(), vf]);
        if hw.emit_pix_fmt {
            args.extend(vec!["-pix_fmt".into(), "yuv420p".into()]);
        }
    } else {
        // Normal video: per-stream audio maps + subtitle passthrough
        args.extend_from_slice(parts.audio_map_args);
        args.extend(parts.video_args.iter().map(|s| s.to_string()));
        // Never change the frame rate: ffmpeg 6.1 duplicated frames under its
        // default mode (51 source frames became 59), which misaligns Dolby
        // Vision RPUs and HDR10+ metadata with their pictures. A stream copy
        // already keeps every packet. Image sources (GIF, APNG, MJPEG) stay
        // on ffmpeg's default: they carry no per-frame metadata, and their
        // timing is rounded frame delays. Animated WebP has its own encoder
        // command (`webp_decode`) and does not come through here.
        if encoder_name != Some("copy") {
            let flag = if parts.legacy_vsync {
                "-vsync"
            } else {
                "-fps_mode"
            };
            args.extend(vec![flag.into(), "passthrough".into()]);
        }
        // Build the video filter chain. For VAAPI/QSV encoders the frames
        // must be uploaded to the hardware device; chain hwupload after any
        // tonemap filter so the tonemap runs on CPU and hwupload moves the
        // result to the GPU.
        //
        // When VAAPI hwaccel decode is used (10-bit HDR/DV without tonemap),
        // frames are already on the GPU as VAAPI surfaces. An explicit
        // scale_vaapi filter forces P010 surface format between decoder and
        // encoder (AMD VAAPI produces green frames without it). -pix_fmt must
        // not be set (the encoder auto-negotiates from the VAAPI surface).
        if let Some(filter) = parts.tonemap_filter {
            if let Some(hw_filter) = &hw_upload_filter {
                args.extend(vec!["-vf".into(), format!("{filter},{hw_filter}")]);
            } else {
                args.extend(vec!["-vf".into(), filter.to_string()]);
            }
        } else if let Some(hw_filter) = &hw_upload_filter {
            args.extend(vec!["-vf".into(), hw_filter.clone()]);
        }
        if hw.emit_pix_fmt {
            // Not set for VAAPI hwaccel decode (the encoder receives VAAPI
            // surfaces and auto-negotiates; -pix_fmt would force a wrong
            // software-to-hardware conversion) nor for Vulkan and D3D12,
            // which accept only their own hardware frames.
            args.extend(vec!["-pix_fmt".into(), parts.pix_fmt.to_string()]);
        }
        args.extend_from_slice(parts.audio_codec_args);
        // Subtitle handling: MP4 only supports mov_text (timed text).
        // Using `-c:s mov_text` converts text-based subs (SRT, ASS) and
        // causes ffmpeg to silently skip bitmap subs (PGS, DVB) that
        // cannot be converted. MKV supports all subtitle codecs as-is.
        if parts.output_path.ends_with(".mp4") {
            args.extend(vec![
                "-map".into(),
                "0:s?".into(),
                "-c:s".into(),
                "mov_text".into(),
            ]);
        } else {
            args.extend(vec![
                "-map".into(),
                "0:s?".into(),
                "-c:s".into(),
                "copy".into(),
                "-disposition:s:0".into(),
                "default".into(),
            ]);
        }
    }

    // MP4-specific muxing flags
    if parts.output_path.ends_with(".mp4") {
        args.extend(vec!["-movflags".into(), "+faststart".into()]);
        // HEVC in MP4: force hvc1 tag (parameter sets out-of-band).
        // ffmpeg defaults to hev1 (in-band parameter sets), which many
        // hardware decoders refuse or fallback from: Apple (macOS/iOS/Safari),
        // Android/MediaCodec, NVIDIA Shield, and Chrome on Windows via DXVA.
        // hvc1 is de facto standard for universal hardware compatibility.
        // Only apply when the codec is actually HEVC to avoid tag mismatches.
        if parts.codec_family == "hevc" && !parts.keep_source_tag {
            let tag = if parts.dovi_p5 { "dvh1" } else { "hvc1" };
            args.extend(vec!["-tag:v".into(), tag.into()]);
        }
        if parts.dovi_in_output {
            args.extend(vec!["-strict".into(), "unofficial".into()]);
        }
    }

    // Colour metadata must come before the output path. Only set for
    // VAAPI hwaccel decode, which does not propagate colour
    // primaries/transfer/matrix from the input; without explicit flags
    // the output has "unknown" colour and players misinterpret 10-bit
    // data as BT.709 SDR. For all other paths, ffmpeg propagates colour
    // metadata from the source automatically, and explicit flags cause
    // haloing around high-contrast text on PQ content.
    if let Some(cm) = &parts.colour_metadata {
        args.extend(vec![
            "-color_primaries".into(),
            cm.primaries.into(),
            "-color_trc".into(),
            cm.trc.into(),
            "-colorspace".into(),
            cm.matrix.into(),
            "-color_range".into(),
            cm.range.into(),
        ]);
    }

    // hevc_vaapi: conformance window from the picture size.
    if let Some(fix) = size_fix {
        args.extend(vec!["-bsf:v".into(), fix.bsf]);
    }

    args.push(parts.output_path.to_string());
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

/// Returns true if the given audio codec can be muxed into an MP4 container.
fn is_mp4_compatible_audio(codec: &str) -> bool {
    matches!(
        codec,
        "aac" | "ac3" | "eac3" | "mp3" | "mp2" | "alac" | "pcm_s16le" | "pcm_s24le"
    )
}

/// Build per-stream audio arguments from pre-probed audio stream data (§6.2).
/// Uses AudioStrategy to determine copy/re-encode behaviour per stream.
/// `container_ext` is used to prevent MP4-incompatible codecs from being
/// copied or re-encoded into their original format (e.g. Opus in MP4).
fn build_audio_args_from_probe(
    audio_streams: &[crate::queue::AudioStreamInfo],
    strategy: &AudioStrategy,
    container_ext: &str,
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

        // Check if copying this codec into the output container would be valid.
        let is_mp4 = container_ext == "mp4";
        let copy_safe = !is_mp4 || is_mp4_compatible_audio(&stream.codec);

        match strategy {
            AudioStrategy::CopyCapped { cap_kbps } => {
                if stream.bitrate_kbps <= *cap_kbps && copy_safe {
                    codec_args.extend([format!("-c:a:{output_idx}"), "copy".into()]);
                    sink.log(&format!(
                        "  Audio {} : {} @ {}kbps - copying",
                        stream.index, stream.codec, stream.bitrate_kbps
                    ));
                } else {
                    let (target_codec, target_br) =
                        if copy_safe && has_ffmpeg_encoder(&stream.codec) {
                            let (enc_name, max_kbps) = ffmpeg_encoder_for_codec(&stream.codec);
                            let br = match max_kbps {
                                Some(max) if *cap_kbps > max => max,
                                _ => *cap_kbps,
                            };
                            (enc_name, br)
                        } else {
                            // Codec is either MP4-incompatible or has no ffmpeg encoder.
                            let fallback = "eac3";
                            if !copy_safe {
                                sink.log(&format!(
                                    "  Audio {} : {} is not supported in MP4, falling back to EAC3",
                                    stream.index, stream.codec
                                ));
                            } else {
                                sink.log(&format!(
                                    "  Audio {} : {} has no ffmpeg encoder, falling back to EAC3",
                                    stream.index, stream.codec
                                ));
                            }
                            (fallback.to_string(), *cap_kbps)
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
                if stream.codec == "aac" && stream.bitrate_kbps <= *cap_kbps {
                    codec_args.extend([format!("-c:a:{output_idx}"), "copy".into()]);
                    sink.log(&format!(
                        "  Audio {} : AAC @ {}kbps - copying",
                        stream.index, stream.bitrate_kbps
                    ));
                } else {
                    let target_br = if stream.bitrate_kbps == 0 {
                        *cap_kbps
                    } else {
                        stream.bitrate_kbps.min(*cap_kbps)
                    };
                    codec_args.extend([
                        format!("-c:a:{output_idx}"),
                        "aac".to_string(),
                        format!("-b:a:{output_idx}"),
                        format!("{target_br}k"),
                    ]);
                    sink.log(&format!(
                        "  Audio {} : {} @ {}kbps - encoding to AAC @ {}kbps",
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

impl Default for FfmpegProgress {
    fn default() -> Self {
        Self::new()
    }
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
fn tokio_stderr_to_std(
    tokio_stderr: tokio::process::ChildStderr,
) -> Result<std::process::ChildStderr, String> {
    #[cfg(windows)]
    {
        let owned = tokio_stderr
            .into_owned_handle()
            .map_err(|e| format!("Failed to take stderr handle: {e}"))?;
        Ok(std::process::ChildStderr::from(owned))
    }
    #[cfg(unix)]
    {
        let owned = tokio_stderr
            .into_owned_fd()
            .map_err(|e| format!("Failed to take stderr fd: {e}"))?;
        Ok(std::process::ChildStderr::from(owned))
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
    let std_stderr = match tokio_stderr_to_std(tokio_stderr) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("WARNING: Could not convert stderr for {label}: {e}");
            return std::thread::spawn(|| {});
        }
    };
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

/// Validate that `path` is usable as an output folder. Creates it if
/// it does not exist, then performs a write/remove round-trip to verify
/// the caller has write permission. Returns a human-readable error on
/// failure; callers translate this to their own error channel (CLI
/// eprintln+exit, GUI Tauri `Result<(), String>`).
///
/// Extracted from the duplicated blocks in `gui_commands::start_batch`
/// and `cli_main::run_batch` (#4c).
pub fn validate_output_folder(path: &std::path::Path) -> Result<(), String> {
    if !path.exists() {
        std::fs::create_dir_all(path)
            .map_err(|e| format!("Could not create output folder '{}': {e}", path.display()))?;
    }
    let test_path = path.join(".histv_write_test");
    std::fs::write(&test_path, b"")
        .map_err(|e| format!("Output folder '{}' is not writable: {e}", path.display()))?;
    let _ = std::fs::remove_file(&test_path);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::AudioStreamInfo;
    use crate::test_helpers::{default_encoders, default_settings, NoopSink};

    // ══════════════════════════════════════════════════════════════
    //  1. decide_encode_strategy
    // ══════════════════════════════════════════════════════════════

    #[test]
    fn test_decide_strategy_copy_below_threshold() {
        // File at 3.0 Mbps with a 4.0 Mbps threshold → Copy.
        let d = decide_encode_strategy(
            3.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Copy));
    }

    #[test]
    fn test_decide_strategy_copy_at_threshold() {
        // Exactly at the threshold → below-or-equal branch → Copy.
        let d = decide_encode_strategy(
            4.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Copy));
    }

    #[test]
    fn test_decide_strategy_vbr_above_threshold() {
        // 10 Mbps with a 4 Mbps threshold → VBR.
        let d = decide_encode_strategy(
            10.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        match d {
            EncodeDecision::Vbr {
                target_bps,
                peak_bps,
            } => {
                assert_eq!(target_bps, 4_000_000);
                assert_eq!(peak_bps, 6_000_000); // 4M * 1.5
            }
            _ => panic!("Expected VBR, got {:?}", d),
        }
    }

    #[test]
    fn test_decide_strategy_vbr_custom_peak_multiplier() {
        let d = decide_encode_strategy(
            10.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            2.0,
        );
        match d {
            EncodeDecision::Vbr {
                target_bps,
                peak_bps,
            } => {
                assert_eq!(target_bps, 4_000_000);
                assert_eq!(peak_bps, 8_000_000); // 4M * 2.0
            }
            _ => panic!("Expected VBR, got {:?}", d),
        }
    }

    #[test]
    fn test_decide_strategy_vbr_peak_multiplier_below_one_uses_default() {
        // peak_multiplier <= 1.0 should use the default 1.5.
        let d = decide_encode_strategy(
            10.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            0.5,
        );
        match d {
            EncodeDecision::Vbr { peak_bps, .. } => {
                assert_eq!(peak_bps, 6_000_000); // 4M * 1.5 (default)
            }
            _ => panic!("Expected VBR, got {:?}", d),
        }
    }

    #[test]
    fn test_decide_strategy_copy_hysteresis_same_codec() {
        // File at 4.5 Mbps, threshold 4.0 → 4.5 <= 4.0 * 1.15 = 4.6 → Copy.
        let d = decide_encode_strategy(
            4.5,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Copy));
    }

    #[test]
    fn test_decide_strategy_no_hysteresis_different_codec() {
        // File at 4.5 Mbps, threshold 4.0, but source is h264 and target is hevc.
        // Hysteresis only applies when source == target codec, so this should VBR.
        let d = decide_encode_strategy(
            4.5,
            4.0,
            "h264",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Vbr { .. }));
    }

    #[test]
    fn test_decide_strategy_above_hysteresis_same_codec() {
        // File at 5.0 Mbps, threshold 4.0 → 5.0 > 4.0 * 1.15 = 4.6 → VBR.
        let d = decide_encode_strategy(
            5.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Vbr { .. }));
    }

    #[test]
    fn test_decide_strategy_cqp_zero_bitrate() {
        // bitrate_mbps == 0.0 → falls into below-threshold branch,
        // but bitrate is not > 0.0 so cannot Copy. QP mode → CQP.
        let d = decide_encode_strategy(
            0.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        match d {
            EncodeDecision::Cqp { qi, qp } => {
                assert_eq!(qi, 20);
                assert_eq!(qp, 22);
            }
            _ => panic!("Expected CQP for zero bitrate, got {:?}", d),
        }
    }

    #[test]
    fn test_decide_strategy_crf_zero_bitrate() {
        // CRF mode with zero bitrate → CRF decision.
        let d = decide_encode_strategy(
            0.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "CRF",
                qp_i: 20,
                qp_p: 22,
                crf_val: 18,
            },
            1.5,
        );
        match d {
            EncodeDecision::Crf {
                crf,
                qi_fallback,
                qp_fallback,
            } => {
                assert_eq!(crf, 18);
                assert_eq!(qi_fallback, 20);
                assert_eq!(qp_fallback, 22);
            }
            _ => panic!("Expected CRF, got {:?}", d),
        }
    }

    #[test]
    fn test_decide_strategy_crf_lowercase() {
        // "crf" (lowercase) should also trigger CRF mode.
        let d = decide_encode_strategy(
            0.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "crf",
                qp_i: 20,
                qp_p: 22,
                crf_val: 18,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Crf { .. }));
    }

    #[test]
    fn test_decide_strategy_non_copyable_gif() {
        // GIF cannot be stream-copied even if below threshold.
        let d = decide_encode_strategy(
            1.0,
            4.0,
            "gif",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Cqp { .. }));
    }

    #[test]
    fn test_decide_strategy_non_copyable_webp() {
        let d = decide_encode_strategy(
            2.0,
            4.0,
            "webp",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Cqp { .. }));
    }

    #[test]
    fn test_decide_strategy_non_copyable_apng() {
        let d = decide_encode_strategy(
            2.0,
            4.0,
            "apng",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Cqp { .. }));
    }

    #[test]
    fn test_decide_strategy_non_copyable_mjpeg() {
        let d = decide_encode_strategy(
            2.0,
            4.0,
            "mjpeg",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Cqp { .. }));
    }

    #[test]
    fn test_decide_strategy_copy_below_threshold_different_codec() {
        // Below threshold → Copy, even when source != target.
        // The function copies regardless of codec mismatch when below threshold.
        let d = decide_encode_strategy(
            2.0,
            4.0,
            "h264",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Copy));
    }

    #[test]
    fn test_decide_strategy_negative_bitrate() {
        // Negative bitrate (edge case) → treated as below-or-equal to threshold.
        // Not > 0.0, so goes to CQP.
        let d = decide_encode_strategy(
            -1.0,
            4.0,
            "hevc",
            "hevc",
            &RateControlParams {
                mode: "QP",
                qp_i: 20,
                qp_p: 22,
                crf_val: 20,
            },
            1.5,
        );
        assert!(matches!(d, EncodeDecision::Cqp { .. }));
    }

    // ══════════════════════════════════════════════════════════════
    //  2. build_audio_args_from_probe
    // ══════════════════════════════════════════════════════════════

    #[test]
    fn test_audio_copy_capped_below_cap() {
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "ac3".to_string(),
            bitrate_kbps: 448,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0"]);
        assert_eq!(codec_args, vec!["-c:a:0", "copy"]);
    }

    #[test]
    fn test_audio_copy_capped_above_cap() {
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "ac3".to_string(),
            bitrate_kbps: 768,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0"]);
        // ac3 has an ffmpeg encoder, so re-encode to ac3 at cap.
        assert_eq!(codec_args, vec!["-c:a:0", "ac3", "-b:a:0", "640k"]);
    }

    #[test]
    fn test_audio_copy_capped_at_cap() {
        // Exactly at the cap → copy (<=).
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "eac3".to_string(),
            bitrate_kbps: 640,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert_eq!(codec_args, vec!["-c:a:0", "copy"]);
    }

    #[test]
    fn test_audio_copy_capped_opus_above_cap_respects_max() {
        // Opus has a max of 512 kbps via ffmpeg_encoder_for_codec.
        // With cap at 640, the encoder should use min(640, 512) = 512.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "opus".to_string(),
            bitrate_kbps: 768,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert_eq!(codec_args, vec!["-c:a:0", "libopus", "-b:a:0", "512k"]);
    }

    #[test]
    fn test_audio_copy_capped_unknown_codec_no_encoder_fallback_eac3() {
        // A codec without an ffmpeg encoder (e.g. truehd) falls back to EAC3.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "truehd".to_string(),
            bitrate_kbps: 4000,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert_eq!(codec_args, vec!["-c:a:0", "eac3", "-b:a:0", "640k"]);
    }

    #[test]
    fn test_audio_compat_capped_aac_below_cap() {
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "aac".to_string(),
            bitrate_kbps: 256,
        }];
        let strategy = AudioStrategy::CompatCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert_eq!(codec_args, vec!["-c:a:0", "copy"]);
    }

    #[test]
    fn test_audio_compat_capped_aac_above_cap() {
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "aac".to_string(),
            bitrate_kbps: 768,
        }];
        let strategy = AudioStrategy::CompatCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        // AAC above cap → re-encode to AAC at cap.
        assert_eq!(codec_args, vec!["-c:a:0", "aac", "-b:a:0", "640k"]);
    }

    #[test]
    fn test_audio_compat_capped_non_aac_reencode() {
        // Non-AAC codec in compat mode → always re-encode to AAC.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "ac3".to_string(),
            bitrate_kbps: 448,
        }];
        let strategy = AudioStrategy::CompatCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        // target_br = min(448, 640) = 448
        assert_eq!(codec_args, vec!["-c:a:0", "aac", "-b:a:0", "448k"]);
    }

    #[test]
    fn test_audio_compat_capped_non_aac_above_cap() {
        // Non-AAC above cap → re-encode to AAC at cap.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "eac3".to_string(),
            bitrate_kbps: 768,
        }];
        let strategy = AudioStrategy::CompatCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert_eq!(codec_args, vec!["-c:a:0", "aac", "-b:a:0", "640k"]);
    }

    #[test]
    fn test_audio_empty_streams() {
        let streams: Vec<AudioStreamInfo> = vec![];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert!(map_args.is_empty());
        assert!(codec_args.is_empty());
    }

    #[test]
    fn test_audio_unknown_codec_skipped() {
        // "unknown" codec streams are skipped entirely.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "unknown".to_string(),
            bitrate_kbps: 0,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert!(map_args.is_empty());
        assert!(codec_args.is_empty());
    }

    #[test]
    fn test_audio_multiple_streams_mixed() {
        // Two streams: one below cap (copy), one above cap (re-encode).
        let streams = vec![
            AudioStreamInfo {
                index: 0,
                codec: "ac3".to_string(),
                bitrate_kbps: 384,
            },
            AudioStreamInfo {
                index: 1,
                codec: "ac3".to_string(),
                bitrate_kbps: 768,
            },
        ];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mkv", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0", "-map", "0:a:1"]);
        assert_eq!(
            codec_args,
            vec!["-c:a:0", "copy", "-c:a:1", "ac3", "-b:a:1", "640k"]
        );
    }

    // ══════════════════════════════════════════════════════════════
    //  3. resolve_file_settings
    // ══════════════════════════════════════════════════════════════

    #[test]
    fn test_resolve_compatibility_mode() {
        let mut settings = default_settings();
        settings.compatibility_mode = true;
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "mkv", &settings, &encoders);
        assert_eq!(result.codec_family, "h264");
        assert_eq!(result.container_ext, "mp4");
        assert!(matches!(
            result.audio_strategy,
            AudioStrategy::CompatCapped { .. }
        ));
    }

    #[test]
    fn test_resolve_source_h264_stays_h264() {
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("h264", "mkv", &settings, &encoders);
        assert_eq!(result.codec_family, "h264");
    }

    #[test]
    fn test_resolve_source_hevc_stays_hevc() {
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "mkv", &settings, &encoders);
        assert_eq!(result.codec_family, "hevc");
    }

    #[test]
    fn test_resolve_source_h265_alias_stays_hevc() {
        // "h265" is an alias for "hevc".
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("h265", "mkv", &settings, &encoders);
        assert_eq!(result.codec_family, "hevc");
    }

    #[test]
    fn test_resolve_preserve_av1_with_av1_source() {
        let mut settings = default_settings();
        settings.preserve_av1 = true;
        let encoders = default_encoders();
        let result = resolve_file_settings("av1", "mkv", &settings, &encoders);
        assert_eq!(result.codec_family, "av1");
    }

    #[test]
    fn test_resolve_av1_without_preserve_defaults_hevc() {
        // Without preserve_av1, AV1 source defaults to HEVC.
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("av1", "mkv", &settings, &encoders);
        assert_eq!(result.codec_family, "hevc");
    }

    #[test]
    fn test_resolve_mpeg2_defaults_hevc() {
        // Any unrecognised codec defaults to HEVC.
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("mpeg2video", "avi", &settings, &encoders);
        assert_eq!(result.codec_family, "hevc");
    }

    #[test]
    fn test_resolve_precision_mode_forces_software() {
        let mut settings = default_settings();
        settings.precision_mode = true;
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "mkv", &settings, &encoders);
        assert_eq!(result.encoder_name, "libx265");
    }

    #[test]
    fn test_resolve_hw_encoder_preferred_when_available() {
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "mkv", &settings, &encoders);
        assert_eq!(result.encoder_name, "hevc_nvenc");
    }

    #[test]
    fn test_resolve_container_auto_from_mp4() {
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "mp4", &settings, &encoders);
        assert_eq!(result.container_ext, "mp4");
    }

    #[test]
    fn test_resolve_container_auto_from_avi() {
        // AVI → auto → MKV (default for non-mp4/mkv containers).
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "avi", &settings, &encoders);
        assert_eq!(result.container_ext, "mkv");
    }

    #[test]
    fn test_resolve_audio_strategy_default_copy_capped() {
        let settings = default_settings();
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "mkv", &settings, &encoders);
        assert!(matches!(
            result.audio_strategy,
            AudioStrategy::CopyCapped { cap_kbps: 640 }
        ));
    }

    // ══════════════════════════════════════════════════════════════
    //  4. vbr_flags / cqp_flags / crf_flags
    // ══════════════════════════════════════════════════════════════

    #[test]
    fn test_vbr_flags_libx265() {
        let flags = vbr_flags("libx265", "4M", "6M");
        assert!(flags.contains(&"-preset".to_string()));
        assert!(flags.contains(&"slow".to_string()));
        assert!(flags.contains(&"-b:v".to_string()));
        assert!(flags.contains(&"4M".to_string()));
        assert!(flags.contains(&"-maxrate".to_string()));
        assert!(flags.contains(&"6M".to_string()));
    }

    #[test]
    fn test_vbr_flags_libx264() {
        let flags = vbr_flags("libx264", "3M", "4.5M");
        assert!(flags.contains(&"-preset".to_string()));
        assert!(flags.contains(&"slow".to_string()));
        assert!(flags.contains(&"-b:v".to_string()));
        assert!(flags.contains(&"3M".to_string()));
    }

    #[test]
    fn test_vbr_flags_nvenc() {
        let flags = vbr_flags("hevc_nvenc", "4M", "6M");
        assert!(flags.contains(&"-preset".to_string()));
        assert!(flags.contains(&"p7".to_string()));
        assert!(flags.contains(&"-rc".to_string()));
        assert!(flags.contains(&"vbr".to_string()));
    }

    #[test]
    fn test_vbr_flags_amf() {
        let flags = vbr_flags("hevc_amf", "4M", "6M");
        assert!(flags.contains(&"-quality".to_string()));
        assert!(flags.contains(&"quality".to_string()));
        assert!(flags.contains(&"-rc".to_string()));
        assert!(flags.contains(&"vbr_peak".to_string()));
    }

    #[test]
    fn test_vbr_flags_qsv() {
        let flags = vbr_flags("hevc_qsv", "4M", "6M");
        assert!(flags.contains(&"-preset".to_string()));
        assert!(flags.contains(&"veryslow".to_string()));
    }

    #[test]
    fn test_vbr_flags_videotoolbox() {
        let flags = vbr_flags("hevc_videotoolbox", "4M", "6M");
        assert_eq!(flags, vec!["-b:v", "4M", "-maxrate", "6M"]);
    }

    #[test]
    fn test_vbr_flags_vaapi() {
        let flags = vbr_flags("hevc_vaapi", "4M", "6M");
        assert!(flags.contains(&"-rc_mode".to_string()));
        assert!(flags.contains(&"VBR".to_string()));
    }

    #[test]
    fn test_vbr_flags_svtav1() {
        let flags = vbr_flags("libsvtav1", "4M", "6M");
        assert!(flags.contains(&"-preset".to_string()));
        assert!(flags.contains(&"6".to_string()));
        // SVT-AV1 rejects -maxrate in VBR mode
        assert!(!flags.contains(&"-maxrate".to_string()));
    }

    #[test]
    fn test_vbr_flags_av1_nvenc() {
        // AV1 HW encoders use the same flags as their H.264/HEVC counterparts.
        let flags = vbr_flags("av1_nvenc", "4M", "6M");
        assert!(flags.contains(&"p7".to_string()));
    }

    #[test]
    fn test_vbr_flags_unknown_encoder() {
        let flags = vbr_flags("some_unknown", "4M", "6M");
        assert_eq!(flags, vec!["-b:v", "4M", "-maxrate", "6M"]);
    }

    #[test]
    fn test_cqp_flags_libx265() {
        let flags = cqp_flags("libx265", "20", "22");
        assert!(flags.contains(&"-preset".to_string()));
        assert!(flags.contains(&"slow".to_string()));
        assert!(flags.contains(&"-qp".to_string()));
        assert!(flags.contains(&"20".to_string()));
    }

    #[test]
    fn test_cqp_flags_nvenc() {
        let flags = cqp_flags("hevc_nvenc", "20", "22");
        assert!(flags.contains(&"-rc".to_string()));
        assert!(flags.contains(&"constqp".to_string()));
        assert!(flags.contains(&"-qp".to_string()));
        assert!(flags.contains(&"20".to_string()));
    }

    #[test]
    fn test_cqp_flags_amf_uses_qi_and_qp() {
        let flags = cqp_flags("hevc_amf", "20", "22");
        assert!(flags.contains(&"-qp_i".to_string()));
        assert!(flags.contains(&"20".to_string()));
        assert!(flags.contains(&"-qp_p".to_string()));
        assert!(flags.contains(&"22".to_string()));
    }

    #[test]
    fn test_cqp_flags_qsv() {
        let flags = cqp_flags("hevc_qsv", "20", "22");
        assert!(flags.contains(&"-global_quality".to_string()));
        assert!(flags.contains(&"20".to_string()));
    }

    #[test]
    fn test_cqp_flags_videotoolbox() {
        let flags = cqp_flags("hevc_videotoolbox", "20", "22");
        assert_eq!(flags, vec!["-q:v", "20"]);
    }

    #[test]
    fn test_cqp_flags_vaapi() {
        let flags = cqp_flags("hevc_vaapi", "20", "22");
        assert!(flags.contains(&"-rc_mode".to_string()));
        assert!(flags.contains(&"CQP".to_string()));
    }

    #[test]
    fn test_cqp_flags_svtav1() {
        let flags = cqp_flags("libsvtav1", "20", "22");
        assert!(flags.contains(&"-preset".to_string()));
        assert!(flags.contains(&"6".to_string()));
        assert!(!flags.contains(&"-rc".to_string()));
        assert!(flags.contains(&"-qp".to_string()));
    }

    #[test]
    fn test_crf_flags_libx265() {
        let flags = crf_flags("libx265", "18", "20", "22");
        assert!(flags.contains(&"-preset".to_string()));
        assert!(flags.contains(&"slow".to_string()));
        assert!(flags.contains(&"-crf".to_string()));
        assert!(flags.contains(&"18".to_string()));
        // Should NOT contain -qp (CRF, not CQP).
        assert!(!flags.contains(&"-qp".to_string()));
    }

    #[test]
    fn test_crf_flags_libx264() {
        let flags = crf_flags("libx264", "22", "20", "22");
        assert!(flags.contains(&"-crf".to_string()));
        assert!(flags.contains(&"22".to_string()));
    }

    #[test]
    fn test_crf_flags_svtav1() {
        let flags = crf_flags("libsvtav1", "30", "20", "22");
        assert!(flags.contains(&"-crf".to_string()));
        assert!(flags.contains(&"30".to_string()));
    }

    #[test]
    fn test_crf_flags_hw_encoder_falls_back_to_cqp() {
        // Hardware encoders do not support CRF; should fall back to CQP flags.
        let flags = crf_flags("hevc_nvenc", "18", "20", "22");
        // Should contain CQP-style flags, not CRF.
        assert!(!flags.contains(&"-crf".to_string()));
        assert!(flags.contains(&"-rc".to_string()));
        assert!(flags.contains(&"constqp".to_string()));
    }

    #[test]
    fn test_crf_flags_amf_falls_back_to_cqp() {
        let flags = crf_flags("hevc_amf", "18", "20", "22");
        assert!(!flags.contains(&"-crf".to_string()));
        assert!(flags.contains(&"-rc".to_string()));
        assert!(flags.contains(&"cqp".to_string()));
    }

    // ══════════════════════════════════════════════════════════════
    //  5. build_audio_args_from_probe — MP4 container
    // ══════════════════════════════════════════════════════════════

    #[test]
    fn test_audio_opus_below_cap_in_mp4_reencodes_eac3() {
        // Opus is not MP4-compatible, so even below the cap it must be
        // re-encoded. The fallback codec for MP4 is EAC3.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "opus".to_string(),
            bitrate_kbps: 200,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0"]);
        assert_eq!(codec_args, vec!["-c:a:0", "eac3", "-b:a:0", "640k"]);
    }

    #[test]
    fn test_audio_vorbis_below_cap_in_mp4_reencodes_eac3() {
        // Vorbis is not MP4-compatible, same behaviour as Opus above.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "vorbis".to_string(),
            bitrate_kbps: 300,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0"]);
        assert_eq!(codec_args, vec!["-c:a:0", "eac3", "-b:a:0", "640k"]);
    }

    #[test]
    fn test_audio_aac_below_cap_in_mp4_copies() {
        // AAC is MP4-compatible and below cap — should be copied.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "aac".to_string(),
            bitrate_kbps: 200,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0"]);
        assert_eq!(codec_args, vec!["-c:a:0", "copy"]);
    }

    #[test]
    fn test_audio_ac3_above_cap_in_mp4_reencodes_ac3() {
        // AC3 is MP4-compatible but above the cap — re-encode to AC3 at cap.
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "ac3".to_string(),
            bitrate_kbps: 800,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0"]);
        assert_eq!(codec_args, vec!["-c:a:0", "ac3", "-b:a:0", "640k"]);
    }

    // ══════════════════════════════════════════════════════════════
    //  6. assemble_ffmpeg_args
    // ══════════════════════════════════════════════════════════════

    #[test]
    fn test_assemble_hevc_in_mp4_has_hvc1_and_faststart() {
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mp4",
            video_args: &["-c:v", "libx265"],
            pix_fmt: "yuv420p",
            audio_map_args: &[],
            audio_codec_args: &[],
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });
        assert!(args.contains(&"-tag:v".to_string()));
        assert!(args.contains(&"hvc1".to_string()));
        assert!(args.contains(&"-movflags".to_string()));
        assert!(args.contains(&"+faststart".to_string()));
    }

    #[test]
    fn test_assemble_h264_in_mp4_no_hvc1_tag() {
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mp4",
            video_args: &["-c:v", "libx264"],
            pix_fmt: "yuv420p",
            audio_map_args: &[],
            audio_codec_args: &[],
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "h264",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });
        assert!(!args.contains(&"-tag:v".to_string()));
        assert!(!args.contains(&"hvc1".to_string()));
        // Should still have faststart for MP4.
        assert!(args.contains(&"-movflags".to_string()));
        assert!(args.contains(&"+faststart".to_string()));
    }

    #[test]
    fn test_assemble_hevc_in_mkv_no_hvc1_or_movflags() {
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "libx265"],
            pix_fmt: "yuv420p",
            audio_map_args: &[],
            audio_codec_args: &[],
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });
        assert!(!args.contains(&"-tag:v".to_string()));
        assert!(!args.contains(&"hvc1".to_string()));
        assert!(!args.contains(&"-movflags".to_string()));
        assert!(!args.contains(&"+faststart".to_string()));
    }

    #[test]
    fn test_assemble_mp4_subtitles_use_mov_text() {
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mp4",
            video_args: &["-c:v", "libx265"],
            pix_fmt: "yuv420p",
            audio_map_args: &[],
            audio_codec_args: &[],
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });
        assert!(args.contains(&"-c:s".to_string()));
        assert!(args.contains(&"mov_text".to_string()));
        // The subtitle codec value following -c:s must not be "copy".
        let cs_positions: Vec<usize> = args
            .iter()
            .enumerate()
            .filter(|(_, a)| *a == "-c:s")
            .map(|(i, _)| i)
            .collect();
        for pos in cs_positions {
            assert_ne!(args[pos + 1], "copy");
        }
    }

    #[test]
    fn test_assemble_mkv_subtitles_use_copy() {
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "libx265"],
            pix_fmt: "yuv420p",
            audio_map_args: &[],
            audio_codec_args: &[],
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });
        assert!(args.contains(&"-c:s".to_string()));
        assert!(args.contains(&"copy".to_string()));
        // Should not contain "mov_text".
        assert!(!args.contains(&"mov_text".to_string()));
    }

    /// HDR→SDR tonemap must emit a -vf filter before -pix_fmt when
    /// re-encoding. Regression test for the HDR H.264 batch bug where
    /// the tonemap filter was paired with -c:v copy and ffmpeg refused
    /// with EINVAL (exit 234).
    #[test]
    fn test_assemble_tonemap_with_reencode_emits_vf_filter() {
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "libx265"],
            pix_fmt: "yuv420p",
            audio_map_args: &[],
            audio_codec_args: &[],
            is_image_source: false,
            threads: 0,
            tonemap_filter: Some(TONEMAP_HABLE),
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });
        assert!(args.contains(&"-vf".to_string()));
        assert!(args.iter().any(|a| a.contains("tonemap=hable")));
        // -vf must precede -pix_fmt so the tonemap runs before format
        // conversion.
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let pix_pos = args.iter().position(|a| a == "-pix_fmt").unwrap();
        assert!(vf_pos < pix_pos);
    }

    /// The assemble helper must refuse -c:v copy paired with a tonemap
    /// filter. ffmpeg rejects this combination at runtime (EINVAL), so
    /// the debug_assert is a defence-in-depth guard against future
    /// regressions where a caller forgets to override a Copy decision
    /// when tonemapping is required.
    #[test]
    #[should_panic(expected = "tonemap_filter is incompatible with -c:v copy")]
    fn test_assemble_tonemap_with_copy_panics() {
        let _ = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "copy"],
            pix_fmt: "yuv420p",
            audio_map_args: &[],
            audio_codec_args: &[],
            is_image_source: false,
            threads: 0,
            tonemap_filter: Some(TONEMAP_HABLE),
            codec_family: "h264",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });
    }

    // ══════════════════════════════════════════════════════════════
    //  6. Compat mode invariants
    // ══════════════════════════════════════════════════════════════

    /// Compat mode MUST produce CompatCapped regardless of audio_encoder setting.
    #[test]
    fn test_compat_mode_forces_compat_capped_for_all_audio_encoders() {
        let audio_values = ["auto", "", "copy", "aac", "ac3", "eac3", "opus", "flac"];
        let encoders = default_encoders();
        for audio in &audio_values {
            let mut settings = default_settings();
            settings.compatibility_mode = true;
            settings.audio_encoder = audio.to_string();
            let result = resolve_file_settings("hevc", "mkv", &settings, &encoders);
            assert!(
                matches!(result.audio_strategy, AudioStrategy::CompatCapped { .. }),
                "audio_encoder={:?} should produce CompatCapped in compat mode, got {:?}",
                audio,
                result.audio_strategy,
            );
        }
    }

    /// Without compat mode, "copy" should produce CopyCapped.
    #[test]
    fn test_no_compat_copy_produces_copy_capped() {
        let mut settings = default_settings();
        settings.audio_encoder = "copy".to_string();
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "mkv", &settings, &encoders);
        assert!(matches!(
            result.audio_strategy,
            AudioStrategy::CopyCapped { .. }
        ));
    }

    /// Without compat mode, explicit "aac" should produce CompatCapped.
    #[test]
    fn test_no_compat_explicit_aac_produces_compat_capped() {
        let mut settings = default_settings();
        settings.audio_encoder = "aac".to_string();
        let encoders = default_encoders();
        let result = resolve_file_settings("hevc", "mkv", &settings, &encoders);
        assert!(matches!(
            result.audio_strategy,
            AudioStrategy::CompatCapped { .. }
        ));
    }

    /// Compat mode MUST produce h264 codec and mp4 container regardless of
    /// codec_family, output_container, and source codec settings.
    #[test]
    fn test_compat_mode_forces_h264_mp4_across_settings() {
        let containers = ["auto", "mkv", "mp4"];
        let source_codecs = ["hevc", "h264", "av1", "mpeg2video"];
        let encoders = default_encoders();
        for container in &containers {
            for source in &source_codecs {
                let mut settings = default_settings();
                settings.compatibility_mode = true;
                settings.output_container = container.to_string();
                let result = resolve_file_settings(source, "mkv", &settings, &encoders);
                assert_eq!(
                    result.container_ext, "mp4",
                    "compat mode with container={}, source={} should force mp4",
                    container, source,
                );
                assert_eq!(
                    result.codec_family, "h264",
                    "compat mode with source={} should force h264",
                    source,
                );
            }
        }
    }

    // ══════════════════════════════════════════════════════════════
    //  7. Composed: resolve_file_settings + build_audio_args_from_probe
    // ══════════════════════════════════════════════════════════════

    /// Compat mode with DTS audio and explicit --audio copy: resolve should
    /// still pick CompatCapped, which makes build_audio_args re-encode to AAC.
    #[test]
    fn test_composed_compat_dts_becomes_aac() {
        let mut settings = default_settings();
        settings.compatibility_mode = true;
        settings.audio_encoder = "copy".to_string();
        let encoders = default_encoders();
        let resolved = resolve_file_settings("hevc", "mkv", &settings, &encoders);

        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "dts".to_string(),
            bitrate_kbps: 1509,
        }];
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(
            &streams,
            &resolved.audio_strategy,
            &resolved.container_ext,
            &sink,
        );
        assert_eq!(codec_args[1], "aac");
    }

    /// Compat mode with TrueHD audio: same pipeline, AAC output.
    #[test]
    fn test_composed_compat_truehd_becomes_aac() {
        let mut settings = default_settings();
        settings.compatibility_mode = true;
        let encoders = default_encoders();
        let resolved = resolve_file_settings("hevc", "mkv", &settings, &encoders);

        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "truehd".to_string(),
            bitrate_kbps: 4000,
        }];
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(
            &streams,
            &resolved.audio_strategy,
            &resolved.container_ext,
            &sink,
        );
        assert_eq!(codec_args[1], "aac");
    }

    /// Non-compat mode with DTS audio in MKV: CopyCapped, DTS copies
    /// (below cap, MKV-compatible).
    #[test]
    fn test_composed_no_compat_dts_copies_in_mkv() {
        let settings = default_settings();
        let encoders = default_encoders();
        let resolved = resolve_file_settings("hevc", "mkv", &settings, &encoders);

        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "dts".to_string(),
            bitrate_kbps: 448,
        }];
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(
            &streams,
            &resolved.audio_strategy,
            &resolved.container_ext,
            &sink,
        );
        assert_eq!(codec_args, vec!["-c:a:0", "copy"]);
    }

    // ══════════════════════════════════════════════════════════════
    //  8. Remux fallback audio scenarios
    // ══════════════════════════════════════════════════════════════

    /// Simulate remux fallback in compat mode: DTS source → MP4.
    /// CompatCapped should re-encode to AAC, never copy DTS into MP4.
    #[test]
    fn test_remux_compat_dts_in_mp4_reencodes() {
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "dts".to_string(),
            bitrate_kbps: 1509,
        }];
        let strategy = AudioStrategy::CompatCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0"]);
        assert_eq!(codec_args[1], "aac");
        assert!(!codec_args.contains(&"copy".to_string()));
    }

    /// Simulate remux fallback in compat mode: TrueHD source → MP4.
    #[test]
    fn test_remux_compat_truehd_in_mp4_reencodes() {
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "truehd".to_string(),
            bitrate_kbps: 4000,
        }];
        let strategy = AudioStrategy::CompatCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(codec_args[1], "aac");
    }

    /// Simulate remux fallback with CopyCapped + DTS in MP4.
    /// DTS is not MP4-compatible, so CopyCapped should fall back to EAC3.
    #[test]
    fn test_remux_no_compat_dts_in_mp4_falls_back_to_eac3() {
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "dts".to_string(),
            bitrate_kbps: 1509,
        }];
        let strategy = AudioStrategy::CopyCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(codec_args[1], "eac3");
    }

    /// Remux with AAC in compat mode: AAC below cap should be copied.
    #[test]
    fn test_remux_compat_aac_below_cap_copies() {
        let streams = vec![AudioStreamInfo {
            index: 0,
            codec: "aac".to_string(),
            bitrate_kbps: 256,
        }];
        let strategy = AudioStrategy::CompatCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (_, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(codec_args, vec!["-c:a:0", "copy"]);
    }

    /// Multi-stream remux: mixed codecs in compat mode + MP4.
    #[test]
    fn test_remux_compat_mixed_streams_mp4() {
        let streams = vec![
            AudioStreamInfo {
                index: 0,
                codec: "aac".to_string(),
                bitrate_kbps: 256,
            },
            AudioStreamInfo {
                index: 1,
                codec: "dts".to_string(),
                bitrate_kbps: 1509,
            },
        ];
        let strategy = AudioStrategy::CompatCapped { cap_kbps: 640 };
        let sink = NoopSink;
        let (map_args, codec_args) = build_audio_args_from_probe(&streams, &strategy, "mp4", &sink);
        assert_eq!(map_args, vec!["-map", "0:a:0", "-map", "0:a:1"]);
        // Stream 0: AAC below cap → copy
        assert_eq!(codec_args[0], "-c:a:0");
        assert_eq!(codec_args[1], "copy");
        // Stream 1: DTS → AAC re-encode
        assert_eq!(codec_args[2], "-c:a:1");
        assert_eq!(codec_args[3], "aac");
    }

    // ══════════════════════════════════════════════════════════════
    //  validate_output_folder (#4c)
    // ══════════════════════════════════════════════════════════════

    #[test]
    fn test_validate_output_folder_creates_missing_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let nested = tmp.path().join("sub").join("dir");
        assert!(!nested.exists());
        validate_output_folder(&nested).expect("should create and validate");
        assert!(nested.is_dir());
        // Probe file must have been cleaned up.
        assert!(!nested.join(".histv_write_test").exists());
    }

    #[test]
    fn test_validate_output_folder_accepts_existing_writable_dir() {
        let tmp = tempfile::tempdir().unwrap();
        validate_output_folder(tmp.path()).expect("existing writable dir should pass");
        assert!(!tmp.path().join(".histv_write_test").exists());
    }

    // Note: a "read-only dir is rejected" test would be ideal, but
    // filesystem DAC checks are bypassed by root (which CI may run as
    // on some platforms) and by filesystems without permission
    // semantics. The existing Ok-path tests cover the happy path; the
    // writability probe is a simple write-then-remove which any std
    // lib regression would surface broadly.

    // ══════════════════════════════════════════════════════════════
    //  find_dri_render_node_in (hardware device detection)
    // ══════════════════════════════════════════════════════════════

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_dri_render_node_found() {
        let tmp = tempfile::tempdir().unwrap();
        // Create a fake render node entry
        std::fs::File::create(tmp.path().join("renderD128")).unwrap();
        let result = find_dri_render_node_in(tmp.path());
        assert_eq!(result, Some(format!("{}/renderD128", tmp.path().display())));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_dri_render_node_picks_first_numeric() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::File::create(tmp.path().join("renderD129")).unwrap();
        std::fs::File::create(tmp.path().join("renderD128")).unwrap();
        std::fs::File::create(tmp.path().join("renderD130")).unwrap();
        let result = find_dri_render_node_in(tmp.path());
        // The lowest number wins whatever the read_dir order.
        assert_eq!(result, Some(format!("{}/renderD128", tmp.path().display())));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn output_with_timeout_kills_hung_process() {
        let mut cmd = tokio::process::Command::new("sleep");
        cmd.arg("30");
        let start = std::time::Instant::now();
        let out = output_with_timeout(cmd, std::time::Duration::from_millis(200)).await;
        assert!(out.is_none());
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
        // A process reading stdin sees end-of-file, not a hang.
        let mut cat = tokio::process::Command::new("cat");
        cat.stdout(std::process::Stdio::piped());
        let out = output_with_timeout(cat, std::time::Duration::from_secs(5)).await;
        assert!(out.is_some_and(|o| o.status.success()));
    }

    /// Runs every CQP arm this machine can encode with the ffmpeg in
    /// `HISTV_TEST_FFMPEG` and fails on a non-zero exit or an option ffmpeg
    /// ignores ("has not been used"). Also covers `-bf 0` on the hardware
    /// HEVC encoders, which the HDR10+ injector route adds.
    /// Run: `HISTV_TEST_FFMPEG=/path/to/ffmpeg cargo test --lib -- --ignored cqp_arms_honoured`
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore = "needs HISTV_TEST_FFMPEG and a GPU render node"]
    fn cqp_arms_honoured_by_real_ffmpeg() {
        let ffmpeg = std::env::var("HISTV_TEST_FFMPEG").expect("set HISTV_TEST_FFMPEG");
        let out_dir = tempfile::tempdir().unwrap();
        let mut failures = Vec::new();
        let cases: &[(&str, &[&str])] = &[
            ("hevc_vaapi", &[]),
            ("h264_vaapi", &[]),
            ("av1_vaapi", &[]),
            ("hevc_vulkan", &[]),
            ("h264_vulkan", &[]),
            ("av1_vulkan", &[]),
            ("libx265", &[]),
            ("libx264", &[]),
            ("libsvtav1", &[]),
            ("hevc_vaapi", &["-bf", "0"]),
            ("hevc_vulkan", &["-bf", "0"]),
        ];
        for (enc, extra) in cases {
            let node = find_dri_render_node_for(enc);
            let hw = hw_device_args(enc, "nv12", false, node.as_deref(), TargetOs::Linux);
            let out = out_dir.path().join(format!("{enc}.mkv"));
            let mut args: Vec<String> = vec!["-hide_banner".into(), "-y".into()];
            args.extend(hw.pre_input.iter().cloned());
            args.extend(strs(&[
                "-f",
                "lavfi",
                "-i",
                "testsrc2=s=320x240:r=24",
                "-frames:v",
                "24",
            ]));
            if let Some(f) = &hw.upload_filter {
                args.extend(["-vf".to_string(), f.clone()]);
            }
            args.extend(strs(&["-c:v", enc]));
            args.extend(cqp_flags(enc, "20", "22"));
            args.extend(extra.iter().map(|x| x.to_string()));
            args.push(out.to_string_lossy().into_owned());
            let o = std::process::Command::new(&ffmpeg)
                .args(&args)
                .stdin(std::process::Stdio::null())
                .output()
                .unwrap();
            let err = String::from_utf8_lossy(&o.stderr);
            if !o.status.success() || err.contains("has not been used") {
                let line = err
                    .lines()
                    .find(|l| l.contains("has not been used") || l.contains("rror"))
                    .unwrap_or("");
                failures.push(format!("{enc} {extra:?}: {} {line}", o.status));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    /// Runs `ffmpeg` and returns (exit ok, stderr).
    #[cfg(target_os = "linux")]
    fn run_ff(ffmpeg: &str, args: &[String]) -> (bool, String) {
        let o = std::process::Command::new(ffmpeg)
            .args(args)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    }

    /// Size of the first decoded frame (`frame`) or of the stream as the
    /// container reports it (`stream`).
    #[cfg(target_os = "linux")]
    fn probed_size(ffmpeg: &str, path: &std::path::Path, what: &str) -> String {
        let probe = std::path::Path::new(ffmpeg).with_file_name("ffprobe");
        let o = std::process::Command::new(probe)
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-read_intervals",
                "%+#1",
            ])
            .args([
                "-show_entries",
                &format!("{what}=width,height"),
                "-of",
                "csv=p=0",
            ])
            .arg(path)
            .output()
            .unwrap();
        String::from_utf8_lossy(&o.stdout)
            .trim()
            .trim_end_matches(',')
            .to_string()
    }

    /// Needs `HISTV_TEST_FFMPEG` (ffprobe beside it) and a VAAPI render node.
    /// hevc_vaapi software frames with the size fix that ffmpeg supports
    /// decode at the source size, in both MKV (hvcC plus in-band SPS) and
    /// raw HEVC. With `width`/`height`, the "driver already right" branch: a
    /// libx265 stream with a conformance window passes through the BSF
    /// byte for byte.
    #[cfg(target_os = "linux")]
    #[test]
    #[ignore]
    fn hevc_vaapi_size_fix_real_ffmpeg() {
        let ffmpeg = std::env::var("HISTV_TEST_FFMPEG").expect("set HISTV_TEST_FFMPEG");
        let dir = tempfile::tempdir().unwrap();
        let node = find_dri_render_node_for("hevc_vaapi").expect("render node");
        let help = std::process::Command::new(&ffmpeg)
            .args(["-hide_banner", "-h", "bsf=hevc_metadata"])
            .output()
            .unwrap();
        let has_size = bsf_help_has_size(&String::from_utf8_lossy(&help.stdout));
        let mut failures = Vec::new();
        for (w, h, pix) in [
            (1920, 1080, "p010"),
            (640, 360, "nv12"),
            (1000, 562, "nv12"),
        ] {
            for ext in ["mkv", "h265"] {
                let out = dir.path().join(format!("{w}x{h}.{ext}"));
                let mut args = strs(&["-v", "error", "-y", "-vaapi_device", &node, "-f", "lavfi"]);
                args.extend(strs(&["-i", &format!("testsrc2=s={w}x{h}:d=0.2")]));
                let fix = hevc_vaapi_size_fix("hevc_vaapi", w, h, has_size).unwrap();
                let vf = match &fix.pad {
                    Some(pad) => format!("{pad},format={pix},hwupload"),
                    None => format!("format={pix},hwupload"),
                };
                args.extend(strs(&[
                    "-vf",
                    &vf,
                    "-c:v",
                    "hevc_vaapi",
                    "-bsf:v",
                    &fix.bsf,
                ]));
                args.push(out.to_string_lossy().into_owned());
                let (ok, err) = run_ff(&ffmpeg, &args);
                let want = format!("{w},{h}");
                let frame = probed_size(&ffmpeg, &out, "frame");
                let stream = probed_size(&ffmpeg, &out, "stream");
                if !ok || frame != want || stream != want {
                    failures.push(format!(
                        "{w}x{h} {pix} {ext}: frame {frame} stream {stream} {err}"
                    ));
                }
            }
        }
        if !has_size {
            assert!(failures.is_empty(), "{failures:#?}");
            return;
        }
        let x265 = dir.path().join("x265.h265");
        let mut args = strs(&[
            "-v",
            "error",
            "-y",
            "-f",
            "lavfi",
            "-i",
            "testsrc2=s=1000x562:d=0.2",
        ]);
        args.extend(strs(&[
            "-c:v",
            "libx265",
            "-x265-params",
            "log-level=error",
        ]));
        args.push(x265.to_string_lossy().into_owned());
        assert!(run_ff(&ffmpeg, &args).0);
        let copied = dir.path().join("copied.h265");
        let mut args = strs(&[
            "-v",
            "error",
            "-y",
            "-i",
            &x265.to_string_lossy(),
            "-c",
            "copy",
        ]);
        args.extend(strs(&[
            "-bsf:v",
            &hevc_vaapi_size_fix("hevc_vaapi", 1000, 562, true)
                .unwrap()
                .bsf,
        ]));
        args.push(copied.to_string_lossy().into_owned());
        assert!(run_ff(&ffmpeg, &args).0);
        if std::fs::read(&x265).unwrap() != std::fs::read(&copied).unwrap() {
            failures.push("BSF changed a stream whose window was already right".into());
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }

    #[test]
    fn hevc_vaapi_size_fix_decision() {
        let fix = |w, h, has_size| hevc_vaapi_size_fix("hevc_vaapi", w, h, has_size);
        let both = |pad: &str, bsf: &str| HevcVaapiSizeFix {
            pad: Some(pad.into()),
            bsf: bsf.into(),
        };
        // BSF with width/height.
        assert_eq!(
            fix(1920, 1080, true),
            Some(HevcVaapiSizeFix {
                pad: None,
                bsf: "hevc_metadata=width=1920:height=1080".into(),
            })
        );
        assert_eq!(
            fix(360, 640, true).map(|f| f.bsf),
            Some("hevc_metadata=width=360:height=640".into())
        );
        // Older ffmpeg: pad to 64 multiples, crop the pad.
        assert_eq!(
            fix(1920, 1080, false),
            Some(both(
                "pad=1920:1088",
                "hevc_metadata=crop_right=0:crop_bottom=8"
            ))
        );
        assert_eq!(
            fix(640, 360, false),
            Some(both(
                "pad=640:384",
                "hevc_metadata=crop_right=0:crop_bottom=24"
            ))
        );
        assert_eq!(
            fix(1000, 562, false),
            Some(both(
                "pad=1024:576",
                "hevc_metadata=crop_right=24:crop_bottom=14"
            ))
        );
        assert_eq!(fix(1280, 640, false), None);
        // Other encoders and sizes that 4:2:0 crop units cannot express.
        for enc in [
            "hevc_vulkan",
            "h264_vaapi",
            "av1_vaapi",
            "libx265",
            "hevc_qsv",
        ] {
            for has_size in [true, false] {
                assert_eq!(
                    hevc_vaapi_size_fix(enc, 1920, 1080, has_size),
                    None,
                    "{enc}"
                );
            }
        }
        for has_size in [true, false] {
            assert_eq!(fix(321, 240, has_size), None);
            assert_eq!(fix(320, 241, has_size), None);
            assert_eq!(fix(0, 0, has_size), None);
        }
        assert!(!vaapi_device_decode_ok("hevc_vaapi", false));
        assert!(vaapi_device_decode_ok("hevc_vaapi", true));
        assert!(vaapi_device_decode_ok("av1_vaapi", false));
    }

    #[test]
    fn parse_bsf_help_has_size() {
        let new = "  -crop_bottom       <int>        ...V....B.. Set bottom border crop offset (from -1 to 16888) (default -1)\n  -width             <int>        ...V....B.. Set width after crop (from -1 to 16888) (default -1)\n  -height            <int>        ...V....B.. Set height after crop (from -1 to 16888) (default -1)\n";
        assert!(bsf_help_has_size(new));
        let old = "  -crop_right        <int>        ...V....B.. Set right border crop offset (from -1 to 16888) (default -1)\n  -crop_bottom       <int>        ...V....B.. Set bottom border crop offset (from -1 to 16888) (default -1)\n  -level             <int>        ...V....B.. Set level (table A.6) (from -2 to 255) (default -2)\n";
        assert!(!bsf_help_has_size(old));
        assert!(!bsf_help_has_size(""));
    }

    #[test]
    fn assemble_image_source_uploads_for_hardware() {
        let empty: &[String] = &[];
        let parts = |enc: &'static [&'static str]| FfmpegAssemblyParts {
            input_path: "in.gif",
            output_path: "out.mkv",
            video_args: enc,
            pix_fmt: "yuv420p",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: true,
            codec_family: "hevc",
            vaapi_hevc_size: Some((322, 242)),
            bsf_has_size: true,
            ..Default::default()
        };
        let node = Some("/dev/dri/renderD128");
        let vf = |a: &[String]| a[a.iter().position(|x| x == "-vf").unwrap() + 1].clone();

        let a = assemble_ffmpeg_args_for(&parts(&["-c:v", "hevc_vaapi"]), node, TargetOs::Linux);
        assert_eq!(vf(&a), "pad=ceil(iw/2)*2:ceil(ih/2)*2,format=nv12,hwupload");
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-vaapi_device" && w[1] == "/dev/dri/renderD128"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-bsf:v" && w[1].contains("width=322")));
        assert!(a.iter().any(|x| x == "-an"));

        let a = assemble_ffmpeg_args_for(&parts(&["-c:v", "hevc_vulkan"]), node, TargetOs::Linux);
        assert_eq!(vf(&a), "pad=ceil(iw/2)*2:ceil(ih/2)*2,format=nv12,hwupload");
        assert!(!a.iter().any(|x| x == "-pix_fmt"));

        let a = assemble_ffmpeg_args_for(&parts(&["-c:v", "libx265"]), node, TargetOs::Linux);
        assert_eq!(vf(&a), "pad=ceil(iw/2)*2:ceil(ih/2)*2");
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-pix_fmt" && w[1] == "yuv420p"));
        assert!(!a
            .iter()
            .any(|x| x == "-vaapi_device" || x == "-init_hw_device"));
    }

    #[test]
    fn select_render_node_qsv_prefers_intel() {
        let nodes = vec![
            (128, "renderD128".to_string(), Some(0x1002)),
            (129, "renderD129".to_string(), Some(PCI_VENDOR_INTEL)),
        ];
        // QSV: the Intel iGPU, not the lower-numbered AMD card.
        assert_eq!(
            select_render_node(&nodes, Some(PCI_VENDOR_INTEL)),
            Some("renderD129")
        );
        // VAAPI and Vulkan: the lowest node.
        assert_eq!(select_render_node(&nodes, None), Some("renderD128"));
        // No Intel GPU (or unreadable vendor): the lowest node, as before.
        let amd_only = vec![
            (129, "renderD129".to_string(), Some(0x1002)),
            (128, "renderD128".to_string(), None),
        ];
        assert_eq!(
            select_render_node(&amd_only, Some(PCI_VENDOR_INTEL)),
            Some("renderD128")
        );
        assert_eq!(select_render_node(&[], None), None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn find_render_node_reads_sysfs_vendor() {
        let dev = tempfile::tempdir().unwrap();
        let sys = tempfile::tempdir().unwrap();
        for (node, vendor) in [("renderD128", "0x1002\n"), ("renderD129", "0x8086\n")] {
            std::fs::File::create(dev.path().join(node)).unwrap();
            let d = sys.path().join(node).join("device");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("vendor"), vendor).unwrap();
        }
        let pick = |v| find_dri_render_node_for_vendor(dev.path(), sys.path(), v);
        assert_eq!(
            pick(Some(PCI_VENDOR_INTEL)),
            Some(format!("{}/renderD129", dev.path().display()))
        );
        assert_eq!(
            pick(None),
            Some(format!("{}/renderD128", dev.path().display()))
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_dri_render_node_skips_card_nodes() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::File::create(tmp.path().join("card0")).unwrap();
        std::fs::File::create(tmp.path().join("controlD64")).unwrap();
        let result = find_dri_render_node_in(tmp.path());
        assert_eq!(
            result, None,
            "card/control entries must not be treated as render nodes"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_dri_render_node_empty_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let result = find_dri_render_node_in(tmp.path());
        assert_eq!(result, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_dri_render_node_nonexistent_dir() {
        let result = find_dri_render_node_in(std::path::Path::new(
            "/nonexistent/path/that/does/not/exist",
        ));
        assert_eq!(result, None);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn test_find_dri_render_node_mixed_entries() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::File::create(tmp.path().join("card0")).unwrap();
        std::fs::File::create(tmp.path().join("renderD128")).unwrap();
        std::fs::File::create(tmp.path().join("controlD64")).unwrap();
        let result = find_dri_render_node_in(tmp.path());
        assert!(result.is_some());
        assert!(result.unwrap().ends_with("renderD128"));
    }

    // ══════════════════════════════════════════════════════════════
    //  hw_device_init / hw_upload_filter in assemble_ffmpeg_args
    // ══════════════════════════════════════════════════════════════

    /// QSV encoder must emit `-init_hw_device qsv=qs` and
    /// `-filter_hw_device qs` before `-i`, and the hwupload filter
    /// must appear in `-vf`. Skipped when no DRI render node exists
    /// (e.g. CI without a GPU); the no-render-node path is covered
    /// by `test_assemble_qsv_missing_render_node_emits_sentinel`.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_qsv_emits_hw_device_init_and_upload() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_qsv"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        // hw_device_init args must precede -i
        let hw_init_pos = args.iter().position(|a| a == "-init_hw_device").unwrap();
        let input_pos = args.iter().position(|a| a == "-i").unwrap();
        assert!(
            hw_init_pos < input_pos,
            "-init_hw_device must come before -i"
        );

        // Verify full QSV init chain
        assert_eq!(args[hw_init_pos + 1], "qsv=qs");
        let filter_hw_pos = args.iter().position(|a| a == "-filter_hw_device").unwrap();
        assert_eq!(args[filter_hw_pos + 1], "qs");

        // hwupload filter must be present in -vf
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        assert!(args[vf_pos + 1].contains("hwupload=extra_hw_frames=64"));
    }

    /// QSV must always emit `-init_hw_device`. When no render node exists the
    /// value is a sentinel so ffmpeg fails loudly rather than silently.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_qsv_missing_render_node_emits_sentinel() {
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_qsv"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });
        assert!(
            args.iter().any(|a| a == "-init_hw_device"),
            "QSV encode must always include -init_hw_device"
        );
        let hw_init_pos = args.iter().position(|a| a == "-init_hw_device").unwrap();
        let init_value = &args[hw_init_pos + 1];
        assert!(
            init_value == "qsv=qs" || init_value == "qsv=qs_child_device_missing",
            "unexpected QSV init device value: {init_value}"
        );
    }

    /// VAAPI encoder must emit `-vaapi_device` before `-i` and
    /// `format=nv12,hwupload` as `-vf`. This test only runs on
    /// systems where `/dev/dri` exists (i.e. Linux with a GPU);
    /// the no-render-node path is covered separately below.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_vaapi_emits_hw_device_init_and_upload() {
        // Skip if no DRI render node available (e.g. CI without GPU)
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }

        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        // -vaapi_device must precede -i
        let vaapi_dev_pos = args.iter().position(|a| a == "-vaapi_device").unwrap();
        let input_pos = args.iter().position(|a| a == "-i").unwrap();
        assert!(
            vaapi_dev_pos < input_pos,
            "-vaapi_device must come before -i"
        );

        // hwupload filter in -vf
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        assert!(args[vf_pos + 1].contains("hwupload"));
    }

    /// When no render node exists for VAAPI, the args must still
    /// contain `-vaapi_device` (with a clearly-missing path) so
    /// ffmpeg fails fast rather than silently producing broken output.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_vaapi_missing_render_node_still_emits_vaapi_device() {
        // This test verifies the fallback path where find_dri_render_node()
        // returns None. We cannot control the real /dev/dri from a test, so
        // we verify the *invariant*: if the encoder is _vaapi, the args must
        // contain -vaapi_device regardless of whether a real node exists.
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        // VAAPI must always have -vaapi_device in args. If the render
        // node is missing, we emit a sentinel path so ffmpeg fails
        // loudly instead of silently producing broken output.
        assert!(
            args.iter().any(|a| a == "-vaapi_device"),
            "VAAPI encode must always include -vaapi_device (even with missing render node)"
        );
    }

    /// VAAPI with 10-bit + tonemap: tonemap forces frames back to CPU,
    /// so hwaccel decode can't be used. Falls back to -vaapi_device +
    /// format=nv12,hwupload (tonemap output is 8-bit yuv420p anyway).
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_vaapi_10bit_tonemap_uses_vaapi_device() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }

        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "p010le",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: Some(TONEMAP_HABLE),
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        // When tonemap is present, can't use hwaccel decode (tonemap is CPU)
        assert!(
            !args.iter().any(|a| a == "-hwaccel"),
            "VAAPI with tonemap must not use hwaccel decode"
        );
        // Must use -vaapi_device instead
        assert!(
            args.iter().any(|a| a == "-vaapi_device"),
            "VAAPI with tonemap must use -vaapi_device"
        );
        // Must have hwupload after tonemap
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let vf_value = &args[vf_pos + 1];
        assert!(vf_value.contains("hwupload"));
        assert!(vf_value.contains("tonemap"));
    }

    /// Software encoder must NOT emit any hw_device_init or hwupload.
    #[test]
    fn test_assemble_software_encoder_no_hw_init() {
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "libx265"],
            pix_fmt: "yuv420p",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        assert!(!args.iter().any(|a| a == "-vaapi_device"));
        assert!(!args.iter().any(|a| a == "-init_hw_device"));
        assert!(!args.iter().any(|a| a == "-filter_hw_device"));
        assert!(!args.iter().any(|a| a == "hwupload"));
    }

    /// NVENC must NOT emit hw_device_init (it auto-detects) and
    /// must NOT emit hwupload filter.
    #[test]
    fn test_assemble_nvenc_no_hw_init() {
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_nvenc"],
            pix_fmt: "yuv420p",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        assert!(!args.iter().any(|a| a == "-vaapi_device"));
        assert!(!args.iter().any(|a| a == "-init_hw_device"));
        assert!(!args.iter().any(|a| a == "hwupload"));
    }

    /// QSV encoder with tonemap: the hwupload filter must be chained
    /// after the tonemap filter, not replace it. QSV hwupload is emitted
    /// only by the Linux `assemble_ffmpeg_args` branch and requires a DRI
    /// render node, so the test is gated accordingly.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_qsv_tonemap_chains_hwupload() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_qsv"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: Some(TONEMAP_HABLE),
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let vf_value = &args[vf_pos + 1];

        assert!(
            vf_value.contains("tonemap=hable"),
            "tonemap must be present"
        );
        assert!(vf_value.contains("hwupload"), "hwupload must be present");
        let tonemap_idx = vf_value.find("tonemap").unwrap();
        let hwupload_idx = vf_value.find("hwupload").unwrap();
        assert!(
            tonemap_idx < hwupload_idx,
            "tonemap must come before hwupload so it runs on CPU first"
        );
    }

    /// VAAPI encoder with tonemap: the hwupload filter must be
    /// chained after the tonemap filter. Skipped if no /dev/dri.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_vaapi_tonemap_chains_hwupload() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }

        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: Some(TONEMAP_HABLE),
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let vf_value = &args[vf_pos + 1];

        assert!(vf_value.contains("tonemap=hable"));
        assert!(vf_value.contains("hwupload"));
        let tonemap_idx = vf_value.find("tonemap").unwrap();
        let hwupload_idx = vf_value.find("hwupload").unwrap();
        assert!(tonemap_idx < hwupload_idx);
    }

    // ══════════════════════════════════════════════════════════════
    //  10-bit (HDR/DV) hwupload format: p010le instead of nv12
    // ══════════════════════════════════════════════════════════════

    /// VAAPI with 10-bit HDR/DV source (no tonemap) must use hwaccel
    /// decode instead of format=p010le,hwupload, because the hwupload
    /// path silently produces green frames on AMD VAAPI. The args must
    /// contain -hwaccel vaapi, -hwaccel_device, and -hwaccel_output_format,
    /// must use scale_vaapi=format=p010 to force P010 surface format, and must
    /// NOT contain hwupload or -pix_fmt.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_vaapi_10bit_uses_hwaccel_decode() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }

        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "p010le",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: true,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        // Must use hwaccel decode for 10-bit VAAPI
        let hwaccel_pos = args.iter().position(|a| a == "-hwaccel").unwrap();
        assert_eq!(args[hwaccel_pos + 1], "vaapi");
        let hwaccel_dev_pos = args.iter().position(|a| a == "-hwaccel_device").unwrap();
        assert!(args[hwaccel_dev_pos + 1].contains("renderD"));
        let hwaccel_fmt_pos = args
            .iter()
            .position(|a| a == "-hwaccel_output_format")
            .unwrap();
        assert_eq!(args[hwaccel_fmt_pos + 1], "vaapi");

        // Must use scale_vaapi to force P010 surface format
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        assert!(
            args[vf_pos + 1].contains("scale_vaapi=format=p010"),
            "10-bit VAAPI hwaccel must use scale_vaapi=format=p010, got: {}",
            args[vf_pos + 1]
        );

        // Must NOT use hwupload or -pix_fmt (encoder auto-negotiates)
        assert!(
            !args.iter().any(|a| a == "hwupload"),
            "10-bit VAAPI hwaccel must not use hwupload"
        );
        assert!(
            !args.iter().any(|a| a == "-pix_fmt"),
            "10-bit VAAPI hwaccel must not set -pix_fmt (encoder auto-negotiates)"
        );

        // Must NOT use old -vaapi_device approach
        assert!(
            !args.iter().any(|a| a == "-vaapi_device"),
            "10-bit VAAPI must use -hwaccel vaapi, not -vaapi_device"
        );
    }

    /// VAAPI with 8-bit SDR source must use format=nv12,hwupload.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_vaapi_8bit_uses_nv12() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }

        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let vf_value = &args[vf_pos + 1];
        assert!(
            vf_value.contains("nv12"),
            "8-bit VAAPI must use format=nv12,hwupload, got: {vf_value}"
        );
    }

    /// QSV with 10-bit HDR/DV source must use format=p010le,hwupload.
    /// QSV args are emitted only by the Linux branch of
    /// `assemble_ffmpeg_args` and require a DRI render node.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_qsv_10bit_uses_p010le() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_qsv"],
            pix_fmt: "p010le",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let vf_value = &args[vf_pos + 1];
        assert!(
            vf_value.contains("p010le"),
            "10-bit QSV must use format=p010le,hwupload, got: {vf_value}"
        );
        assert!(
            !vf_value.contains("nv12"),
            "10-bit QSV must not use format=nv12, got: {vf_value}"
        );
    }

    /// QSV with 8-bit SDR source must use format=nv12,hwupload.
    /// QSV args are emitted only by the Linux branch of
    /// `assemble_ffmpeg_args` and require a DRI render node.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_assemble_qsv_8bit_uses_nv12() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_qsv"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        let vf_value = &args[vf_pos + 1];
        assert!(
            vf_value.contains("nv12"),
            "8-bit QSV must use format=nv12,hwupload, got: {vf_value}"
        );
    }

    // ── Colour metadata tests ─────────────────────────────────────

    /// Colour metadata flags must appear when VAAPI hwaccel decode is
    /// used (10-bit, no tonemap) because hwaccel decode does not
    /// propagate colour primaries/transfer/matrix from the input.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_colour_metadata_present_for_vaapi_hwaccel_decode() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }

        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mp4",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "p010le",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: Some(ColourMetadata {
                primaries: "bt2020",
                trc: "smpte2084",
                matrix: "bt2020nc",
                range: "tv",
            }),
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        assert!(
            args.iter().any(|a| a == "-color_primaries"),
            "colour metadata must include -color_primaries for VAAPI hwaccel decode"
        );
        assert!(
            args.iter().any(|a| a == "-color_trc"),
            "colour metadata must include -color_trc for VAAPI hwaccel decode"
        );
        // Colour flags must come before the output path
        let colour_pos = args.iter().position(|a| a == "-color_primaries").unwrap();
        let output_pos = args.iter().rposition(|a| a == "output.mp4").unwrap();
        assert!(
            colour_pos < output_pos,
            "colour metadata must come before output path"
        );
    }

    /// Software encode (libx265) should NOT have explicit colour
    /// metadata flags. ffmpeg propagates colour info from the input
    /// automatically; explicit flags cause haloing around text on PQ
    /// content.
    #[test]
    fn test_colour_metadata_absent_for_software_encode() {
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mp4",
            video_args: &["-c:v", "libx265"],
            pix_fmt: "p010le",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        assert!(
            !args.iter().any(|a| a == "-color_primaries"),
            "software encode must not have explicit colour metadata flags"
        );
        assert!(
            !args.iter().any(|a| a == "-color_trc"),
            "software encode must not have explicit colour metadata flags"
        );
        assert!(
            !args.iter().any(|a| a == "-color_range"),
            "software encode must not have explicit colour metadata flags"
        );
    }

    /// 8-bit VAAPI uses hwupload (not hwaccel decode), so colour
    /// metadata should not be needed even for VAAPI.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_colour_metadata_absent_for_vaapi_8bit() {
        if find_dri_render_node().is_none() {
            eprintln!("skipping: no /dev/dri/renderD* available");
            return;
        }

        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mp4",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "nv12",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        assert!(
            !args.iter().any(|a| a == "-color_primaries"),
            "8-bit VAAPI uses hwupload, not hwaccel decode - no colour metadata needed"
        );
    }

    /// The size BSF reaches the command line.
    #[cfg(target_os = "linux")]
    #[test]
    fn test_vaapi_hevc_bsf_crop_for_10bit() {
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mp4",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt: "p010le",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: Some((1920, 1080)),
            bsf_has_size: true,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        assert!(args
            .windows(2)
            .any(|w| w[0] == "-bsf:v" && w[1] == "hevc_metadata=width=1920:height=1080"));
        assert_eq!(args.last().unwrap(), "output.mp4");
    }

    /// ffmpeg without hevc_metadata width/height: frames are padded in
    /// software before the upload (after any tonemap), 10-bit included.
    #[cfg(target_os = "linux")]
    #[test]
    fn assemble_vaapi_size_fallback_old_ffmpeg() {
        let empty: &[String] = &[];
        let parts = |pix_fmt, tonemap| FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mkv",
            video_args: &["-c:v", "hevc_vaapi"],
            pix_fmt,
            audio_map_args: empty,
            audio_codec_args: empty,
            tonemap_filter: tonemap,
            codec_family: "hevc",
            vaapi_hevc_size: Some((1000, 562)),
            bsf_has_size: false,
            ..Default::default()
        };
        let node = Some("/dev/dri/renderD128");
        let vf = |a: &[String]| a[a.iter().position(|x| x == "-vf").unwrap() + 1].clone();
        let bsf = |a: &[String]| a[a.len() - 2..].to_vec();

        let a = assemble_ffmpeg_args_for(&parts("yuv420p", None), node, TargetOs::Linux);
        assert_eq!(vf(&a), "pad=1024:576,format=nv12,hwupload");
        assert_eq!(
            bsf(&a),
            ["hevc_metadata=crop_right=24:crop_bottom=14", "output.mkv"]
        );

        let a = assemble_ffmpeg_args_for(&parts("yuv420p", Some("tm")), node, TargetOs::Linux);
        assert_eq!(vf(&a), "tm,pad=1024:576,format=nv12,hwupload");

        // 10-bit decodes in software so the pad can run.
        let a = assemble_ffmpeg_args_for(&parts("p010le", None), node, TargetOs::Linux);
        assert_eq!(vf(&a), "pad=1024:576,format=p010,hwupload");
        assert!(!a.iter().any(|x| x == "-hwaccel"));
        assert!(a
            .windows(2)
            .any(|w| w[0] == "-vaapi_device" && w[1] == "/dev/dri/renderD128"));
        assert!(a.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "p010le"));

        // With width/height the 10-bit route stays on the device.
        let mut p = parts("p010le", None);
        p.bsf_has_size = true;
        let a = assemble_ffmpeg_args_for(&p, node, TargetOs::Linux);
        assert_eq!(vf(&a), "scale_vaapi=format=p010");
        assert_eq!(
            bsf(&a),
            ["hevc_metadata=width=1000:height=562", "output.mkv"]
        );
    }

    /// Non-VAAPI encoders must NOT include the VAAPI crop BSF.
    #[test]
    fn test_no_vaapi_bsf_for_software_encode() {
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args(&FfmpegAssemblyParts {
            input_path: "input.mkv",
            output_path: "output.mp4",
            video_args: &["-c:v", "libx265"],
            pix_fmt: "p010le",
            audio_map_args: empty,
            audio_codec_args: empty,
            is_image_source: false,
            threads: 0,
            tonemap_filter: None,
            codec_family: "hevc",
            colour_metadata: None,
            vaapi_hevc_size: None,
            bsf_has_size: false,
            dovi_in_output: false,
            dovi_p5: false,
            keep_source_tag: false,
            legacy_vsync: false,
            input_video_bsf: None,
        });

        assert!(
            !args.iter().any(|a| a == "-bsf:v"),
            "software encode must not include VAAPI crop BSF"
        );
    }
    // ══════════════════════════════════════════════════════════════
    //  Encoder selection, Vulkan and D3D12 (phase A)
    // ══════════════════════════════════════════════════════════════

    fn enc(name: &str, family: &str, hw: bool) -> EncoderInfo {
        EncoderInfo {
            name: name.into(),
            codec_family: family.into(),
            is_hardware: hw,
        }
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn pick_encoder_prefers_first_hw_in_table_order() {
        let detected = vec![
            enc("libx265", "hevc", false),
            enc("hevc_vaapi", "hevc", true),
            enc("hevc_vulkan", "hevc", true),
        ];
        assert_eq!(pick_encoder("hevc", &detected), "hevc_vaapi");
    }

    #[test]
    fn pick_encoder_ignores_other_families() {
        let detected = vec![
            enc("h264_vaapi", "h264", true),
            enc("libx265", "hevc", false),
        ];
        assert_eq!(pick_encoder("hevc", &detected), "libx265");
    }

    #[test]
    fn pick_encoder_falls_back_to_sw() {
        assert_eq!(pick_encoder("hevc", &[]), "libx265");
        assert_eq!(pick_encoder("h264", &[]), "libx264");
        assert_eq!(
            pick_encoder("av1", &[enc("libsvtav1", "av1", false)]),
            "libsvtav1"
        );
    }

    #[test]
    fn resolve_file_settings_auto_uses_first_hw() {
        let settings = default_settings();
        let detected = vec![
            enc("libx265", "hevc", false),
            enc("hevc_vaapi", "hevc", true),
            enc("hevc_vulkan", "hevc", true),
        ];
        let r = resolve_file_settings(
            "h264",
            "mkv",
            &{
                let mut s = settings.clone();
                s.codec_family = "hevc".into();
                s
            },
            &detected,
        );
        assert_eq!(r.encoder_name, "hevc_vaapi");
    }

    #[test]
    fn priority_tables_vendor_before_generic() {
        for table in [HEVC_PRIORITY, H264_PRIORITY, AV1_PRIORITY] {
            let pos = |pred: fn(&str) -> bool| table.iter().position(|e| pred(e));
            let generic = [pos(is_vulkan_encoder), pos(is_d3d12_encoder)];
            let vendor = table
                .iter()
                .enumerate()
                .filter(|(_, e)| {
                    ["_vaapi", "_nvenc", "_qsv", "_amf", "_videotoolbox"]
                        .iter()
                        .any(|v| e.ends_with(v))
                })
                .map(|(i, _)| i)
                .max();
            for g in generic.into_iter().flatten() {
                if let Some(v) = vendor {
                    assert!(v < g, "{table:?}: vendor API after generic at {g}");
                }
            }
            // Software is last.
            assert!(table.last().unwrap().starts_with("lib"));
            // D3D12 ahead of Vulkan where both exist.
            if let (Some(d), Some(v)) = (pos(is_d3d12_encoder), pos(is_vulkan_encoder)) {
                assert!(d < v);
            }
            // av1_vulkan pads the frame size, so it is never auto-selected.
            assert!(!table.contains(&"av1_vulkan"));
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn priority_tables_linux_include_vulkan() {
        assert_eq!(
            HEVC_PRIORITY,
            &[
                "hevc_vaapi",
                "hevc_nvenc",
                "hevc_qsv",
                "hevc_vulkan",
                "libx265"
            ]
        );
        assert_eq!(
            H264_PRIORITY,
            &[
                "h264_vaapi",
                "h264_nvenc",
                "h264_qsv",
                "h264_vulkan",
                "libx264"
            ]
        );
    }

    #[test]
    fn is_vulkan_encoder_matches() {
        assert!(is_vulkan_encoder("hevc_vulkan"));
        assert!(is_vulkan_encoder("av1_vulkan"));
        assert!(!is_vulkan_encoder("hevc_vaapi"));
        assert!(!is_vulkan_encoder("libx265"));
    }

    #[test]
    fn is_d3d12_encoder_matches() {
        assert!(is_d3d12_encoder("h264_d3d12va"));
        assert!(is_d3d12_encoder("av1_d3d12va"));
        assert!(!is_d3d12_encoder("hevc_amf"));
        assert!(!is_d3d12_encoder("d3d12va"));
    }

    #[test]
    fn test_vbr_flags_vulkan() {
        for e in ["hevc_vulkan", "h264_vulkan", "av1_vulkan"] {
            assert_eq!(
                vbr_flags(e, "4000000", "6000000"),
                strs(&["-b:v", "4000000", "-maxrate", "6000000"]),
                "{e}"
            );
        }
    }

    #[test]
    fn test_vbr_flags_d3d12() {
        for e in ["hevc_d3d12va", "h264_d3d12va", "av1_d3d12va"] {
            assert_eq!(
                vbr_flags(e, "4000000", "6000000"),
                strs(&["-rc_mode", "VBR", "-b:v", "4000000", "-maxrate", "6000000"]),
                "{e}"
            );
        }
    }

    #[test]
    fn test_cqp_flags_vulkan() {
        assert_eq!(cqp_flags("hevc_vulkan", "20", "22"), strs(&["-qp", "20"]));
        assert_eq!(cqp_flags("h264_vulkan", "20", "22"), strs(&["-qp", "20"]));
    }

    #[test]
    fn test_cqp_flags_d3d12() {
        // hevc/h264: -qp is the codec QP (d3d12va_encode_hevc.c:742,
        // d3d12va_encode_h264.c:608, range 0-52).
        for e in ["hevc_d3d12va", "h264_d3d12va"] {
            assert_eq!(
                cqp_flags(e, "20", "22"),
                strs(&["-rc_mode", "CQP", "-qp", "20"]),
                "{e}"
            );
        }
        // av1: -qp is capped at 52 (d3d12va_encode_av1.c:1114) yet used as
        // the 0-255 q-index (:761); -global_quality (d3d12va_encode.c:1139)
        // carries the scaled q-index instead.
        assert_eq!(
            cqp_flags("av1_d3d12va", "20", "22"),
            strs(&["-rc_mode", "CQP", "-global_quality", "100"])
        );
    }

    #[test]
    fn test_cqp_flags_av1_vulkan_scaled() {
        assert_eq!(cqp_flags("av1_vulkan", "20", "22"), strs(&["-qp", "100"]));
    }

    #[test]
    fn test_cqp_flags_av1_vaapi_scaled() {
        assert_eq!(
            cqp_flags("av1_vaapi", "20", "22"),
            strs(&["-rc_mode", "CQP", "-global_quality", "100"])
        );
        // HEVC VAAPI keeps the unscaled QP.
        assert_eq!(
            cqp_flags("hevc_vaapi", "20", "22"),
            strs(&["-rc_mode", "CQP", "-qp", "20"])
        );
    }

    #[test]
    fn test_crf_flags_vulkan_falls_back_to_cqp() {
        assert_eq!(
            crf_flags("hevc_vulkan", "20", "20", "22"),
            strs(&["-qp", "20"])
        );
    }

    #[test]
    fn av1_qindex_maps_defaults() {
        assert_eq!(av1_qindex(20), 100);
        assert_eq!(av1_qindex(22), 110);
        assert_eq!(av1_qindex(51), 255);
    }

    #[test]
    fn av1_qindex_clamps() {
        assert_eq!(av1_qindex(0), 1);
        assert_eq!(av1_qindex(60), 255);
        assert_eq!(av1_qindex(u32::MAX), 255);
    }

    #[test]
    fn hw_args_vulkan_linux_derives_from_drm() {
        let hw = hw_device_args(
            "hevc_vulkan",
            "yuv420p",
            false,
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
        );
        assert_eq!(
            hw.pre_input,
            strs(&[
                "-init_hw_device",
                "drm=dr:/dev/dri/renderD128",
                "-init_hw_device",
                "vulkan=vk@dr",
                "-filter_hw_device",
                "vk"
            ])
        );
        assert_eq!(hw.upload_filter.as_deref(), Some("format=nv12,hwupload"));
        assert!(!hw.emit_pix_fmt);
        assert!(!hw.hwaccel_decode);
    }

    #[test]
    fn hw_args_vulkan_linux_missing_node_sentinel() {
        let hw = hw_device_args("h264_vulkan", "yuv420p", false, None, TargetOs::Linux);
        assert_eq!(hw.pre_input[1], "drm=dr:/dev/dri/renderD_missing");
    }

    #[test]
    fn hw_args_vulkan_10bit_uploads_p010() {
        let hw = hw_device_args(
            "hevc_vulkan",
            "p010le",
            false,
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
        );
        assert_eq!(hw.upload_filter.as_deref(), Some("format=p010le,hwupload"));
        assert!(!hw.emit_pix_fmt);
    }

    #[test]
    fn hw_args_vulkan_windows() {
        let hw = hw_device_args("hevc_vulkan", "p010le", false, None, TargetOs::Windows);
        assert_eq!(
            hw.pre_input,
            strs(&["-init_hw_device", "vulkan=vk", "-filter_hw_device", "vk"])
        );
        assert_eq!(hw.upload_filter.as_deref(), Some("format=p010le,hwupload"));
        assert!(!hw.emit_pix_fmt);
    }

    #[test]
    fn hw_args_d3d12_windows() {
        // ffmpeg n8.1.3 sources: the device type is named "d3d12va"
        // (libavutil/hwcontext.c:83); the encoders accept only D3D12 frames
        // (libavcodec/d3d12va_encode_hevc.c:822, CODEC_PIXFMTS(AV_PIX_FMT_D3D12)),
        // so frames are uploaded; the D3D12 frame pool maps NV12 and P010
        // (libavutil/hwcontext_d3d12va.c:71-72), hence format=nv12 before
        // hwupload and no -pix_fmt. Not executed: Windows only.
        let hw = hw_device_args("av1_d3d12va", "yuv420p", false, None, TargetOs::Windows);
        assert_eq!(
            hw.pre_input,
            strs(&[
                "-init_hw_device",
                "d3d12va=d3d12",
                "-filter_hw_device",
                "d3d12"
            ])
        );
        assert_eq!(hw.upload_filter.as_deref(), Some("format=nv12,hwupload"));
        assert!(!hw.emit_pix_fmt);
    }

    #[test]
    fn hw_args_windows_vendor_encoders_unchanged() {
        // AMF, NVENC and QSV on Windows have never had device arguments.
        for e in ["hevc_amf", "hevc_nvenc", "hevc_qsv"] {
            let hw = hw_device_args(e, "p010le", false, None, TargetOs::Windows);
            assert!(hw.pre_input.is_empty(), "{e}");
            assert!(hw.upload_filter.is_none(), "{e}");
            assert!(hw.emit_pix_fmt, "{e}");
        }
    }

    #[test]
    fn hw_args_vaapi_unchanged() {
        let node = Some("/dev/dri/renderD128");
        let ten = hw_device_args("hevc_vaapi", "p010le", false, node, TargetOs::Linux);
        assert_eq!(
            ten.pre_input,
            strs(&[
                "-hwaccel",
                "vaapi",
                "-hwaccel_device",
                "/dev/dri/renderD128",
                "-hwaccel_output_format",
                "vaapi"
            ])
        );
        assert_eq!(
            ten.upload_filter.as_deref(),
            Some("scale_vaapi=format=p010")
        );
        assert!(ten.hwaccel_decode && !ten.emit_pix_fmt);

        let eight = hw_device_args("hevc_vaapi", "yuv420p", false, node, TargetOs::Linux);
        assert_eq!(
            eight.pre_input,
            strs(&["-vaapi_device", "/dev/dri/renderD128"])
        );
        assert_eq!(eight.upload_filter.as_deref(), Some("format=nv12,hwupload"));
        assert!(eight.emit_pix_fmt && !eight.hwaccel_decode);

        // 10-bit with a tonemap uses hwupload, not hwaccel decode.
        let tm = hw_device_args("hevc_vaapi", "p010le", true, node, TargetOs::Linux);
        assert_eq!(
            tm.pre_input,
            strs(&["-vaapi_device", "/dev/dri/renderD128"])
        );

        let missing = hw_device_args("hevc_vaapi", "yuv420p", false, None, TargetOs::Linux);
        assert_eq!(
            missing.pre_input,
            strs(&["-vaapi_device", "/dev/dri/renderD_missing"])
        );
        assert!(missing.upload_filter.is_none());
    }

    #[test]
    fn hw_args_qsv_unchanged() {
        let hw = hw_device_args(
            "hevc_qsv",
            "p010le",
            false,
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
        );
        assert_eq!(
            hw.pre_input,
            strs(&[
                "-init_hw_device",
                "qsv=qs",
                "-filter_hw_device",
                "qs",
                "-qsv_device",
                "/dev/dri/renderD128"
            ])
        );
        assert_eq!(
            hw.upload_filter.as_deref(),
            Some("format=p010le,hwupload=extra_hw_frames=64")
        );
        assert!(hw.emit_pix_fmt);
        let missing = hw_device_args("hevc_qsv", "yuv420p", false, None, TargetOs::Linux);
        assert_eq!(
            missing.pre_input,
            strs(&["-init_hw_device", "qsv=qs_child_device_missing"])
        );
    }

    #[test]
    fn hw_args_software_has_none() {
        let hw = hw_device_args(
            "libx265",
            "p010le",
            false,
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
        );
        assert_eq!(
            hw,
            HwArgs {
                pre_input: vec![],
                upload_filter: None,
                emit_pix_fmt: true,
                hwaccel_decode: false
            }
        );
    }

    #[test]
    fn hw_args_tonemap_then_upload() {
        let empty: &[String] = &[];
        let args = assemble_ffmpeg_args_for(
            &FfmpegAssemblyParts {
                input_path: "in.mkv",
                output_path: "out.mkv",
                video_args: &["-c:v", "hevc_vulkan", "-qp", "20"],
                pix_fmt: "yuv420p",
                audio_map_args: empty,
                audio_codec_args: empty,
                tonemap_filter: Some(TONEMAP_HABLE),
                codec_family: "hevc",
                ..Default::default()
            },
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
        );
        let vf = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(
            args[vf + 1],
            format!("{TONEMAP_HABLE},format=nv12,hwupload")
        );
        assert!(!args.iter().any(|a| a == "-pix_fmt"));
        let init = args.iter().position(|a| a == "-init_hw_device").unwrap();
        assert!(init < args.iter().position(|a| a == "-i").unwrap());
    }

    #[test]
    fn assemble_vulkan_10bit_no_pix_fmt() {
        let args = assemble_ffmpeg_args_for(
            &FfmpegAssemblyParts {
                input_path: "in.mkv",
                output_path: "out.mkv",
                video_args: &["-c:v", "hevc_vulkan", "-qp", "20"],
                pix_fmt: "p010le",
                codec_family: "hevc",
                ..Default::default()
            },
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
        );
        let vf = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(args[vf + 1], "format=p010le,hwupload");
        assert!(!args.iter().any(|a| a == "-pix_fmt"));
    }

    #[test]
    fn assemble_d3d12_windows() {
        let args = assemble_ffmpeg_args_for(
            &FfmpegAssemblyParts {
                input_path: "C:\\in.mkv",
                output_path: "C:\\out.mp4",
                video_args: &["-c:v", "hevc_d3d12va", "-rc_mode", "CQP", "-qp", "20"],
                pix_fmt: "p010le",
                codec_family: "hevc",
                ..Default::default()
            },
            None,
            TargetOs::Windows,
        );
        let i = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(
            &args[i - 4..i],
            &strs(&[
                "-init_hw_device",
                "d3d12va=d3d12",
                "-filter_hw_device",
                "d3d12"
            ])[..]
        );
        let vf = args.iter().position(|a| a == "-vf").unwrap();
        assert_eq!(args[vf + 1], "format=p010le,hwupload");
        assert!(!args.iter().any(|a| a == "-pix_fmt"));
    }

    #[test]
    fn assemble_vaapi_8bit_byte_identical() {
        // Pinned from the pre-refactor assembly for hevc_vaapi 8-bit, plus
        // the frame-rate mode.
        let args = assemble_ffmpeg_args_for(
            &FfmpegAssemblyParts {
                input_path: "in.mkv",
                output_path: "out.mkv",
                video_args: &["-c:v", "hevc_vaapi", "-rc_mode", "CQP", "-qp", "20"],
                pix_fmt: "yuv420p",
                codec_family: "hevc",
                ..Default::default()
            },
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
        );
        assert_eq!(
            args,
            strs(&[
                "-err_detect",
                "ignore_err",
                "-probesize",
                "100M",
                "-analyzeduration",
                "100M",
                "-y",
                "-vaapi_device",
                "/dev/dri/renderD128",
                "-i",
                "in.mkv",
                "-map",
                "0:v:0",
                "-c:v",
                "hevc_vaapi",
                "-rc_mode",
                "CQP",
                "-qp",
                "20",
                "-fps_mode",
                "passthrough",
                "-vf",
                "format=nv12,hwupload",
                "-pix_fmt",
                "yuv420p",
                "-map",
                "0:s?",
                "-c:s",
                "copy",
                "-disposition:s:0",
                "default",
                "out.mkv"
            ])
        );
    }

    #[test]
    fn test_encode_args_vulkan() {
        let args = test_encode_args(
            "hevc_vulkan",
            Some("/dev/dri/renderD128"),
            TargetOs::Linux,
            "/tmp/t.mp4",
        )
        .unwrap();
        assert_eq!(
            args,
            strs(&[
                "-init_hw_device",
                "drm=dr:/dev/dri/renderD128",
                "-init_hw_device",
                "vulkan=vk@dr",
                "-filter_hw_device",
                "vk",
                "-y",
                "-f",
                "lavfi",
                "-i",
                "color=black:s=256x256:d=0.04:r=25",
                "-frames:v",
                "1",
                "-vf",
                "format=nv12,hwupload",
                "-c:v",
                "hevc_vulkan",
                "/tmp/t.mp4"
            ])
        );
    }

    #[test]
    fn test_encode_args_d3d12_windows() {
        // Same device and upload arguments as `hw_args_d3d12_windows`, with
        // the same ffmpeg source references. Not executed: Windows only.
        let args = test_encode_args("h264_d3d12va", None, TargetOs::Windows, "t.mp4").unwrap();
        assert_eq!(
            &args[..4],
            &strs(&[
                "-init_hw_device",
                "d3d12va=d3d12",
                "-filter_hw_device",
                "d3d12"
            ])[..]
        );
        assert!(args
            .windows(2)
            .any(|w| w[0] == "-vf" && w[1] == "format=nv12,hwupload"));
        assert!(!args.iter().any(|a| a == "-pix_fmt"));
    }

    #[test]
    fn test_encode_args_vaapi_and_qsv_unchanged() {
        let node = Some("/dev/dri/renderD128");
        let v = test_encode_args("hevc_vaapi", node, TargetOs::Linux, "t.mp4").unwrap();
        assert_eq!(
            &v[..2],
            &strs(&["-vaapi_device", "/dev/dri/renderD128"])[..]
        );
        assert!(v
            .windows(2)
            .any(|w| w[0] == "-vf" && w[1] == "format=nv12,hwupload"));
        assert!(!v.iter().any(|a| a == "-pix_fmt"));
        let q = test_encode_args("hevc_qsv", node, TargetOs::Linux, "t.mp4").unwrap();
        assert!(q
            .windows(2)
            .any(|w| w[0] == "-vf" && w[1] == "format=nv12,hwupload=extra_hw_frames=64"));
        // No render node: VAAPI and QSV cannot work, so detection skips them.
        assert!(test_encode_args("hevc_vaapi", None, TargetOs::Linux, "t.mp4").is_none());
        assert!(test_encode_args("hevc_qsv", None, TargetOs::Linux, "t.mp4").is_none());
        // Windows QSV and NVENC: neither -vf nor device args, as before.
        let wq = test_encode_args("hevc_qsv", None, TargetOs::Windows, "t.mp4").unwrap();
        assert!(!wq.iter().any(|a| a == "-vf" || a == "-pix_fmt"));
        let wn = test_encode_args("hevc_nvenc", None, TargetOs::Windows, "t.mp4").unwrap();
        assert!(wn.windows(2).any(|w| w[0] == "-pix_fmt" && w[1] == "nv12"));
    }
    // ══════════════════════════════════════════════════════════════
    //  Dolby Vision routing and native coding (phase B)
    // ══════════════════════════════════════════════════════════════

    const CAPS_ALL: FfmpegCaps = FfmpegCaps {
        dovi_x265: true,
        dovi_svtav1: true,
        x265_hdr10plus: true,
        hevc_metadata_size: true,
        no_fps_mode: false,
        ffmpeg_major: None,
    };
    const CAPS_NONE: FfmpegCaps = FfmpegCaps {
        dovi_x265: false,
        dovi_svtav1: false,
        x265_hdr10plus: false,
        hevc_metadata_size: false,
        no_fps_mode: false,
        ffmpeg_major: None,
    };

    #[test]
    fn parse_ffmpeg_major_versions() {
        let banner = |v: &str| {
            format!("ffmpeg version {v} Copyright (c) 2000-2024 the FFmpeg developers\nbuilt with gcc 13\n")
        };
        assert_eq!(parse_ffmpeg_major(&banner("n8.1.3")), Some(8));
        assert_eq!(parse_ffmpeg_major(&banner("8.1.3")), Some(8));
        assert_eq!(parse_ffmpeg_major(&banner("6.1.1-3ubuntu5")), Some(6));
        assert_eq!(parse_ffmpeg_major(&banner("7.0")), Some(7));
        assert_eq!(parse_ffmpeg_major(&banner("n7.1.1-4-gabcdef")), Some(7));
        assert_eq!(parse_ffmpeg_major(&banner("9.0.1")), Some(9));
        // The bare version token is accepted too.
        assert_eq!(parse_ffmpeg_major("n8.1.3"), Some(8));
        assert_eq!(parse_ffmpeg_major("6.1.1-3ubuntu5"), Some(6));
        // Git and date-stamped builds carry no release number.
        assert_eq!(parse_ffmpeg_major(&banner("N-117341-g1a2b3c4d5e")), None);
        assert_eq!(parse_ffmpeg_major(&banner("git-2024-05-01-abcdef")), None);
        assert_eq!(parse_ffmpeg_major(&banner("2024-05-01-git-abcdef")), None);
        assert_eq!(parse_ffmpeg_major(&banner("unknown")), None);
        assert_eq!(parse_ffmpeg_major(""), None);
        assert_eq!(parse_ffmpeg_major("ffmpeg version \n"), None);
    }

    #[test]
    fn hdr10p_vaapi_needs_ffmpeg_7() {
        use hdr10plus_loss::*;
        use Hdr10PlusRoute::*;
        let with = |major| FfmpegCaps {
            ffmpeg_major: major,
            ..CAPS_ALL
        };
        let route = |enc, caps: &FfmpegCaps| {
            hdr10plus_route(true, true, true, enc, "hevc", None, caps, "B")
        };
        for major in [Some(5), Some(6)] {
            assert_eq!(
                route("hevc_vaapi", &with(major)),
                Err(VAAPI_OLD_FFMPEG),
                "{major:?}"
            );
        }
        for major in [Some(7), Some(8), Some(9), None] {
            assert_eq!(
                route("hevc_vaapi", &with(major)),
                Ok(Some(Injector)),
                "{major:?}"
            );
        }
        // Other encoders are unaffected by the version.
        assert_eq!(route("hevc_nvenc", &with(Some(6))), Ok(Some(Injector)));
        assert_eq!(route("libx265", &with(Some(6))), Ok(Some(X265Native)));
        // Nothing to keep: no reason is given.
        assert_eq!(
            hdr10plus_route(
                false,
                true,
                true,
                "hevc_vaapi",
                "hevc",
                None,
                &with(Some(6)),
                ""
            ),
            Ok(None)
        );
    }

    #[test]
    fn parse_help_has_dolbyvision() {
        let yes = "libx265 AVOptions:\n  -x265-params       <dictionary> E..V....... set the x265 configuration\n  -dolbyvision       <boolean>    E..V....... Enable Dolby Vision RPU coding (default auto)\n     auto                         E..V.......\n";
        let no = "libx265 AVOptions:\n  -crf               <float>      E..V....... set the x265 crf\n  -udu_sei           <boolean>    E..V....... Use user data unregistered SEI\n";
        assert!(help_has_dolbyvision(yes));
        assert!(!help_has_dolbyvision(no));
        // A mention in a description is not the option itself.
        assert!(!help_has_dolbyvision(
            "  -foo  <int>  E..V.... like dolbyvision\n"
        ));
    }

    #[test]
    fn parse_dhdr10_disabled_warning() {
        assert!(dhdr10_disabled(
            "x265 [warning]: --dhdr10-info disabled. Enable HDR10_PLUS in cmake.\n"
        ));
        assert!(!dhdr10_disabled(
            "x265 [info]: tools: b-intra strong-intra-smoothing deblock sao dhdr10-info\n"
        ));
    }

    #[test]
    fn x265_param_path_escape_windows_drive() {
        assert_eq!(
            x265_param_path_escape("C:\\Users\\me\\Temp\\m.json"),
            "C\\:\\\\Users\\\\me\\\\Temp\\\\m.json"
        );
        assert_eq!(x265_param_path_escape("/tmp/m.json"), "/tmp/m.json");
    }

    /// Port of ffmpeg's `av_get_token` (libavutil/avstring.c:143-176):
    /// returns the token up to `term` and the rest of the input.
    fn ff_get_token<'a>(buf: &'a str, term: &[char]) -> (String, &'a str) {
        let ws = |c: char| matches!(c, ' ' | '\n' | '\t' | '\r');
        let mut it = buf.trim_start_matches(ws).char_indices().peekable();
        let start = buf.len() - buf.trim_start_matches(ws).len();
        let (mut out, mut end) = (String::new(), 0usize);
        let mut rest = buf.len();
        while let Some((i, c)) = it.next() {
            if term.contains(&c) {
                rest = start + i;
                break;
            }
            if c == '\\' && it.peek().is_some() {
                out.push(it.next().unwrap().1);
                end = out.len();
            } else if c == '\'' {
                let mut closed = false;
                for (_, q) in it.by_ref() {
                    if q == '\'' {
                        closed = true;
                        break;
                    }
                    out.push(q);
                }
                if closed {
                    end = out.len();
                }
            } else {
                out.push(c);
            }
        }
        while out.len() > end && out.ends_with(ws) {
            out.pop();
        }
        (out, &buf[rest.min(buf.len())..])
    }

    /// ffmpeg's dictionary parser must hand x265 the exact path back.
    #[test]
    fn x265_param_path_escape_survives_ffmpeg_tokeniser() {
        for path in [
            "/tmp/Mum's C/histv_hdr10p_1.json",
            "/tmp/back\\slash/x.json",
            "/tmp/caf\u{e9} \u{65e5}\u{672c}/x.json",
            "C:\\Users\\O'Brien\\AppData\\Local\\Temp\\x.json",
            " /lead and trail ",
            "/a=b/c:d/x.json",
        ] {
            let arg = format!("dhdr10-info={}:hdr10-opt=1", x265_param_path_escape(path));
            let (key, rest) = ff_get_token(&arg, &['=']);
            assert_eq!(key, "dhdr10-info");
            let (val, rest) = ff_get_token(&rest[1..], &[':']);
            assert_eq!(val, path, "{arg}");
            assert_eq!(rest, ":hdr10-opt=1", "{arg}");
        }
    }

    #[test]
    fn hdr10p_native_args_x265() {
        let mut args = strs(&["-c:v", "libx265", "-b:v", "4000000"]);
        let p = format!(
            "dhdr10-info={}",
            x265_param_path_escape("C:\\out\\histv_hdr10p_1.json")
        );
        add_x265_param(&mut args, &p);
        assert_eq!(
            &args[4..],
            strs(&[
                "-x265-params",
                "dhdr10-info=C\\:\\\\out\\\\histv_hdr10p_1.json"
            ])
        );
        // An existing -x265-params is extended, not duplicated.
        add_x265_param(&mut args, "hdr10-opt=1");
        assert_eq!(args.iter().filter(|a| *a == "-x265-params").count(), 1);
        assert_eq!(
            args.last().unwrap(),
            "dhdr10-info=C\\:\\\\out\\\\histv_hdr10p_1.json:hdr10-opt=1"
        );
    }

    /// The hardware encode ran without B-frames for the injector; the
    /// software fallback has B-frames, so libx265 must code the metadata.
    #[test]
    fn fallback_after_hw_injector_codes_hdr10plus_with_x265() {
        let decision = EncodeDecision::Cqp { qi: 22, qp: 22 };
        let hdr10 = FallbackHdr10Plus {
            can_process: true,
            family: "hevc",
            dovi: None,
            caps: &CAPS_ALL,
            profile: "B",
        };
        let mut json_calls = 0;
        let plan = fallback_sw_args(&decision, "libx265", "22", "22", Some(&hdr10), &mut || {
            json_calls += 1;
            Ok("/tmp/histv_hdr10p_1.json".to_string())
        });
        assert_eq!(json_calls, 1);
        assert!(!plan.keep_injection);
        assert_eq!(plan.video_args[..2], strs(&["-c:v", "libx265"])[..]);
        let i = plan
            .video_args
            .iter()
            .position(|a| a == "-x265-params")
            .unwrap();
        assert!(plan.video_args[i + 1].contains("dhdr10-info=/tmp/histv_hdr10p_1.json"));
        assert!(!plan.video_args.contains(&"-bf".to_string()));
    }

    #[test]
    fn fallback_without_x265_hdr10plus_is_static_hdr10() {
        let decision = EncodeDecision::Vbr {
            target_bps: 4_000_000,
            peak_bps: 6_000_000,
        };
        let hdr10 = FallbackHdr10Plus {
            can_process: true,
            family: "hevc",
            dovi: None,
            caps: &CAPS_NONE,
            profile: "B",
        };
        let plan = fallback_sw_args(&decision, "libx265", "22", "22", Some(&hdr10), &mut || {
            panic!("no JSON without x265 support")
        });
        assert!(!plan.keep_injection);
        assert!(!plan.video_args.iter().any(|a| a.contains("dhdr10-info")));
        assert_eq!(
            plan.notes,
            vec![format!(
                "  HDR10+: Static HDR10 only ({})",
                hdr10plus_loss::X265_NO_SUPPORT
            )]
        );
    }

    #[test]
    fn fallback_json_failure_drops_injection() {
        let decision = EncodeDecision::Cqp { qi: 22, qp: 22 };
        let hdr10 = FallbackHdr10Plus {
            can_process: true,
            family: "hevc",
            dovi: None,
            caps: &CAPS_ALL,
            profile: "B",
        };
        let plan = fallback_sw_args(&decision, "libx265", "22", "22", Some(&hdr10), &mut || {
            Err("disk full".to_string())
        });
        assert!(!plan.keep_injection);
        assert!(!plan.video_args.iter().any(|a| a.contains("dhdr10-info")));
        assert_eq!(
            plan.notes,
            vec!["  HDR10+: disk full - output has static HDR10 only".to_string()]
        );
    }

    /// An encoder that still routes to the injector gets B-frames off and
    /// keeps the metadata.
    #[test]
    fn fallback_on_injector_route_keeps_injection_without_b_frames() {
        let decision = EncodeDecision::Cqp { qi: 22, qp: 22 };
        let hdr10 = FallbackHdr10Plus {
            can_process: true,
            family: "hevc",
            dovi: None,
            caps: &CAPS_ALL,
            profile: "B",
        };
        let plan = fallback_sw_args(
            &decision,
            "hevc_vaapi",
            "22",
            "22",
            Some(&hdr10),
            &mut || panic!("injector route writes no JSON"),
        );
        assert!(plan.keep_injection);
        assert_eq!(
            plan.video_args[plan.video_args.len() - 2..],
            strs(&["-bf", "0"])[..]
        );
    }

    /// No extracted metadata: the arguments are the plain software ones.
    #[test]
    fn fallback_without_metadata_is_plain() {
        let decision = EncodeDecision::Cqp { qi: 22, qp: 22 };
        let plan = fallback_sw_args(&decision, "libx265", "22", "22", None, &mut || {
            panic!("no JSON without metadata")
        });
        assert!(!plan.keep_injection);
        assert!(plan.notes.is_empty());
        assert_eq!(plan.video_args[..2], strs(&["-c:v", "libx265"])[..]);
        assert!(!plan.video_args.contains(&"-bf".to_string()));
    }

    #[test]
    fn hdr10p_hw_uses_injector() {
        use Hdr10PlusRoute::*;
        for enc in ["hevc_vaapi", "hevc_vulkan", "hevc_nvenc", "hevc_d3d12va"] {
            for profile in ["A", "B", "N/A", ""] {
                assert_eq!(
                    hdr10plus_route(true, true, true, enc, "hevc", None, &CAPS_ALL, profile),
                    Ok(Some(Injector)),
                    "{enc} {profile}"
                );
            }
        }
        assert_eq!(
            hdr10plus_route(true, true, true, "libx265", "hevc", None, &CAPS_ALL, "B"),
            Ok(Some(X265Native))
        );
    }

    #[test]
    fn hdr10p_requires_cap() {
        use hdr10plus_loss::*;
        let r = |has, crate_ok, preserve, enc, fam, caps: &FfmpegCaps, prof| {
            hdr10plus_route(has, crate_ok, preserve, enc, fam, None, caps, prof)
        };
        // libx265 reorders pictures, so without dhdr10-info nothing is kept,
        // and nothing is extracted.
        assert_eq!(
            r(true, true, true, "libx265", "hevc", &CAPS_NONE, "B"),
            Err(X265_NO_SUPPORT)
        );
        assert_eq!(
            r(true, true, true, "libx265", "hevc", &CAPS_ALL, "N/A"),
            Err(X265_PROFILE)
        );
        // An unknown profile is attempted (the JSON writer checks again).
        assert!(r(true, true, true, "libx265", "hevc", &CAPS_ALL, "").is_ok());
        assert_eq!(
            r(true, false, true, "libx265", "hevc", &CAPS_ALL, "B"),
            Err(NO_CRATE)
        );
        assert_eq!(
            r(true, true, true, "libsvtav1", "av1", &CAPS_ALL, "B"),
            Err(NOT_HEVC)
        );
        assert_eq!(
            r(true, true, true, "libx264", "h264", &CAPS_ALL, "B"),
            Err(NOT_HEVC)
        );
        // Nothing to keep: no warning.
        assert_eq!(
            r(true, true, false, "libx265", "hevc", &CAPS_ALL, "B"),
            Ok(None)
        );
        assert_eq!(
            r(false, false, true, "libx265", "hevc", &CAPS_NONE, ""),
            Ok(None)
        );
    }

    #[test]
    fn hdr10p_with_dovi_only_through_native_x265() {
        use hdr10plus_loss::*;
        use Hdr10PlusRoute::*;
        let native = Some(DoviRoute::Native {
            encoder: "libx265",
            family: "hevc",
        });
        let r = |enc, dovi, caps: &FfmpegCaps| {
            hdr10plus_route(true, true, true, enc, "hevc", dovi, caps, "B")
        };
        assert_eq!(r("libx265", native, &CAPS_ALL), Ok(Some(X265Native)));
        assert_eq!(r("libx265", native, &CAPS_NONE), Err(X265_NO_SUPPORT));
        assert_eq!(
            r("hevc_vaapi", Some(DoviRoute::RpuPipeline), &CAPS_ALL),
            Err(DOVI_KEPT)
        );
        // DV dropped to HDR10: HDR10+ routes as for a plain source.
        assert_eq!(
            r("hevc_vaapi", Some(DoviRoute::Hdr10), &CAPS_ALL),
            Ok(Some(Injector))
        );
    }

    #[test]
    fn encoder_family_from_name() {
        for (e, f) in [
            ("libx265", "hevc"),
            ("hevc_vulkan", "hevc"),
            ("h264_d3d12va", "h264"),
            ("libsvtav1", "av1"),
            ("av1_vaapi", "av1"),
        ] {
            assert_eq!(encoder_family(e), f, "{e}");
        }
    }

    /// Every combination of profile, target, encoder, capability, MP4Box and
    /// preserve-HDR.
    #[test]
    fn dovi_route_matrix() {
        use DoviRoute::*;
        const X265: DoviRoute = Native {
            encoder: "libx265",
            family: "hevc",
        };
        const SVT: DoviRoute = Native {
            encoder: "libsvtav1",
            family: "av1",
        };
        for profile in [5u8, 7, 8, 10] {
            for (family, _encoder) in [
                ("hevc", "libx265"),
                ("hevc", "hevc_nvenc"),
                ("hevc", "hevc_vaapi"),
                ("h264", "h264_vaapi"),
                ("av1", "libsvtav1"),
            ] {
                for caps in [CAPS_ALL, CAPS_NONE] {
                    for mp4box in [true, false] {
                        for preserve in [true, false] {
                            let hw = is_hardware_encoder_name(_encoder);
                            let got = dovi_route(profile, family, hw, &caps, mp4box, preserve);
                            let want = if !preserve {
                                Hdr10
                            } else if hw && mp4box && profile != 10 {
                                // Hardware keeps its encoder through the RPU pipeline.
                                RpuPipeline
                            } else if caps.dovi_x265 {
                                match (profile, family) {
                                    (8 | 10, "av1") => SVT,
                                    (5 | 8 | 10, _) => X265,
                                    (7, _) if mp4box => RpuPipeline,
                                    _ => Hdr10,
                                }
                            } else if mp4box && profile != 10 {
                                RpuPipeline
                            } else {
                                Hdr10
                            };
                            assert_eq!(
                                got, want,
                                "profile {profile} family {family} caps {caps:?} mp4box {mp4box} preserve {preserve}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn dovi_route_av1_needs_svtav1_cap() {
        let x265_only = FfmpegCaps {
            dovi_x265: true,
            ..CAPS_NONE
        };
        assert_eq!(
            dovi_route(10, "av1", false, &x265_only, false, true),
            DoviRoute::Native {
                encoder: "libx265",
                family: "hevc"
            }
        );
        // Profile 5 never goes to AV1.
        assert_eq!(
            dovi_route(5, "av1", false, &CAPS_ALL, false, true),
            DoviRoute::Native {
                encoder: "libx265",
                family: "hevc"
            }
        );
    }

    fn dv_settings(container: &str) -> BatchSettings {
        let mut s = default_settings();
        s.output_container = container.into();
        s
    }

    #[test]
    fn resolve_output_ext_native_dv_keeps_mkv() {
        let ext = resolve_output_ext_with(
            "/v/p81.mkv",
            "hevc",
            Some(8),
            &dv_settings("auto"),
            true,
            &[],
            &CAPS_ALL,
            true,
            false,
        );
        assert_eq!(ext, "mkv");
        let forced = resolve_output_ext_with(
            "/v/p81.mp4",
            "hevc",
            Some(8),
            &dv_settings("mkv"),
            true,
            &[],
            &CAPS_ALL,
            true,
            false,
        );
        assert_eq!(forced, "mkv");
    }

    #[test]
    fn resolve_output_ext_rpu_pipeline_forces_mp4() {
        // Profile 7 always uses the pipeline; so does any profile without native coding.
        for (profile, caps) in [(7, CAPS_ALL), (8, CAPS_NONE)] {
            let ext = resolve_output_ext_with(
                "/v/a.mkv",
                "hevc",
                Some(profile),
                &dv_settings("mkv"),
                true,
                &[],
                &caps,
                true,
                false,
            );
            assert_eq!(ext, "mp4", "profile {profile}");
        }
        // Without MP4Box the route is HDR10, which keeps the configured container.
        let ext = resolve_output_ext_with(
            "/v/a.mkv",
            "hevc",
            Some(7),
            &dv_settings("mkv"),
            true,
            &[],
            &CAPS_ALL,
            false,
            false,
        );
        assert_eq!(ext, "mkv");
    }

    #[test]
    fn resolve_output_ext_hw_encoder_uses_rpu_pipeline() {
        let hw = [
            enc("hevc_nvenc", "hevc", true),
            enc("libx265", "hevc", false),
        ];
        // Hardware encoder with MP4Box: the RPU pipeline, so MP4.
        let ext = resolve_output_ext_with(
            "/v/p81.mkv",
            "hevc",
            Some(8),
            &dv_settings("mkv"),
            true,
            &hw,
            &CAPS_ALL,
            true,
            false,
        );
        assert_eq!(ext, "mp4");
        // Without MP4Box: native libx265, which keeps MKV.
        let ext = resolve_output_ext_with(
            "/v/p81.mkv",
            "hevc",
            Some(8),
            &dv_settings("mkv"),
            true,
            &hw,
            &CAPS_ALL,
            false,
            false,
        );
        assert_eq!(ext, "mkv");
    }

    /// A VAAPI encoder is swapped for libx265 for Dolby Vision, and libx265
    /// codes DV natively: the output keeps the configured container.
    #[cfg(target_os = "linux")]
    #[test]
    fn resolve_output_ext_vaapi_swap_keeps_mkv() {
        let vaapi = [
            enc("hevc_vaapi", "hevc", true),
            enc("libx265", "hevc", false),
        ];
        let ext = resolve_output_ext_with(
            "/v/p81.mkv",
            "hevc",
            Some(8),
            &dv_settings("mkv"),
            true,
            &vaapi,
            &CAPS_ALL,
            true,
            false,
        );
        assert_eq!(ext, "mkv");
    }

    /// A VAAPI encoder with `HISTV_ALLOW_VAAPI_DV=1` keeps the RPU pipeline.
    #[test]
    fn resolve_output_ext_vaapi_allowed_uses_rpu_pipeline() {
        let vaapi = [
            enc("hevc_vaapi", "hevc", true),
            enc("libx265", "hevc", false),
        ];
        let ext = resolve_output_ext_with(
            "/v/p81.mkv",
            "hevc",
            Some(8),
            &dv_settings("mkv"),
            true,
            &vaapi,
            &CAPS_ALL,
            true,
            true,
        );
        assert_eq!(ext, "mp4");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn dovi_route_for_encoder_vaapi_swap_is_native() {
        let native = DoviRoute::Native {
            encoder: "libx265",
            family: "hevc",
        };
        let detected = [
            enc("hevc_vaapi", "hevc", true),
            enc("h264_vaapi", "h264", true),
        ];
        let route = |profile, enc, fam, caps: &FfmpegCaps, rpu, allow| {
            dovi_route_for_encoder(profile, enc, fam, &detected, caps, rpu, true, allow)
        };
        // Profiles 5 and 8 on VAAPI end on native libx265.
        for profile in [5, 8] {
            assert_eq!(
                route(profile, "hevc_vaapi", "hevc", &CAPS_ALL, true, false),
                native
            );
            // An H.264 target is forced to HEVC, which is VAAPI too.
            assert_eq!(
                route(profile, "h264_vaapi", "h264", &CAPS_ALL, true, false),
                native
            );
            // HISTV_ALLOW_VAAPI_DV keeps VAAPI on the RPU pipeline.
            assert_eq!(
                route(profile, "hevc_vaapi", "hevc", &CAPS_ALL, true, true),
                DoviRoute::RpuPipeline
            );
            // Without native DV in ffmpeg the swap still needs the pipeline.
            assert_eq!(
                route(profile, "hevc_vaapi", "hevc", &CAPS_NONE, true, false),
                DoviRoute::RpuPipeline
            );
        }
        // Profile 7 cannot be coded natively.
        assert_eq!(
            route(7, "hevc_vaapi", "hevc", &CAPS_ALL, true, false),
            DoviRoute::RpuPipeline
        );
        // Other hardware encoders are not swapped.
        assert_eq!(
            route(8, "hevc_nvenc", "hevc", &CAPS_ALL, true, false),
            DoviRoute::RpuPipeline
        );
        // Without the pipeline the route is already native, as before.
        assert_eq!(
            route(8, "hevc_vaapi", "hevc", &CAPS_ALL, false, false),
            native
        );
    }

    /// The swap to libx265 keeps HDR10+ through x265 instead of dropping it.
    #[cfg(target_os = "linux")]
    #[test]
    fn dovi_vaapi_swap_keeps_hdr10plus() {
        let dovi =
            dovi_route_for_encoder(8, "hevc_vaapi", "hevc", &[], &CAPS_ALL, true, true, false);
        let DoviRoute::Native { encoder, family } = dovi else {
            panic!("{dovi:?}");
        };
        assert_eq!(
            hdr10plus_route(
                true,
                true,
                true,
                encoder,
                family,
                Some(dovi),
                &CAPS_ALL,
                "B"
            ),
            Ok(Some(Hdr10PlusRoute::X265Native))
        );
        // The same file kept on the RPU pipeline loses HDR10+.
        assert_eq!(
            hdr10plus_route(
                true,
                true,
                true,
                "libx265",
                "hevc",
                Some(DoviRoute::RpuPipeline),
                &CAPS_ALL,
                "B"
            ),
            Err(hdr10plus_loss::DOVI_KEPT)
        );
    }

    const HDR_ON: OutputRange = OutputRange {
        compat: false,
        preserve_hdr: true,
    };
    const COMPAT: OutputRange = OutputRange {
        compat: true,
        preserve_hdr: true,
    };
    const HDR_OFF: OutputRange = OutputRange {
        compat: false,
        preserve_hdr: false,
    };
    const COMPAT_HDR_OFF: OutputRange = OutputRange {
        compat: true,
        preserve_hdr: false,
    };

    fn dv_item(name: &str, profile: Option<u8>) -> QueueItem {
        let mut probe = crate::test_helpers::synthetic_probe("hevc", 20.0, 60.0, true);
        probe.dovi_profile = profile;
        crate::test_helpers::make_queue_item(
            &format!("/nonexistent/{name}.mp4"),
            QueueItemStatus::Pending,
            probe,
        )
    }

    #[test]
    fn sdr_issue_names_the_setting_to_change() {
        let fact = "profile 5 has no HDR10 layer, so SDR colours will probably be wrong";
        assert_eq!(
            dovi_sdr_issue(5, COMPAT, false).as_deref(),
            Some(format!("{fact}; turn off Compatibility mode").as_str())
        );
        assert_eq!(
            dovi_sdr_issue(5, HDR_OFF, false).as_deref(),
            Some(format!("{fact}; turn on Preserve HDR").as_str())
        );
        assert_eq!(
            dovi_sdr_issue(5, COMPAT_HDR_OFF, false).as_deref(),
            Some(format!("{fact}; turn off Compatibility mode and turn on Preserve HDR").as_str())
        );
        assert_eq!(
            dovi_sdr_issue(5, COMPAT, true).as_deref(),
            Some(format!("{fact}; drop --compat").as_str())
        );
        assert_eq!(
            dovi_sdr_issue(5, HDR_OFF, true).as_deref(),
            Some(format!("{fact}; drop --no-hdr").as_str())
        );
        assert_eq!(
            dovi_sdr_issue(5, COMPAT_HDR_OFF, true).as_deref(),
            Some(format!("{fact}; drop --compat and --no-hdr").as_str())
        );
    }

    #[test]
    fn sdr_issue_only_for_profile5_converted_to_sdr() {
        // HDR kept: nothing to say here.
        assert_eq!(dovi_sdr_issue(5, HDR_ON, false), None);
        // Profiles with an HDR10 (or SDR/HLG) base layer tonemap fine.
        for profile in [7, 8, 10] {
            for out in [COMPAT, HDR_OFF, COMPAT_HDR_OFF] {
                assert_eq!(dovi_sdr_issue(profile, out, false), None);
                assert_eq!(dovi_sdr_issue(profile, out, true), None);
            }
        }
    }

    #[test]
    fn sdr_log_line_for_profile5_only() {
        assert_eq!(
            dovi_sdr_log_line(5, COMPAT, false).as_deref(),
            Some("  Dolby Vision cannot be kept - output is SDR (profile 5 has no HDR10 layer, so SDR colours will probably be wrong; turn off Compatibility mode)")
        );
        assert_eq!(dovi_sdr_log_line(5, HDR_ON, false), None);
        assert_eq!(dovi_sdr_log_line(8, COMPAT, false), None);
        assert_eq!(dovi_sdr_log_line(7, HDR_OFF, false), None);
        // A stream copy keeps Dolby Vision.
        assert_eq!(dovi_sdr_log_line(5, HDR_OFF, true), None);
    }

    #[test]
    fn preflight_scan_warns_on_profile5_converted_to_sdr() {
        let queue = [dv_item("p5", Some(5))];
        let w = preflight_scan(&queue, &[], COMPAT);
        let sdr: Vec<_> = w
            .iter()
            .filter(|w| w.actual_outcome.starts_with("SDR conversion"))
            .collect();
        assert_eq!(sdr.len(), 1);
        assert_eq!(sdr[0].file_name, "p5.mp4");
        assert_eq!(sdr[0].source_type, "Dolby Vision Profile 5");
        assert_eq!(
            sdr[0].actual_outcome,
            "SDR conversion (profile 5 has no HDR10 layer, so SDR colours will probably be wrong; turn off Compatibility mode)"
        );
        assert_eq!(sdr[0].missing_tool, None);
        // The HDR10-fallback warning would contradict it.
        assert!(!w
            .iter()
            .any(|w| w.actual_outcome.starts_with("HDR10 fallback")));
    }

    #[test]
    fn preflight_scan_is_quiet_for_other_profiles_and_sources_under_sdr() {
        let queue = [
            dv_item("p8", Some(8)),
            dv_item("p7", Some(7)),
            dv_item("plain", None),
        ];
        for out in [COMPAT, HDR_OFF] {
            let w = preflight_scan(&queue, &[], out);
            assert!(!w
                .iter()
                .any(|w| w.actual_outcome.starts_with("SDR conversion")));
        }
        // HDR kept: profile 5 gets no SDR warning either.
        let w = preflight_scan(&[dv_item("p5", Some(5))], &[], HDR_ON);
        assert!(!w
            .iter()
            .any(|w| w.actual_outcome.starts_with("SDR conversion")));
    }

    #[test]
    fn preflight_no_mp4box_warning_for_native_route() {
        // Native coding needs no MP4Box.
        assert_eq!(dovi_preflight_issue(8, &CAPS_ALL, true, false), None);
        assert_eq!(dovi_preflight_issue(5, &CAPS_ALL, true, false), None);
        // Profile 7 still needs it.
        assert_eq!(
            dovi_preflight_issue(7, &CAPS_ALL, true, false).map(|i| i.1),
            Some(Some("MP4Box"))
        );
        // Profile 10 without native coding cannot use the HEVC pipeline.
        assert_eq!(
            dovi_preflight_issue(10, &CAPS_NONE, true, true).map(|i| i.1),
            Some(None)
        );
        assert_eq!(dovi_preflight_issue(7, &CAPS_NONE, true, true), None);
    }

    #[test]
    fn preflight_profile5_hdr10_route_names_the_missing_base_layer() {
        use dovi_loss::*;
        // No native coding, Rust pipeline present, MP4Box missing: the way
        // out is MP4Box or HISTV's ffmpeg.
        assert_eq!(
            dovi_preflight_issue(5, &CAPS_NONE, true, false),
            Some((P5_NO_MP4BOX, Some("MP4Box")))
        );
        // No native coding and no Rust pipeline: only the ffmpeg helps.
        assert_eq!(
            dovi_preflight_issue(5, &CAPS_NONE, false, false),
            Some((P5_NO_NATIVE, None))
        );
        for detail in [P5_NO_MP4BOX, P5_NO_NATIVE] {
            assert!(detail.contains("profile 5 has no HDR10 layer"));
            assert!(detail.contains("HISTV's ffmpeg"));
        }
        assert!(P5_NO_MP4BOX.contains("install MP4Box"));
        assert!(!P5_NO_NATIVE.contains("MP4Box"));
    }

    #[test]
    fn preflight_profile5_unaffected_when_dv_can_be_kept() {
        // Native coding, or the RPU pipeline with MP4Box.
        assert_eq!(dovi_preflight_issue(5, &CAPS_ALL, false, false), None);
        assert_eq!(dovi_preflight_issue(5, &CAPS_NONE, true, true), None);
    }

    #[test]
    fn preflight_profiles_7_8_keep_the_generic_hdr10_wording() {
        use dovi_loss::*;
        assert_eq!(
            dovi_preflight_issue(7, &CAPS_ALL, true, false),
            Some((NO_MP4BOX, Some("MP4Box")))
        );
        assert_eq!(
            dovi_preflight_issue(8, &CAPS_NONE, true, false),
            Some((NO_MP4BOX, Some("MP4Box")))
        );
        assert_eq!(
            dovi_preflight_issue(8, &CAPS_NONE, false, false),
            Some((NO_NATIVE, None))
        );
        assert_eq!(NO_MP4BOX, "MP4Box not found");
        assert_eq!(NO_NATIVE, "this ffmpeg has no native Dolby Vision coding");
        assert!(!NO_MP4BOX.contains("profile 5"));
        assert!(!NO_NATIVE.contains("profile 5"));
    }

    #[test]
    fn dovi_hdr10_log_line_is_specific_for_profile5_only() {
        use dovi_loss::*;
        let generic =
            "  Dolby Vision cannot be kept with this ffmpeg and tool set - output is HDR10";
        assert_eq!(
            dovi_hdr10_log_line(5, &CAPS_NONE, true, false),
            format!("  Dolby Vision cannot be kept - output is HDR10 ({P5_NO_MP4BOX})")
        );
        assert_eq!(
            dovi_hdr10_log_line(5, &CAPS_NONE, false, false),
            format!("  Dolby Vision cannot be kept - output is HDR10 ({P5_NO_NATIVE})")
        );
        assert_eq!(dovi_hdr10_log_line(7, &CAPS_ALL, true, false), generic);
        assert_eq!(dovi_hdr10_log_line(8, &CAPS_NONE, true, false), generic);
    }

    #[test]
    fn copy_needs_dovi_entry_only_for_profile5_copies() {
        let vbr = EncodeDecision::Vbr {
            target_bps: 1,
            peak_bps: 2,
        };
        assert!(copy_needs_dovi_entry(&EncodeDecision::Copy, true));
        // Profile 8 or non-DV copies, and re-encodes (which write dvh1).
        assert!(!copy_needs_dovi_entry(&EncodeDecision::Copy, false));
        assert!(!copy_needs_dovi_entry(&vbr, true));
        assert!(!copy_needs_dovi_entry(&vbr, false));
    }

    #[test]
    fn native_dv_x265_cqp_vbv_from_threshold() {
        let existing = strs(&["-c:v", "libx265", "-preset", "slow", "-qp", "20"]);
        assert_eq!(
            dovi_native_args("libx265", &existing, 4_000_000, 1.5, 9_000_000),
            strs(&[
                "-dolbyvision",
                "1",
                "-maxrate",
                "6000000",
                "-bufsize",
                "12000000"
            ])
        );
        // No threshold: twice the source bitrate.
        assert_eq!(
            dovi_native_args("libx265", &existing, 0, 1.5, 9_000_000)[3],
            "18000000"
        );
    }

    #[test]
    fn native_dv_x265_vbr_uses_peak_for_vbv() {
        let existing = strs(&[
            "-c:v", "libx265", "-preset", "slow", "-b:v", "2000000", "-maxrate", "3000000",
        ]);
        assert_eq!(
            dovi_native_args("libx265", &existing, 2_000_000, 1.5, 9_000_000),
            strs(&["-dolbyvision", "1", "-bufsize", "6000000"])
        );
    }

    #[test]
    fn native_dv_no_duplicate_maxrate_in_precision() {
        let existing = strs(&[
            "-c:v", "libx265", "-crf", "20", "-maxrate", "7500000", "-bufsize", "15000000",
        ]);
        assert_eq!(
            dovi_native_args("libx265", &existing, 5_000_000, 1.5, 9_000_000),
            strs(&["-dolbyvision", "1"])
        );
    }

    #[test]
    fn native_dv_svtav1_no_vbv() {
        let existing = strs(&["-c:v", "libsvtav1", "-preset", "6", "-crf", "20"]);
        assert_eq!(
            dovi_native_args("libsvtav1", &existing, 4_000_000, 1.5, 9_000_000),
            strs(&["-dolbyvision", "1"])
        );
    }

    fn dv_parts<'a>(
        output: &'a str,
        video: &'a [&'a str],
        in_out: bool,
        p5: bool,
    ) -> FfmpegAssemblyParts<'a> {
        FfmpegAssemblyParts {
            input_path: "in.mp4",
            output_path: output,
            video_args: video,
            pix_fmt: "p010le",
            codec_family: "hevc",
            dovi_in_output: in_out,
            dovi_p5: p5,
            ..Default::default()
        }
    }

    fn arg_after(args: &[String], flag: &str) -> Option<String> {
        args.windows(2).find(|w| w[0] == flag).map(|w| w[1].clone())
    }

    #[test]
    fn native_dv_x265_mp4_strict_vbv() {
        let video = [
            "-c:v",
            "libx265",
            "-dolbyvision",
            "1",
            "-maxrate",
            "6000000",
            "-bufsize",
            "12000000",
        ];
        let args = assemble_ffmpeg_args_for(
            &dv_parts("out.mp4", &video, true, false),
            None,
            TargetOs::Linux,
        );
        assert_eq!(arg_after(&args, "-strict").as_deref(), Some("unofficial"));
        assert_eq!(arg_after(&args, "-dolbyvision").as_deref(), Some("1"));
        assert_eq!(args.last().unwrap(), "out.mp4");
    }

    #[test]
    fn native_dv_mkv_no_strict() {
        let video = ["-c:v", "libx265", "-dolbyvision", "1"];
        let args = assemble_ffmpeg_args_for(
            &dv_parts("out.mkv", &video, false, false),
            None,
            TargetOs::Linux,
        );
        assert!(arg_after(&args, "-strict").is_none());
    }

    #[test]
    fn assemble_dv_p5_mp4_uses_dvh1() {
        let video = ["-c:v", "libx265"];
        let args = assemble_ffmpeg_args_for(
            &dv_parts("out.mp4", &video, true, true),
            None,
            TargetOs::Linux,
        );
        assert_eq!(arg_after(&args, "-tag:v").as_deref(), Some("dvh1"));
    }

    #[test]
    fn assemble_dv_p8_mp4_uses_hvc1() {
        let video = ["-c:v", "libx265"];
        let args = assemble_ffmpeg_args_for(
            &dv_parts("out.mp4", &video, true, false),
            None,
            TargetOs::Linux,
        );
        assert_eq!(arg_after(&args, "-tag:v").as_deref(), Some("hvc1"));
    }

    #[test]
    fn copy_dv_mp4_has_strict() {
        let video = ["-c:v", "copy"];
        let args = assemble_ffmpeg_args_for(
            &dv_parts("out.mp4", &video, true, false),
            None,
            TargetOs::Linux,
        );
        assert_eq!(arg_after(&args, "-strict").as_deref(), Some("unofficial"));
    }

    #[test]
    fn copy_non_dv_mp4_no_strict() {
        let video = ["-c:v", "copy"];
        let args = assemble_ffmpeg_args_for(
            &dv_parts("out.mp4", &video, false, false),
            None,
            TargetOs::Linux,
        );
        assert!(arg_after(&args, "-strict").is_none());
    }
    #[test]
    fn copy_from_mp4_keeps_source_tag() {
        let video = ["-c:v", "copy"];
        let mut parts = dv_parts("out.mp4", &video, true, false);
        parts.keep_source_tag = true;
        let args = assemble_ffmpeg_args_for(&parts, None, TargetOs::Linux);
        assert!(arg_after(&args, "-tag:v").is_none());
        assert_eq!(arg_after(&args, "-strict").as_deref(), Some("unofficial"));
    }

    // ── Native DV: input filter for RPUs followed by parameter sets ──

    #[test]
    fn native_dv_input_filter_only_for_encoded_hevc_sources() {
        assert_eq!(dovi_input_bsf(true, false, "hevc"), Some(DOVI_INPUT_BSF));
        // A stream copy keeps every packet as it is.
        assert_eq!(dovi_input_bsf(true, true, "hevc"), None);
        // RPU pipeline, HDR10 fallback and non-DV sources decode as before.
        assert_eq!(dovi_input_bsf(false, false, "hevc"), None);
        // Profile 10 is AV1: the HEVC filters would reject the stream.
        assert_eq!(dovi_input_bsf(true, false, "av1"), None);
    }

    #[test]
    fn input_video_bsf_is_an_input_option_on_the_mapped_stream() {
        let video = ["-c:v", "libx265", "-dolbyvision", "1"];
        let mut parts = dv_parts("out.mp4", &video, true, true);
        let without = assemble_ffmpeg_args_for(&parts, None, TargetOs::Linux);
        assert!(!without.iter().any(|a| a.starts_with("-bsf")));

        parts.input_video_bsf = Some(DOVI_INPUT_BSF);
        let args = assemble_ffmpeg_args_for(&parts, None, TargetOs::Linux);
        let bsf = args.iter().position(|a| a == "-bsf:v:0").unwrap();
        let input = args.iter().position(|a| a == "-i").unwrap();
        assert!(bsf < input, "an output -bsf would not reach the decoder");
        assert_eq!(args[bsf + 1], DOVI_INPUT_BSF);
        assert_eq!(arg_after(&args, "-map").as_deref(), Some("0:v:0"));
        // Nothing else changes.
        let mut rest = args.clone();
        rest.drain(bsf..bsf + 2);
        assert_eq!(rest, without);
    }

    /// The dv-compare self-test re-encodes its fixture with this filter.
    #[test]
    fn dv_compare_selftest_uses_the_same_input_filter() {
        let selftest = include_str!("../../tools/dv-compare/dvcompare/selftest.py");
        assert!(selftest.contains(&format!("\"{DOVI_INPUT_BSF}\"")));
    }

    // ── Frame-rate mode ─────────────────────────────────────────

    fn fps_args(parts: &FfmpegAssemblyParts<'_>) -> Vec<String> {
        assemble_ffmpeg_args_for(parts, None, TargetOs::Linux)
    }

    /// `-fps_mode passthrough` (or legacy `-vsync passthrough`) as an output
    /// option: after the input, before the output path.
    fn has_frame_sync(args: &[String], flag: &str) -> bool {
        let input = args.iter().position(|a| a == "-i").unwrap();
        args.windows(2)
            .enumerate()
            .any(|(i, w)| i > input && w[0] == flag && w[1] == "passthrough")
    }

    #[test]
    fn fps_mode_passthrough_for_normal_video() {
        for (enc, out) in [
            ("libx265", "o.mp4"),
            ("libx264", "o.mkv"),
            ("libsvtav1", "o.mkv"),
            ("hevc_nvenc", "o.mkv"),
            ("hevc_vaapi", "o.mp4"),
        ] {
            let video = ["-c:v", enc];
            let args = fps_args(&FfmpegAssemblyParts {
                input_path: "in.mkv",
                output_path: out,
                video_args: &video,
                pix_fmt: "yuv420p",
                codec_family: "hevc",
                ..Default::default()
            });
            assert!(has_frame_sync(&args, "-fps_mode"), "{enc}: {args:?}");
            assert!(!args.contains(&"-vsync".to_string()), "{enc}");
            assert_eq!(args.last().unwrap(), out);
        }
    }

    #[test]
    fn fps_mode_passthrough_for_dolby_vision_and_hdr10plus_routes() {
        // Native Dolby Vision, MP4 and MKV.
        let dv = ["-c:v", "libx265", "-dolbyvision", "1"];
        for (out, in_out) in [("o.mp4", true), ("o.mkv", false)] {
            let args = fps_args(&dv_parts(out, &dv, in_out, false));
            assert!(has_frame_sync(&args, "-fps_mode"), "{out}");
        }
        // Native HDR10+ (dhdr10-info through -x265-params).
        let hdr10p = ["-c:v", "libx265", "-x265-params", "dhdr10-info=/t/a.json"];
        let args = fps_args(&dv_parts("o.mkv", &hdr10p, false, false));
        assert!(has_frame_sync(&args, "-fps_mode"));
        // RPU pipeline and HDR10+ injector: a hardware encode and a software one.
        for enc in ["hevc_vaapi", "libx265"] {
            let video = ["-c:v", enc];
            let args = fps_args(&dv_parts("o.mp4", &video, false, false));
            assert!(has_frame_sync(&args, "-fps_mode"), "{enc}");
        }
        // Both passes of a two-pass encode keep it.
        let base = fps_args(&dv_parts("o.mp4", &dv, true, false));
        for pass in [1u8, 2] {
            let args = build_two_pass_args(&base, pass, "log", "mp4");
            assert!(has_frame_sync(&args, "-fps_mode"), "pass {pass}");
        }
    }

    #[test]
    fn fps_mode_left_alone_for_image_sources_and_stream_copy() {
        // GIF/APNG/MJPEG sources keep ffmpeg's default mode.
        let video = ["-c:v", "libx265"];
        let args = fps_args(&FfmpegAssemblyParts {
            input_path: "in.gif",
            output_path: "o.mp4",
            video_args: &video,
            pix_fmt: "yuv420p",
            codec_family: "hevc",
            is_image_source: true,
            ..Default::default()
        });
        assert!(!args.contains(&"-fps_mode".to_string()), "{args:?}");
        assert!(!args.contains(&"-vsync".to_string()), "{args:?}");
        // A stream copy keeps every packet already.
        let copy = ["-c:v", "copy"];
        let args = fps_args(&dv_parts("o.mp4", &copy, true, false));
        assert!(!args.contains(&"-fps_mode".to_string()), "{args:?}");
    }

    #[test]
    fn fps_mode_falls_back_to_vsync_before_ffmpeg_5_1() {
        let video = ["-c:v", "libx264"];
        let args = fps_args(&FfmpegAssemblyParts {
            input_path: "in.mkv",
            output_path: "o.mp4",
            video_args: &video,
            pix_fmt: "yuv420p",
            codec_family: "h264",
            legacy_vsync: true,
            ..Default::default()
        });
        assert!(has_frame_sync(&args, "-vsync"), "{args:?}");
        assert!(!args.contains(&"-fps_mode".to_string()), "{args:?}");
    }

    #[test]
    fn fps_mode_probe_reads_unrecognised_option() {
        assert!(fps_mode_unrecognised(
            "Unrecognized option 'fps_mode'.\nError splitting the argument list: Option not found\n"
        ));
        assert!(!fps_mode_unrecognised(
            "frame=    1 fps=0.0 q=-0.0 Lsize=N/A\n"
        ));
        assert!(!fps_mode_unrecognised(""));
    }
}
