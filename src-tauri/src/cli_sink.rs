//! CLI implementation of `EventSink` - writes to stderr with optional
//! indicatif progress bars when a TTY is attached.
//!
//! All output goes to stderr so stdout remains clean for piped usage.
//! Log level filtering controls verbosity: quiet (errors only),
//! normal (key events), verbose (everything including ffmpeg stderr).

use std::io::Write;
use std::sync::Mutex;

use indicatif::{ProgressBar, ProgressStyle};

use histv_lib::events::EventSink;

use crate::cli::LogLevel;

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

    /// Write a line to stderr, suspending any active progress bar first
    /// so the output doesn't collide with the bar rendering.
    fn eprintln(&self, msg: &str) {
        let pb = self.progress_bar.lock().unwrap();
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
        let mut pb = self.progress_bar.lock().unwrap();
        if let Some(bar) = pb.take() {
            bar.finish_and_clear();
        }
    }

    /// Get the cached pass label, updating it only if the pass value changed (#17).
    fn pass_label(&self, pass: Option<(u8, u8)>) -> String {
        let mut cached = self.cached_pass_label.lock().unwrap();
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

        let pass_label = self.pass_label(pass);

        if self.is_tty {
            // Rich mode: update indicatif progress bar
            let mut pb = self.progress_bar.lock().unwrap();
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
            if pct % 10 == 0 {
                let mut last = self.last_simple_pct.lock().unwrap();
                if *last != pct {
                    *last = pct;
                    let _ = writeln!(std::io::stderr(), "  {}%{}", pct, pass_label);
                }
            }
        }
    }

    fn batch_progress(&self, current: u32, total: usize) {
        if !self.is_quiet() && !self.is_tty {
            // Simple mode: print batch position
            let _ = writeln!(std::io::stderr(), "[{}/{}]", current, total);
        }
        // In TTY mode, batch_status handles the display
    }

    fn batch_status(&self, message: &str) {
        if self.is_quiet() {
            return;
        }
        // Clear any existing per-file progress bar before the new file header
        self.clear_progress();
        *self.last_simple_pct.lock().unwrap() = u32::MAX; // reset for new file
        self.eprintln(message);
    }

    fn queue_item_updated(&self, _index: usize, status: &str) {
        // The CLI doesn't maintain a live queue display; status changes
        // are reflected through log messages instead. Prompt bridges
        // (overwrite-prompt:, fallback-prompt:) are not used by the CLI -
        // those will be handled by BatchControl in Phase 3.
        if self.is_verbose() {
            self.eprintln(&format!("  [status] {}", status));
        }
    }

    fn queue_item_probed(&self, _index: usize) {
        // Probing progress is shown via log messages
    }

    fn batch_started(&self) {
        if !self.is_quiet() {
            self.eprintln("");
        }
    }

    fn batch_finished(&self, done: u32, failed: u32, skipped: u32, duration: &str) {
        self.clear_progress();

        // Always show the summary, even in quiet mode
        eprintln!("");
        eprintln!(
            "Batch complete. Done: {}, Failed: {}, Skipped: {}. Duration: {}",
            done, failed, skipped, duration
        );
    }

    fn ffmpeg_stderr(&self, line: &str) {
        // Only show raw ffmpeg stderr in verbose mode
        if self.is_verbose() {
            self.eprintln(&format!("  [ffmpeg] {}", line));
        }
    }

    fn batch_command(&self, cmd: &str) {
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
        if self.is_tty {
            // Update status in-line
            let pb = self.progress_bar.lock().unwrap();
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
        self.clear_progress();
        self.eprintln(message);
    }

    fn batch_time_estimate(&self, elapsed_secs: f64, remaining_secs: f64) {
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
