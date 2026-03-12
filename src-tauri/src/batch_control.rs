//! CLI implementation of `BatchControl` — signal-handler-safe state with
//! TTY prompts for interactive decisions.

use std::io::Write;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use histv_lib::events::BatchControl;

use crate::cli::{FallbackPolicy, OverwritePolicy};

/// CLI batch control state, using atomics for signal-handler safety.
pub struct CliBatchControl {
    cancel_current: AtomicBool,
    cancel_all: AtomicBool,
    overwrite_all: AtomicBool,
    fallback_offered: AtomicBool,
    is_tty: bool,
    overwrite_policy: OverwritePolicy,
    fallback_policy: FallbackPolicy,
}

impl CliBatchControl {
    pub fn new(overwrite_policy: OverwritePolicy, fallback_policy: FallbackPolicy) -> Arc<Self> {
        let is_tty = std::io::IsTerminal::is_terminal(&std::io::stderr());
        let state = Arc::new(Self {
            cancel_current: AtomicBool::new(false),
            cancel_all: AtomicBool::new(false),
            overwrite_all: AtomicBool::new(false),
            fallback_offered: AtomicBool::new(false),
            is_tty,
            overwrite_policy,
            fallback_policy,
        });

        // Register Ctrl+C handler
        let handler_state = Arc::clone(&state);
        let last_sigint = Arc::new(AtomicU64::new(0));
        ctrlc_register(handler_state, last_sigint);

        state
    }
}

/// Register Ctrl+C / SIGINT handler.
///
/// First press: sets `cancel_current`.
/// Second press within 2 seconds: sets `cancel_all`.
fn ctrlc_register(state: Arc<CliBatchControl>, last_sigint: Arc<AtomicU64>) {
    let _ = ctrlc::set_handler(move || {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let prev = last_sigint.swap(now_ms, Ordering::SeqCst);

        if now_ms.saturating_sub(prev) < 2000 {
            // Double Ctrl+C within 2 seconds — cancel entire batch
            state.cancel_all.store(true, Ordering::SeqCst);
            eprintln!("\nCancelling batch...");
        } else if state.cancel_current.load(Ordering::SeqCst) {
            // Already cancelling current file, escalate to cancel all
            state.cancel_all.store(true, Ordering::SeqCst);
            eprintln!("\nCancelling batch...");
        } else {
            // First press — cancel current file
            state.cancel_current.store(true, Ordering::SeqCst);
            eprintln!("\nCancelling current file (press Ctrl+C again to cancel batch)...");
        }
    });
}

impl BatchControl for CliBatchControl {
    fn should_cancel_current(&self) -> bool {
        self.cancel_current.load(Ordering::SeqCst)
    }

    fn should_cancel_all(&self) -> bool {
        self.cancel_all.load(Ordering::SeqCst)
    }

    fn is_paused(&self) -> bool {
        // Pause not supported in CLI v1
        false
    }

    fn clear_cancel_current(&self) {
        self.cancel_current.store(false, Ordering::SeqCst);
    }

    fn overwrite_always(&self) -> bool {
        self.overwrite_all.load(Ordering::SeqCst)
    }

    fn set_overwrite_always(&self) {
        self.overwrite_all.store(true, Ordering::SeqCst);
    }

    fn overwrite_prompt(&self, path: &str) -> String {
        // Check policy first
        match self.overwrite_policy {
            OverwritePolicy::Yes => return "yes".to_string(),
            OverwritePolicy::Skip => return "no".to_string(),
            OverwritePolicy::Ask => {}
        }

        // Not a TTY — safe default is skip
        if !self.is_tty {
            return "no".to_string();
        }

        // Interactive TTY prompt
        eprintln!("  Output file already exists: {}", path);
        eprint!("  [o]verwrite / [s]kip / [a]lways / [c]ancel batch: ");
        let _ = std::io::stderr().flush();

        let response = read_tty_line();
        match response.trim().to_lowercase().as_str() {
            "o" | "overwrite" | "yes" | "y" => "yes".to_string(),
            "a" | "always" => "always".to_string(),
            "c" | "cancel" => "cancel".to_string(),
            _ => "no".to_string(), // skip is the default
        }
    }

    fn hw_fallback_offered(&self) -> bool {
        self.fallback_offered.load(Ordering::SeqCst)
    }

    fn set_hw_fallback_offered(&self) {
        self.fallback_offered.store(true, Ordering::SeqCst);
    }

    fn fallback_prompt(&self, filename: &str) -> String {
        // Check policy first
        match self.fallback_policy {
            FallbackPolicy::Yes => return "yes".to_string(),
            FallbackPolicy::No => return "no".to_string(),
            FallbackPolicy::Ask => {}
        }

        // Not a TTY — default to yes (auto-fallback)
        if !self.is_tty {
            return "yes".to_string();
        }

        // Interactive TTY prompt
        eprintln!("  Hardware encoder failed for: {}", filename);
        eprint!("  Retry with software encoder? [y/n]: ");
        let _ = std::io::stderr().flush();

        let response = read_tty_line();
        match response.trim().to_lowercase().as_str() {
            "y" | "yes" => "yes".to_string(),
            _ => "no".to_string(),
        }
    }
}

/// Read a line from the TTY (not stdin, which may be piped).
/// Falls back to stdin if /dev/tty is not available (Windows).
fn read_tty_line() -> String {
    // On Unix, read from /dev/tty to bypass stdin redirection
    #[cfg(unix)]
    {
        if let Ok(mut tty) = std::fs::File::open("/dev/tty") {
            use std::io::BufRead;
            let mut reader = std::io::BufReader::new(&mut tty);
            let mut line = String::new();
            if reader.read_line(&mut line).is_ok() {
                return line;
            }
        }
    }

    // Fallback: read from stdin
    let mut line = String::new();
    let _ = std::io::stdin().read_line(&mut line);
    line
}
