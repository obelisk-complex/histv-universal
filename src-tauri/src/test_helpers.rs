//! Shared test utilities for the histv library crate.
//!
//! Provides mock implementations of `EventSink` and `BatchControl`,
//! factory functions for `BatchSettings` and `ProbeResult`, and other
//! helpers used across multiple test modules.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use crate::encoder::{BatchSettings, EncoderInfo};
use crate::events::{BatchControl, EventSink};
use crate::probe::ProbeResult;
use crate::queue::{AudioStreamInfo, QueueItem};

// ── NoopSink ────────────────────────────────────────────────────

/// `EventSink` that discards all events. Use when a function requires
/// a sink but the test doesn't need to inspect output.
pub struct NoopSink;

impl EventSink for NoopSink {
    fn log(&self, _: &str) {}
    fn file_progress(&self, _: f64, _: f64, _: f64, _: Option<(u8, u8)>) {}
    fn batch_progress(&self, _: u32, _: usize) {}
    fn batch_status(&self, _: &str) {}
    fn queue_item_updated(&self, _: usize, _: &str) {}
    fn queue_item_probed(&self, _: usize, _: &QueueItem) {}
    fn batch_started(&self) {}
    fn batch_finished(&self, _: u32, _: u32, _: u32, _: &str) {}
    fn ffmpeg_stderr(&self, _: &str) {}
    fn batch_command(&self, _: &str) {}
    fn ffmpeg_download_progress(&self, _: &str) {}
    fn toast(&self, _: &str) {}
    fn post_batch(&self, _: &str, _: u32) {}
}

// ── RecordingSink ──────���────────────────────────────────────────

/// `EventSink` that records all `log()` calls for later assertion.
pub struct RecordingSink {
    pub logs: Mutex<Vec<String>>,
}

impl RecordingSink {
    pub fn new() -> Self {
        Self {
            logs: Mutex::new(Vec::new()),
        }
    }

    /// Return a snapshot of all recorded log messages.
    pub fn take_logs(&self) -> Vec<String> {
        self.logs.lock().unwrap().clone()
    }
}

impl Default for RecordingSink {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for RecordingSink {
    fn log(&self, message: &str) {
        self.logs.lock().unwrap().push(message.to_string());
    }
    fn file_progress(&self, _: f64, _: f64, _: f64, _: Option<(u8, u8)>) {}
    fn batch_progress(&self, _: u32, _: usize) {}
    fn batch_status(&self, _: &str) {}
    fn queue_item_updated(&self, _: usize, _: &str) {}
    fn queue_item_probed(&self, _: usize, _: &QueueItem) {}
    fn batch_started(&self) {}
    fn batch_finished(&self, _: u32, _: u32, _: u32, _: &str) {}
    fn ffmpeg_stderr(&self, _: &str) {}
    fn batch_command(&self, _: &str) {}
    fn ffmpeg_download_progress(&self, _: &str) {}
    fn toast(&self, _: &str) {}
    fn post_batch(&self, _: &str, _: u32) {}
}

// ── NoopBatchControl ────────���───────────────────────────────────

/// `BatchControl` that never cancels, never pauses, always overwrites.
pub struct NoopBatchControl;

impl BatchControl for NoopBatchControl {
    fn should_cancel_current(&self) -> bool {
        false
    }
    fn should_cancel_all(&self) -> bool {
        false
    }
    fn is_paused(&self) -> bool {
        false
    }
    fn clear_cancel_current(&self) {}
    fn overwrite_always(&self) -> bool {
        true
    }
    fn set_overwrite_always(&self) {}
    fn overwrite_prompt(&self, _: &str) -> String {
        "yes".to_string()
    }
    fn hw_fallback_offered(&self) -> bool {
        false
    }
    fn set_hw_fallback_offered(&self) {}
    fn fallback_prompt(&self, _: &str) -> String {
        "yes".to_string()
    }
}

// ── CancellableBatchControl ───���─────────────────────────────────

/// `BatchControl` with atomic flags that tests can flip mid-execution
/// to simulate user cancellation.
pub struct CancellableBatchControl {
    pub cancel_current: AtomicBool,
    pub cancel_all: AtomicBool,
    pub paused: AtomicBool,
}

impl CancellableBatchControl {
    pub fn new() -> Self {
        Self {
            cancel_current: AtomicBool::new(false),
            cancel_all: AtomicBool::new(false),
            paused: AtomicBool::new(false),
        }
    }
}

impl Default for CancellableBatchControl {
    fn default() -> Self {
        Self::new()
    }
}

impl BatchControl for CancellableBatchControl {
    fn should_cancel_current(&self) -> bool {
        self.cancel_current.load(Ordering::SeqCst)
    }
    fn should_cancel_all(&self) -> bool {
        self.cancel_all.load(Ordering::SeqCst)
    }
    fn is_paused(&self) -> bool {
        self.paused.load(Ordering::SeqCst)
    }
    fn clear_cancel_current(&self) {
        self.cancel_current.store(false, Ordering::SeqCst);
    }
    fn overwrite_always(&self) -> bool {
        true
    }
    fn set_overwrite_always(&self) {}
    fn overwrite_prompt(&self, _: &str) -> String {
        "yes".to_string()
    }
    fn hw_fallback_offered(&self) -> bool {
        false
    }
    fn set_hw_fallback_offered(&self) {}
    fn fallback_prompt(&self, _: &str) -> String {
        "yes".to_string()
    }
}

// ── CapturingSink ───────────────────────────────────────────────

/// `EventSink` that records every `queue_item_probed` call.
/// Use this to pin the contract: index forwarded correctly, and the
/// `QueueItem` snapshot reflects the post-mutation state (#3c).
pub struct CapturingSink {
    pub probed: Mutex<Vec<(usize, QueueItem)>>,
}

impl CapturingSink {
    pub fn new() -> Self {
        Self {
            probed: Mutex::new(Vec::new()),
        }
    }

    /// Return a snapshot of all recorded probed events.
    pub fn take_probed(&self) -> Vec<(usize, QueueItem)> {
        self.probed.lock().unwrap().clone()
    }
}

impl Default for CapturingSink {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for CapturingSink {
    fn log(&self, _: &str) {}
    fn file_progress(&self, _: f64, _: f64, _: f64, _: Option<(u8, u8)>) {}
    fn batch_progress(&self, _: u32, _: usize) {}
    fn batch_status(&self, _: &str) {}
    fn queue_item_updated(&self, _: usize, _: &str) {}
    fn queue_item_probed(&self, index: usize, item: &QueueItem) {
        self.probed.lock().unwrap().push((index, item.clone()));
    }
    fn batch_started(&self) {}
    fn batch_finished(&self, _: u32, _: u32, _: u32, _: &str) {}
    fn ffmpeg_stderr(&self, _: &str) {}
    fn batch_command(&self, _: &str) {}
    fn ffmpeg_download_progress(&self, _: &str) {}
    fn toast(&self, _: &str) {}
    fn post_batch(&self, _: &str, _: u32) {}
}

// ── Factory functions ──────────���────────────────────────────────

/// Default `BatchSettings` suitable for most unit tests.
pub fn default_settings() -> BatchSettings {
    BatchSettings {
        output_folder: "output".to_string(),
        output_mode: "folder".to_string(),
        threshold: 4.0,
        qp_i: 20,
        qp_p: 22,
        crf_val: 20,
        rate_control_mode: "QP".to_string(),
        pix_fmt: "yuv420p".to_string(),
        delete_source: false,
        save_log: false,
        post_command: None,
        peak_multiplier: 1.5,
        threads: 0,
        low_priority: false,
        precision_mode: false,
        compatibility_mode: false,
        preserve_av1: false,
        force_local: false,
        video_encoder: "auto".to_string(),
        codec_family: "auto".to_string(),
        audio_encoder: "auto".to_string(),
        audio_cap: 640,
        output_container: "auto".to_string(),
    }
}

/// Default detected encoders: NVENC HEVC/H264 + libsvtav1.
pub fn default_encoders() -> Vec<EncoderInfo> {
    vec![
        EncoderInfo {
            name: "hevc_nvenc".to_string(),
            codec_family: "hevc".to_string(),
            is_hardware: true,
        },
        EncoderInfo {
            name: "h264_nvenc".to_string(),
            codec_family: "h264".to_string(),
            is_hardware: true,
        },
        EncoderInfo {
            name: "libsvtav1".to_string(),
            codec_family: "av1".to_string(),
            is_hardware: false,
        },
    ]
}

/// Build a synthetic `ProbeResult` without running ffprobe.
///
/// # Arguments
/// - `codec` - e.g. "hevc", "h264", "av1", "gif"
/// - `bitrate_mbps` - video bitrate in Mbps (0.0 for unknown)
/// - `duration_secs` - file duration
/// - `hdr` - whether the source is HDR
pub fn synthetic_probe(
    codec: &str,
    bitrate_mbps: f64,
    duration_secs: f64,
    hdr: bool,
) -> ProbeResult {
    ProbeResult {
        video_codec: codec.to_string(),
        video_width: 1920,
        video_height: 1080,
        video_bitrate_bps: bitrate_mbps * 1_000_000.0,
        video_bitrate_mbps: bitrate_mbps,
        is_hdr: hdr,
        color_transfer: if hdr {
            "smpte2084".to_string()
        } else {
            "bt709".to_string()
        },
        audio_streams: vec![AudioStreamInfo {
            index: 0,
            codec: "aac".to_string(),
            bitrate_kbps: 128,
        }],
        duration_secs,
        dovi_profile: None,
        dovi_bl_compat_id: None,
        has_hdr10plus: false,
        video_fps: 23.976,
        subtitle_stream_count: 0,
    }
}

/// Build a `QueueItem` with the given path, status, and probe data.
pub fn make_queue_item(
    path: &str,
    status: crate::queue::QueueItemStatus,
    probe: ProbeResult,
) -> crate::queue::QueueItem {
    let file_name = std::path::Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let base_name = std::path::Path::new(path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let source_bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    crate::queue::QueueItem {
        full_path: path.to_string(),
        file_name,
        base_name,
        status,
        source_bytes,
        probe,
    }
}

/// Build a `QueueItem` with an explicit `source_bytes` value (for tests
/// that need a specific size without a real file on disk).
pub fn make_queue_item_sized(
    path: &str,
    status: crate::queue::QueueItemStatus,
    probe: ProbeResult,
    source_bytes: u64,
) -> crate::queue::QueueItem {
    let file_name = std::path::Path::new(path)
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    let base_name = std::path::Path::new(path)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .to_string();
    crate::queue::QueueItem {
        full_path: path.to_string(),
        file_name,
        base_name,
        status,
        source_bytes,
        probe,
    }
}
