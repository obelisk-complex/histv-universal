//! CLI implementation of `EventSink` - writes to stderr with optional
//! indicatif progress bars when a TTY is attached.
//!
//! All output goes to stderr so stdout remains clean for piped usage.
//! Log level filtering controls verbosity: quiet (errors only),
//! normal (key events), verbose (everything including ffmpeg stderr).
//!
//! In TTY normal mode, a persistent live display shows a windowed queue
//! table with progress bar and batch timing, similar to the GUI.
//!
//! Mutex locks use `unwrap_or_else(|e| e.into_inner())` throughout this
//! module to recover from poisoned mutexes rather than cascade-panicking.
//! A poisoned mutex means another thread panicked while holding the lock,
//! but the inner data (a progress bar handle) is still safe to use.

use std::io::Write;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use indicatif::{ProgressBar, ProgressStyle};

use histv_lib::events::EventSink;

use crate::cli::LogLevel;

// ── Live display constants ───────────────────────────────────────

/// Maximum completed items shown above the current file.
const WINDOW_COMPLETED: usize = 3;
/// Maximum pending items shown below the current file.
const WINDOW_UPCOMING: usize = 3;
/// Minimum interval between progress-driven redraws.
const MIN_REDRAW_MS: u64 = 100;

// ── Live display types ───────────────────────────────────────────

/// Display status for a queue item in the live table.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DisplayStatus {
    Pending,
    Encoding,
    Done,
    Failed,
    Skipped,
    Cancelled,
}

impl DisplayStatus {
    fn from_str(s: &str) -> Self {
        match s {
            "Encoding" => Self::Encoding,
            "Done" => Self::Done,
            "Failed" => Self::Failed,
            "Skipped" => Self::Skipped,
            "Cancelled" => Self::Cancelled,
            _ => Self::Pending,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Encoding => "Encoding",
            Self::Done => "Done",
            Self::Failed => "Failed",
            Self::Skipped => "Skipped",
            Self::Cancelled => "Cancelled",
        }
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Done | Self::Failed | Self::Skipped | Self::Cancelled
        )
    }
}

/// Snapshot of a queue item for the live display table.
pub(crate) struct DisplayItem {
    pub(crate) file_name: String,
    pub(crate) source_size: String,
    pub(crate) estimated_size: String,
    pub(crate) resolution: String,
    pub(crate) hdr_label: &'static str,
    pub(crate) source_br: String,
    pub(crate) target_br: String,
    pub(crate) status: DisplayStatus,
}

/// Persistent live display state, updated via EventSink callbacks.
struct LiveDisplay {
    items: Vec<DisplayItem>,
    current_index: Option<usize>,
    file_counter: u32,
    total_files: usize,
    progress_percent: f64,
    progress_time: f64,
    progress_total: f64,
    progress_pass: Option<(u8, u8)>,
    batch_elapsed: f64,
    batch_remaining: f64,
    last_warning: Option<String>,
    last_draw_lines: u32,
    last_draw_instant: Instant,
}

impl LiveDisplay {
    fn new(items: Vec<DisplayItem>) -> Self {
        let total = items.len();
        Self {
            items,
            current_index: None,
            file_counter: 0,
            total_files: total,
            progress_percent: 0.0,
            progress_time: 0.0,
            progress_total: 0.0,
            progress_pass: None,
            batch_elapsed: 0.0,
            batch_remaining: 0.0,
            last_warning: None,
            last_draw_lines: 0,
            last_draw_instant: Instant::now()
                .checked_sub(std::time::Duration::from_secs(1))
                .unwrap_or_else(Instant::now),
        }
    }

    /// Draw the live display. Returns the buffer to write to stderr.
    fn draw(&mut self, is_tty: bool) -> String {
        let term_width = if is_tty {
            console::Term::stderr()
                .size()
                .1
                .max(60) as usize
        } else {
            120
        };

        let mut buf = String::with_capacity(2048);

        // ── Move cursor up to overwrite previous draw ────────
        if self.last_draw_lines > 0 {
            buf.push_str(&format!("\x1b[{}A", self.last_draw_lines));
        }

        let mut lines: u32 = 0;

        // ── Header ───────────────────────────────────────────
        let header = format!(
            "Queue [{}/{}]",
            self.file_counter, self.total_files
        );
        buf.push_str(&format!("\x1b[1m{}\x1b[0m\x1b[K\n", header));
        lines += 1;

        // ── Column headers ───────────────────────────────────
        // Dynamic filename width: total - fixed columns
        let fixed_cols = 2 + 9 + 2 + 10 + 2 + 10 + 2 + 6 + 2 + 10 + 2 + 20 + 2 + 10;
        // = 89 for fixed + spacing
        let name_width = term_width.saturating_sub(fixed_cols + 4).max(10); // +4 for "  > " prefix

        buf.push_str(&format!(
            "  \x1b[2m{:<nw$}  {:>9}  {:>10}  {:>10}  {:<6}  {:>10}  {:<20}  {:<10}\x1b[0m\x1b[K\n",
            "File", "From", "To (est.)", "Resolution", "HDR", "From B/R", "To B/R", "Status",
            nw = name_width,
        ));
        lines += 1;

        buf.push_str(&format!("  \x1b[2m{}\x1b[0m\x1b[K\n", "-".repeat(term_width.saturating_sub(2))));
        lines += 1;

        // ── Partition items into completed / current / upcoming ──
        let current_idx = self.current_index.unwrap_or(0);

        // Completed: terminal-status items before current (zero-alloc)
        let completed_count = self.items[..current_idx].iter()
            .filter(|item| item.status.is_terminal())
            .count();
        let hidden_completed = completed_count.saturating_sub(WINDOW_COMPLETED);
        if hidden_completed > 0 {
            buf.push_str(&format!(
                "  \x1b[2m... {} completed\x1b[0m\x1b[K\n",
                hidden_completed
            ));
            lines += 1;
        }

        // Show last WINDOW_COMPLETED completed items
        let completed_indices = (0..current_idx)
            .filter(|&i| self.items[i].status.is_terminal())
            .skip(hidden_completed);
        for idx in completed_indices {
            self.write_item_line(&mut buf, idx, name_width, false);
            lines += 1;
        }

        // Current item
        if self.current_index.is_some() {
            self.write_item_line(&mut buf, current_idx, name_width, true);
            lines += 1;
        }

        // Upcoming: pending items after current (zero-alloc)
        let upcoming_start = current_idx + 1;
        let upcoming_total = self.items[upcoming_start..].iter()
            .filter(|item| item.status == DisplayStatus::Pending)
            .count();
        let upcoming_indices = (upcoming_start..self.items.len())
            .filter(|&i| self.items[i].status == DisplayStatus::Pending)
            .take(WINDOW_UPCOMING);
        for idx in upcoming_indices {
            self.write_item_line(&mut buf, idx, name_width, false);
            lines += 1;
        }

        let hidden_upcoming = upcoming_total.saturating_sub(WINDOW_UPCOMING);
        if hidden_upcoming > 0 {
            buf.push_str(&format!(
                "  \x1b[2m... {} more pending\x1b[0m\x1b[K\n",
                hidden_upcoming
            ));
            lines += 1;
        }

        // ── Progress bar ─────────────────────────────────────
        let bar_width = term_width.saturating_sub(30).max(10);
        let filled = (self.progress_percent / 100.0 * bar_width as f64).round() as usize;
        let empty = bar_width.saturating_sub(filled);
        let elapsed_str = format_duration(self.progress_time);
        let total_str = format_duration(self.progress_total);
        let pass_label = match self.progress_pass {
            Some((cur, tot)) => format!(" (pass {}/{})", cur, tot),
            None => String::new(),
        };

        buf.push_str(&format!(
            "  \x1b[36m{}\x1b[2m{}\x1b[0m {:>3.0}%  {} / {}{}\x1b[K\n",
            "━".repeat(filled),
            "░".repeat(empty),
            self.progress_percent,
            elapsed_str,
            total_str,
            pass_label,
        ));
        lines += 1;

        // ── Warning line (if present) ────────────────────────
        if let Some(ref warning) = self.last_warning {
            let truncated = if warning.len() > term_width - 2 {
                format!("{}...", &warning[..term_width.saturating_sub(5)])
            } else {
                warning.clone()
            };
            buf.push_str(&format!("  \x1b[33m{}\x1b[0m\x1b[K\n", truncated));
            lines += 1;
        }

        // ── Batch timing ─────────────────────────────────────
        if self.batch_elapsed > 0.0 || self.batch_remaining > 0.0 {
            let elapsed = format_duration(self.batch_elapsed);
            let remaining = if self.batch_remaining > 0.0 {
                format!("~{}", format_duration(self.batch_remaining))
            } else {
                "calculating...".to_string()
            };
            buf.push_str(&format!(
                "  Batch: {} elapsed, {} remaining\x1b[K\n",
                elapsed, remaining,
            ));
            lines += 1;
        }

        // ── Clear any leftover lines from previous draw ──────
        if lines < self.last_draw_lines {
            for _ in 0..(self.last_draw_lines - lines) {
                buf.push_str("\x1b[K\n");
            }
            // Move cursor back up to the end of actual content
            let extra = self.last_draw_lines - lines;
            buf.push_str(&format!("\x1b[{}A", extra));
        }

        self.last_draw_lines = lines;
        self.last_draw_instant = Instant::now();
        buf
    }

    /// Write a single item row into the buffer.
    fn write_item_line(&self, buf: &mut String, idx: usize, name_width: usize, is_current: bool) {
        let item = &self.items[idx];
        let prefix = if is_current { "> " } else { "  " };
        let name = crate::truncate_filename(&item.file_name, name_width);

        // Status colouring
        let (status_colour, name_colour, reset) = match item.status {
            DisplayStatus::Done => ("\x1b[32m", "", "\x1b[0m"),       // green
            DisplayStatus::Failed => ("\x1b[31m", "\x1b[31m", "\x1b[0m"), // red
            DisplayStatus::Encoding => ("\x1b[36m", "\x1b[1m", "\x1b[0m"), // cyan/bold
            DisplayStatus::Skipped => ("\x1b[33m", "", "\x1b[0m"),    // yellow
            DisplayStatus::Cancelled => ("\x1b[33m", "", "\x1b[0m"),  // yellow
            DisplayStatus::Pending => ("\x1b[2m", "\x1b[2m", "\x1b[0m"), // dim
        };

        buf.push_str(&format!(
            "{}{}{:<nw$}{}  {:>9}  {:>10}  {:>10}  {:<6}  {:>10}  {:<20}  {}{:<10}{}\x1b[K\n",
            prefix,
            name_colour,
            name,
            reset,
            item.source_size,
            item.estimated_size,
            item.resolution,
            item.hdr_label,
            item.source_br,
            item.target_br,
            status_colour,
            item.status.label(),
            reset,
            nw = name_width,
        ));
    }

    /// Whether enough time has passed for a rate-limited redraw.
    fn should_redraw(&self) -> bool {
        self.last_draw_instant.elapsed().as_millis() >= MIN_REDRAW_MS as u128
    }
}

// ── CliSink ──────────────────────────────────────────────────────

/// Terminal output sink for the CLI.
pub struct CliSink {
    log_level: LogLevel,
    is_tty: bool,
    progress_bar: Mutex<Option<ProgressBar>>,
    /// Cached pass label string (#17). Updated only when the pass value
    /// changes, instead of being formatted on every progress tick.
    cached_pass_label: Mutex<(Option<(u8, u8)>, String)>,
    /// Last percentage printed in non-TTY simple mode, to avoid duplicates.
    last_simple_pct: Mutex<u32>,
    /// Fast flag for checking whether the live display is active without
    /// locking the mutex. Set in `init_live_display`, cleared in `batch_finished`.
    live_active: AtomicBool,
    /// Persistent live display (TTY + normal mode only).
    live_display: Mutex<Option<LiveDisplay>>,
}

impl CliSink {
    pub fn new(log_level: LogLevel) -> Self {
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
        Self {
            log_level,
            is_tty,
            progress_bar: Mutex::new(None),
            cached_pass_label: Mutex::new((None, String::new())),
            last_simple_pct: Mutex::new(u32::MAX),
            live_active: AtomicBool::new(false),
            live_display: Mutex::new(None),
        }
    }

    /// Whether we're in verbose mode.
    fn is_verbose(&self) -> bool {
        matches!(self.log_level, LogLevel::Verbose)
    }

    /// Whether we're in quiet mode (errors only).
    fn is_quiet(&self) -> bool {
        matches!(self.log_level, LogLevel::Quiet)
    }

    /// Whether the live display is active for this session.
    fn live_active(&self) -> bool {
        self.live_active.load(Ordering::Relaxed)
    }

    /// Initialize the live display with pre-computed queue data.
    /// Only activates in TTY normal mode (not verbose, not quiet).
    pub fn init_live_display(&self, items: Vec<DisplayItem>) {
        if !self.is_tty || self.is_verbose() || self.is_quiet() {
            return;
        }
        let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
        *ld = Some(LiveDisplay::new(items));
        self.live_active.store(true, Ordering::Relaxed);
    }

    /// Draw the live display if active. Writes directly to stderr.
    fn draw_live(&self) {
        let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref mut display) = *ld {
            let buf = display.draw(self.is_tty);
            let _ = std::io::stderr().write_all(buf.as_bytes());
            let _ = std::io::stderr().flush();
        }
    }

    /// Write a line to stderr, suspending any active progress bar first
    /// so the output doesn't collide with the bar rendering.
    fn eprintln(&self, msg: &str) {
        let pb = self.progress_bar.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(ref bar) = *pb {
            bar.suspend(|| {
                eprintln!("{}", msg);
            });
        } else {
            eprintln!("{}", msg);
        }
    }

    /// Clear and drop the current progress bar, if any.
    fn clear_progress(&self) {
        let mut pb = self.progress_bar.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(bar) = pb.take() {
            bar.finish_and_clear();
        }
    }

    /// Get the cached pass label, updating it only if the pass value changed (#17).
    fn pass_label(&self, pass: Option<(u8, u8)>) -> String {
        let mut cached = self.cached_pass_label.lock().unwrap_or_else(|e| e.into_inner());
        if cached.0 != pass {
            cached.1 = match pass {
                Some((cur, tot)) => format!(" (pass {}/{})", cur, tot),
                None => String::new(),
            };
            cached.0 = pass;
        }
        cached.1.clone()
    }
}

impl EventSink for CliSink {
    fn log(&self, message: &str) {
        // Live display mode: suppress normal logs, capture warnings/errors
        if self.live_active() {
            let trimmed = message.trim();
            if trimmed.contains("ERROR") || trimmed.contains("WARNING") {
                let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(ref mut display) = *ld {
                    display.last_warning = Some(trimmed.to_string());
                    let buf = display.draw(self.is_tty);
                    drop(ld);
                    let _ = std::io::stderr().write_all(buf.as_bytes());
                    let _ = std::io::stderr().flush();
                }
            }
            return;
        }

        if self.is_quiet() {
            // In quiet mode, only show errors and warnings
            let trimmed = message.trim();
            if trimmed.contains("ERROR") || trimmed.contains("WARNING") {
                self.eprintln(message);
            }
            return;
        }

        // In normal mode, filter out noisy internal messages
        if !self.is_verbose() {
            let trimmed = message.trim();
            // Skip verbose-only messages
            if trimmed.starts_with("  CMD:") || trimmed.starts_with("  ffmpeg PID:") {
                return;
            }
        }

        self.eprintln(message);
    }

    fn file_progress(&self, percent: f64, time_secs: f64, total_secs: f64, pass: Option<(u8, u8)>) {
        if self.is_quiet() {
            return;
        }

        // Live display mode: update state and throttled redraw
        if self.live_active() {
            let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut display) = *ld {
                display.progress_percent = percent;
                display.progress_time = time_secs;
                display.progress_total = total_secs;
                display.progress_pass = pass;
                if display.should_redraw() {
                    let buf = display.draw(self.is_tty);
                    drop(ld);
                    let _ = std::io::stderr().write_all(buf.as_bytes());
                    let _ = std::io::stderr().flush();
                }
            }
            return;
        }

        let pass_label = self.pass_label(pass);

        if self.is_tty {
            // Rich mode: update indicatif progress bar
            let mut pb = self.progress_bar.lock().unwrap_or_else(|e| e.into_inner());
            if pb.is_none() {
                let bar = ProgressBar::new(1000);
                bar.set_style(
                    ProgressStyle::with_template("  {bar:40.cyan/dim} {percent:>3}%  {msg}")
                        .unwrap()
                        .progress_chars("━░"),
                );
                *pb = Some(bar);
            }
            if let Some(ref bar) = *pb {
                let pos = (percent * 10.0).round() as u64; // 0-1000 for smooth movement
                bar.set_position(pos);

                let elapsed = format_duration(time_secs);
                let total = format_duration(total_secs);
                bar.set_message(format!("{} / {}{}", elapsed, total, pass_label));
            }
        } else {
            // Simple mode: print at 10% increments, skip duplicates
            let pct = percent.round() as u32;
            if pct.is_multiple_of(10) {
                let mut last = self.last_simple_pct.lock().unwrap_or_else(|e| e.into_inner());
                if *last != pct {
                    *last = pct;
                    let _ = writeln!(std::io::stderr(), "  {}%{}", pct, pass_label);
                }
            }
        }
    }

    fn batch_progress(&self, current: u32, total: usize) {
        // Live display mode: update counter
        if self.live_active() {
            let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut display) = *ld {
                display.file_counter = current;
                display.total_files = total;
            }
            return;
        }

        if !self.is_quiet() && !self.is_tty {
            // Simple mode: print batch position
            let _ = writeln!(std::io::stderr(), "[{}/{}]", current, total);
        }
        // In TTY mode, batch_status handles the display
    }

    fn batch_status(&self, message: &str) {
        // Live display mode: clear warning for new file, suppress text
        if self.live_active() {
            let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut display) = *ld {
                display.last_warning = None;
                display.progress_percent = 0.0;
                display.progress_time = 0.0;
                display.progress_total = 0.0;
                display.progress_pass = None;
            }
            return;
        }

        if self.is_quiet() {
            return;
        }
        // Clear any existing per-file progress bar before the new file header
        self.clear_progress();
        *self.last_simple_pct.lock().unwrap_or_else(|e| e.into_inner()) = u32::MAX; // reset for new file
        self.eprintln(message);
    }

    fn queue_item_updated(&self, index: usize, status: &str) {
        // Live display mode: update item status and redraw immediately
        if self.live_active() {
            let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut display) = *ld {
                if index < display.items.len() {
                    let new_status = DisplayStatus::from_str(status);
                    display.items[index].status = new_status;
                    if new_status == DisplayStatus::Encoding {
                        display.current_index = Some(index);
                        display.progress_percent = 0.0;
                        display.progress_time = 0.0;
                        display.progress_total = 0.0;
                        display.progress_pass = None;
                    }
                }
                let buf = display.draw(self.is_tty);
                drop(ld);
                let _ = std::io::stderr().write_all(buf.as_bytes());
                let _ = std::io::stderr().flush();
            }
            return;
        }

        // The CLI doesn't maintain a live queue display; status changes
        // are reflected through log messages instead.
        if self.is_verbose() {
            self.eprintln(&format!("  [status] {}", status));
        }
    }

    fn queue_item_probed(&self, _index: usize) {
        // Probing progress is shown via log messages
    }

    fn batch_started(&self) {
        if self.live_active() {
            self.draw_live();
            return;
        }
        if !self.is_quiet() {
            self.eprintln("");
        }
    }

    fn batch_finished(&self, done: u32, failed: u32, skipped: u32, duration: &str) {
        // Clear live display if active
        {
            let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref display) = *ld {
                // Move cursor up and clear the entire live area
                if display.last_draw_lines > 0 {
                    let clear = format!(
                        "\x1b[{}A{}",
                        display.last_draw_lines,
                        "\x1b[K\n".repeat(display.last_draw_lines as usize),
                    );
                    let _ = std::io::stderr().write_all(clear.as_bytes());
                    // Move back up after clearing
                    let _ = write!(
                        std::io::stderr(),
                        "\x1b[{}A",
                        display.last_draw_lines
                    );
                    let _ = std::io::stderr().flush();
                }
            }
            *ld = None;
            self.live_active.store(false, Ordering::Relaxed);
        }

        self.clear_progress();

        // Always show the summary, even in quiet mode
        eprintln!();
        eprintln!(
            "Batch complete. Done: {}, Failed: {}, Skipped: {}. Duration: {}",
            done, failed, skipped, duration
        );
    }

    fn ffmpeg_stderr(&self, line: &str) {
        if self.live_active() {
            return;
        }
        // Only show raw ffmpeg stderr in verbose mode
        if self.is_verbose() {
            self.eprintln(&format!("  [ffmpeg] {}", line));
        }
    }

    fn batch_command(&self, cmd: &str) {
        if self.live_active() {
            return;
        }
        if self.is_verbose() {
            self.eprintln(&format!("  CMD: {}", cmd));
        }
    }

    fn ffmpeg_download_progress(&self, _message: &str) {
        // CLI users install ffmpeg themselves; download is not supported
    }

    fn toast(&self, _message: &str) {
        // No toast notifications in CLI mode
    }

    fn post_batch(&self, action: &str, _countdown: u32) {
        if action != "None" && !action.is_empty() {
            self.eprintln(&format!("Running post-batch command: {}", action));
        }
    }

    // ── Wave-based staging events (Phase 3) ───────────────────

    fn wave_progress(&self, wave: u32, total_waves: u32, file_in_wave: u32, wave_size: u32) {
        if self.is_quiet() {
            return;
        }
        if self.live_active() {
            self.draw_live();
            return;
        }
        if self.is_tty {
            // Update status in-line
            let pb = self.progress_bar.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref bar) = *pb {
                bar.set_message(format!(
                    "Wave {}/{} [{}/{}]",
                    wave, total_waves, file_in_wave, wave_size
                ));
            }
        } else {
            let _ = writeln!(
                std::io::stderr(),
                "  Wave {}/{} [{}/{}]",
                wave,
                total_waves,
                file_in_wave,
                wave_size
            );
        }
    }

    fn wave_status(&self, message: &str) {
        if self.is_quiet() {
            return;
        }
        if self.live_active() {
            return;
        }
        self.clear_progress();
        self.eprintln(message);
    }

    fn batch_time_estimate(&self, elapsed_secs: f64, remaining_secs: f64) {
        // Live display: update state, next draw picks it up
        if self.live_active() {
            let mut ld = self.live_display.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(ref mut display) = *ld {
                display.batch_elapsed = elapsed_secs;
                display.batch_remaining = remaining_secs;
            }
            return;
        }

        if self.is_quiet() || !self.is_tty {
            return;
        }
        let elapsed = format_duration(elapsed_secs);
        let remaining = if remaining_secs > 0.0 {
            format_duration(remaining_secs)
        } else {
            "calculating...".to_string()
        };
        self.eprintln(&format!(
            "  Batch: {} elapsed, ~{} remaining",
            elapsed, remaining
        ));
    }

    fn wave_time_estimate(&self, elapsed_secs: f64, remaining_secs: f64) {
        if self.live_active() {
            return;
        }
        if self.is_quiet() || !self.is_tty {
            return;
        }
        let elapsed = format_duration(elapsed_secs);
        let remaining = if remaining_secs > 0.0 {
            format_duration(remaining_secs)
        } else {
            "calculating...".to_string()
        };
        self.eprintln(&format!(
            "  Wave: {} elapsed, ~{} remaining",
            elapsed, remaining
        ));
    }
}

/// Format seconds as MM:SS or HH:MM:SS.
fn format_duration(secs: f64) -> String {
    let total = secs as u64;
    let h = total / 3600;
    let m = (total % 3600) / 60;
    let s = total % 60;
    if h > 0 {
        format!("{:02}:{:02}:{:02}", h, m, s)
    } else {
        format!("{:02}:{:02}", m, s)
    }
}
