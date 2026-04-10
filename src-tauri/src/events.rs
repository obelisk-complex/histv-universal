//! Output event abstraction layer.
//!
//! The `EventSink` trait decouples core modules (encoder, probe, ffmpeg) from
//! any specific UI framework. The GUI implements it via `TauriSink` (wrapping
//! `app.emit`); the CLI will implement it as terminal output. All methods are
//! fire-and-forget — they never return data to the caller.

use crate::queue::QueueItem;

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
    /// Emitted after a queue item's probe (or repair) completes. The full
    /// post-probe `QueueItem` snapshot travels with the event so the frontend
    /// can patch a single row without refetching the whole queue (#3c).
    fn queue_item_probed(&self, index: usize, item: &QueueItem);
    fn batch_started(&self);
    fn batch_finished(&self, done: u32, failed: u32, skipped: u32, duration: &str);
    fn ffmpeg_stderr(&self, line: &str);
    fn batch_command(&self, cmd: &str);
    fn ffmpeg_download_progress(&self, message: &str);
    fn toast(&self, message: &str);
    fn post_batch(&self, action: &str, countdown: u32);

    // ── Wave-based staging events (Phase 3) ───────────────────
    fn wave_progress(&self, _wave: u32, _total_waves: u32, _file_in_wave: u32, _wave_size: u32) {}
    fn wave_status(&self, _message: &str) {}
    fn batch_time_estimate(&self, _elapsed_secs: f64, _remaining_secs: f64) {}
    fn wave_time_estimate(&self, _elapsed_secs: f64, _remaining_secs: f64) {}
}

/// Blanket impl: `Arc<T>` delegates to the inner `T`, allowing shared
/// ownership of sinks across async tasks without trait-object indirection.
impl<T: EventSink> EventSink for std::sync::Arc<T> {
    fn log(&self, message: &str) {
        (**self).log(message)
    }
    fn file_progress(&self, p: f64, t: f64, d: f64, pass: Option<(u8, u8)>) {
        (**self).file_progress(p, t, d, pass)
    }
    fn batch_progress(&self, current: u32, total: usize) {
        (**self).batch_progress(current, total)
    }
    fn batch_status(&self, message: &str) {
        (**self).batch_status(message)
    }
    fn queue_item_updated(&self, index: usize, status: &str) {
        (**self).queue_item_updated(index, status)
    }
    fn queue_item_probed(&self, index: usize, item: &QueueItem) {
        (**self).queue_item_probed(index, item)
    }
    fn batch_started(&self) {
        (**self).batch_started()
    }
    fn batch_finished(&self, d: u32, f: u32, s: u32, dur: &str) {
        (**self).batch_finished(d, f, s, dur)
    }
    fn ffmpeg_stderr(&self, line: &str) {
        (**self).ffmpeg_stderr(line)
    }
    fn batch_command(&self, cmd: &str) {
        (**self).batch_command(cmd)
    }
    fn ffmpeg_download_progress(&self, message: &str) {
        (**self).ffmpeg_download_progress(message)
    }
    fn toast(&self, message: &str) {
        (**self).toast(message)
    }
    fn post_batch(&self, action: &str, countdown: u32) {
        (**self).post_batch(action, countdown)
    }
    fn wave_progress(&self, w: u32, tw: u32, f: u32, ws: u32) {
        (**self).wave_progress(w, tw, f, ws)
    }
    fn wave_status(&self, message: &str) {
        (**self).wave_status(message)
    }
    fn batch_time_estimate(&self, e: f64, r: f64) {
        (**self).batch_time_estimate(e, r)
    }
    fn wave_time_estimate(&self, e: f64, r: f64) {
        (**self).wave_time_estimate(e, r)
    }
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

// ── Unit tests ─────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::{QueueItem, QueueItemStatus};
    use crate::test_helpers::{synthetic_probe, CapturingSink};
    use std::sync::Arc;

    fn make_item(path: &str, status: QueueItemStatus) -> QueueItem {
        QueueItem {
            full_path: path.to_string(),
            file_name: "test.mkv".to_string(),
            base_name: "test".to_string(),
            status,
            source_bytes: 0,
            probe: synthetic_probe("hevc", 8.0, 60.0, false),
        }
    }

    // ── queue_item_probed contract (#3c) ─────────────────────────

    /// The sink receives exactly one call with the correct index and the
    /// post-mutation item state. This pins the #3c contract so that a
    /// revert to the old bare-index payload or wrong index would be caught.
    #[test]
    fn test_queue_item_probed_captures_index_and_item() {
        let sink = CapturingSink::new();
        let item = make_item("/videos/movie.mkv", QueueItemStatus::Pending);

        sink.queue_item_probed(3, &item);

        let captured = sink.take_probed();
        assert_eq!(captured.len(), 1, "expected exactly one probed event");
        let (idx, captured_item) = &captured[0];
        assert_eq!(*idx, 3, "index must be forwarded unchanged");
        assert_eq!(captured_item.full_path, "/videos/movie.mkv");
        assert!(
            matches!(captured_item.status, QueueItemStatus::Pending),
            "item status must reflect post-mutation state"
        );
    }

    /// Arc<CapturingSink> delegates through the blanket impl — verifies
    /// the Arc wrapper does not silently swallow the call.
    #[test]
    fn test_queue_item_probed_arc_delegation() {
        let sink = Arc::new(CapturingSink::new());
        let item = make_item("/videos/arc_test.mkv", QueueItemStatus::Pending);

        sink.queue_item_probed(7, &item);

        let captured = sink.take_probed();
        assert_eq!(captured.len(), 1);
        assert_eq!(captured[0].0, 7);
        assert_eq!(captured[0].1.full_path, "/videos/arc_test.mkv");
    }

    /// Multiple sequential calls all land in the sink in order, preserving
    /// the index for each.
    #[test]
    fn test_queue_item_probed_multiple_calls_ordered() {
        let sink = CapturingSink::new();
        for i in 0..5usize {
            let item = make_item("/videos/file.mkv", QueueItemStatus::Pending);
            sink.queue_item_probed(i, &item);
        }
        let captured = sink.take_probed();
        assert_eq!(captured.len(), 5);
        for (i, (idx, _)) in captured.iter().enumerate() {
            assert_eq!(*idx, i);
        }
    }
}
