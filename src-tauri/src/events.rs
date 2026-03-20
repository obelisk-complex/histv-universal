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
    fn file_progress(&self, percent: f64, time_secs: f64, total_secs: f64, pass: Option<(u8, u8)>);
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

/// Bidirectional batch control interface for cancellation, overwrite policy,
/// and HW fallback policy. Unlike `EventSink` (one-way output), this trait
/// handles interactive decisions and mutable state.
///
/// The GUI implements this by locking `Mutex<BatchState>` and using the
/// existing emit-and-poll pattern for prompts. The CLI implements it with
/// `AtomicBool` fields (signal-handler-safe) and TTY prompts or flag-based
/// defaults.
pub trait BatchControl: Send + Sync {
    // ── Cancellation / pause ───────────────────────────────────
    fn should_cancel_current(&self) -> bool;
    fn should_cancel_all(&self) -> bool;
    fn is_paused(&self) -> bool;
    fn clear_cancel_current(&self);

    // ── Overwrite policy ───────────────────────────────────────
    fn overwrite_always(&self) -> bool;
    fn set_overwrite_always(&self);
    /// Prompt the user about an overwrite conflict.
    /// Returns "yes" | "no" | "always" | "cancel".
    fn overwrite_prompt(&self, path: &str) -> String;

    // ── HW fallback policy ─────────────────────────────────────
    fn hw_fallback_offered(&self) -> bool;
    fn set_hw_fallback_offered(&self);
    /// Prompt the user about falling back to software encoding.
    /// Returns "yes" | "no".
    fn fallback_prompt(&self, filename: &str) -> String;
}