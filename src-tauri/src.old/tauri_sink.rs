//! GUI implementation of `EventSink` — thin wrapper over `tauri::AppHandle`.
//!
//! Each trait method is a one-liner forwarding to the corresponding
//! `app.emit(...)` call that previously lived inline in the core modules.

use tauri::{AppHandle, Emitter};

use crate::events::EventSink;

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

    fn file_progress(&self, percent: f64, time_secs: f64, total_secs: f64) {
        let _ = self.app.emit(
            "file-progress",
            serde_json::json!({
                "percent": percent,
                "timeSecs": time_secs,
                "totalSecs": total_secs,
            }),
        );
    }

    fn batch_progress(&self, current: u32, total: usize) {
        let _ = self.app.emit(
            "batch-progress",
            serde_json::json!({
                "current": current,
                "total": total,
            }),
        );
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
            serde_json::json!({
                "done": done,
                "failed": failed,
                "skipped": skipped,
                "duration": duration,
            }),
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
            serde_json::json!({
                "action": action,
                "countdown": countdown,
            }),
        );
    }
}