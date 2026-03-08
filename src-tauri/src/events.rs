//! Output event abstraction layer.
//!
//! The `EventSink` trait decouples core modules (encoder, probe, ffmpeg) from
//! any specific UI framework. The GUI implements it via `TauriSink` (wrapping
//! `app.emit`); the CLI will implement it as terminal output. All methods are
//! fire-and-forget — they never return data to the caller.

/// One-way output channel for logging, progress, and status events.
///
/// Every place the core modules previously called `app.emit(...)` now calls
/// a method on this trait instead. Prompts (overwrite, fallback) are excluded
/// — they are bidirectional and belong on the `BatchControl` trait (Phase 3).
pub trait EventSink: Send + Sync {
    fn log(&self, message: &str);
    fn file_progress(&self, percent: f64, time_secs: f64, total_secs: f64);
    fn batch_progress(&self, current: u32, total: usize);
    fn batch_status(&self, message: &str);
    fn queue_item_updated(&self, index: usize, status: &str);
    fn queue_item_probed(&self, index: usize);
    fn batch_started(&self);
    fn batch_finished(&self, done: u32, failed: u32, skipped: u32, duration: &str);
    fn ffmpeg_stderr(&self, line: &str);
    fn batch_command(&self, cmd: &str);
    fn ffmpeg_download_progress(&self, message: &str);
    fn toast(&self, message: &str);
    fn post_batch(&self, action: &str, countdown: u32);
}
