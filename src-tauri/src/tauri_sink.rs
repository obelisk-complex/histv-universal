//! GUI implementation of `EventSink` - thin wrapper over `tauri::AppHandle`.
//!
//! Each trait method is a one-liner forwarding to the corresponding
//! `app.emit(...)` call that previously lived inline in the core modules.
//! Event payloads use #[derive(Serialize)] structs instead of the json!
//! macro to avoid per-tick Map/Value heap allocations (#16).

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::events::EventSink;

// ── Typed event payloads (#16) ─────────────────────────────────

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct FileProgressPayload {
    percent: f64,
    time_secs: f64,
    total_secs: f64,
    pass: Option<PassInfo>,
}

#[derive(Serialize, Clone)]
struct PassInfo {
    current: u8,
    total: u8,
}

#[derive(Serialize, Clone)]
struct BatchProgressPayload {
    current: u32,
    total: usize,
}

#[derive(Serialize, Clone)]
struct BatchFinishedPayload {
    done: u32,
    failed: u32,
    skipped: u32,
    duration: String,
}

#[derive(Serialize, Clone)]
struct PostBatchPayload {
    action: String,
    countdown: u32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct WaveProgressPayload {
    wave: u32,
    total_waves: u32,
    file_in_wave: u32,
    wave_size: u32,
}

#[derive(Serialize, Clone)]
#[serde(rename_all = "camelCase")]
struct TimeEstimatePayload {
    elapsed_secs: f64,
    remaining_secs: f64,
}

/// Wraps a Tauri `AppHandle` to satisfy the `EventSink` trait.
pub struct TauriSink {
    app: AppHandle,
}

impl TauriSink {
    pub fn new(app: AppHandle) -> Self {
        Self { app }
    }
}

impl EventSink for TauriSink {
    fn log(&self, message: &str) {
        let _ = self.app.emit("log", message);
    }

    fn file_progress(&self, percent: f64, time_secs: f64, total_secs: f64, pass: Option<(u8, u8)>) {
        let _ = self.app.emit(
            "file-progress",
            FileProgressPayload {
                percent,
                time_secs,
                total_secs,
                pass: pass.map(|(cur, tot)| PassInfo {
                    current: cur,
                    total: tot,
                }),
            },
        );
    }

    fn batch_progress(&self, current: u32, total: usize) {
        let _ = self
            .app
            .emit("batch-progress", BatchProgressPayload { current, total });
    }

    fn batch_status(&self, message: &str) {
        let _ = self.app.emit("batch-status", message);
    }

    fn queue_item_updated(&self, index: usize, status: &str) {
        let _ = self.app.emit("queue-item-updated", (index, status));
    }

    fn queue_item_probed(&self, index: usize) {
        let _ = self.app.emit("queue-item-probed", index);
    }

    fn batch_started(&self) {
        let _ = self.app.emit("batch-started", ());
    }

    fn batch_finished(&self, done: u32, failed: u32, skipped: u32, duration: &str) {
        let _ = self.app.emit(
            "batch-finished",
            BatchFinishedPayload {
                done,
                failed,
                skipped,
                duration: duration.to_string(),
            },
        );
    }

    fn ffmpeg_stderr(&self, line: &str) {
        let _ = self.app.emit("ffmpeg-stderr", line);
    }

    fn batch_command(&self, cmd: &str) {
        let _ = self.app.emit("batch-command", cmd);
    }

    fn ffmpeg_download_progress(&self, message: &str) {
        let _ = self.app.emit("ffmpeg-download-progress", message);
    }

    fn toast(&self, message: &str) {
        let _ = self.app.emit("toast", message);
    }

    fn post_batch(&self, action: &str, countdown: u32) {
        let _ = self.app.emit(
            "post-batch",
            PostBatchPayload {
                action: action.to_string(),
                countdown,
            },
        );
    }

    // ── Wave-based staging events (Phase 3) ───────────────────

    fn wave_progress(&self, wave: u32, total_waves: u32, file_in_wave: u32, wave_size: u32) {
        let _ = self.app.emit(
            "wave-progress",
            WaveProgressPayload {
                wave,
                total_waves,
                file_in_wave,
                wave_size,
            },
        );
    }

    fn wave_status(&self, message: &str) {
        let _ = self.app.emit("wave-status", message);
    }

    fn batch_time_estimate(&self, elapsed_secs: f64, remaining_secs: f64) {
        let _ = self.app.emit(
            "batch-time-estimate",
            TimeEstimatePayload {
                elapsed_secs,
                remaining_secs,
            },
        );
    }

    fn wave_time_estimate(&self, elapsed_secs: f64, remaining_secs: f64) {
        let _ = self.app.emit(
            "wave-time-estimate",
            TimeEstimatePayload {
                elapsed_secs,
                remaining_secs,
            },
        );
    }
}
