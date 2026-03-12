//! Remote file staging for the CLI encoding loop.
//!
//! When a file resides on a remote mount, the `StagingContext` copies it
//! to local storage before encoding. ffmpeg reads from the local copy
//! but writes output directly to the final destination. The local copy
//! is cleaned up after encoding completes (success or failure).

use std::path::{Path, PathBuf};

use crate::events::EventSink;

/// Resolves the staging directory from flags, env vars, or platform default.
pub fn resolve_staging_dir(local_tmp: Option<&Path>) -> PathBuf {
    if let Some(dir) = local_tmp {
        return dir.to_path_buf();
    }

    if let Ok(dir) = std::env::var("HISTV_TMP") {
        if !dir.is_empty() {
            return PathBuf::from(dir);
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        let base = std::env::var("TMPDIR")
            .unwrap_or_else(|_| "/tmp".to_string());
        PathBuf::from(base).join("histv-staging")
    }

    #[cfg(target_os = "windows")]
    {
        let base = std::env::var("TEMP")
            .or_else(|_| std::env::var("TMP"))
            .unwrap_or_else(|_| ".".to_string());
        PathBuf::from(base).join("histv-staging")
    }
}

/// Manages staging a single file: copy-in, path rewriting, and cleanup.
///
/// Create one per file that needs staging. The staged copy is automatically
/// cleaned up when the context is dropped (even on panic/SIGINT).
pub struct StagingContext {
    staged_path: PathBuf,
    created: bool,
}

impl StagingContext {
    /// Stage a file by copying it to the staging directory.
    ///
    /// Returns the `StagingContext` (for cleanup) and the local path to
    /// use as ffmpeg's input. Returns `None` if staging fails (caller
    /// should fall back to in-place encoding).
    pub fn stage_file(
        source_path: &Path,
        staging_dir: &Path,
        queue_index: usize,
        sink: &dyn EventSink,
    ) -> Option<Self> {
        // Ensure staging directory exists
        if let Err(e) = std::fs::create_dir_all(staging_dir) {
            sink.log(&format!(
                "  WARNING: Could not create staging directory '{}': {e} - encoding in-place",
                staging_dir.display()
            ));
            return None;
        }

        // Check free space: need at least 1.1x source size
        let source_size = std::fs::metadata(source_path)
            .map(|m| m.len())
            .unwrap_or(0);

        if source_size > 0 {
            if let Some((_, free)) = crate::disk_monitor::partition_free_space(staging_dir) {
                let needed = (source_size as f64 * 1.1) as u64;
                if free < needed {
                    sink.log(&format!(
                        "  WARNING: Insufficient space in staging directory ({} free, {} needed) - encoding in-place",
                        crate::disk_monitor::format_bytes(free),
                        crate::disk_monitor::format_bytes(needed),
                    ));
                    return None;
                }
            }
        }

        // Build the staged filename: {index}_{original_filename}
        let file_name = source_path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy();
        let staged_name = format!("{}_{}", queue_index, file_name);
        let staged_path = staging_dir.join(staged_name);

        // Copy the file
        let size_str = crate::disk_monitor::format_bytes(source_size);
        sink.log(&format!(
            "  Staging input to {} ({})...",
            staged_path.display(), size_str
        ));

        let start = std::time::Instant::now();
        match std::fs::copy(source_path, &staged_path) {
            Ok(_) => {
                let elapsed = start.elapsed().as_secs();
                sink.log(&format!("  Staged in {}s", elapsed));
                Some(Self {
                    staged_path,
                    created: true,
                })
            }
            Err(e) => {
                sink.log(&format!(
                    "  WARNING: Staging failed: {e} - encoding in-place"
                ));
                // Clean up partial copy
                let _ = std::fs::remove_file(&staged_path);
                None
            }
        }
    }

    /// Return the path to the staged local copy (for use as ffmpeg input).
    pub fn local_path(&self) -> &Path {
        &self.staged_path
    }

    /// Explicitly clean up the staged file and log the action.
    pub fn cleanup(&mut self, sink: &dyn EventSink) {
        if self.created {
            let _ = std::fs::remove_file(&self.staged_path);
            self.created = false;
            sink.log("  Cleaned up staged input");
        }
    }
}

/// Drop guard: ensure the staged file is removed even if we panic
/// or exit early (e.g. SIGINT during encoding).
impl Drop for StagingContext {
    fn drop(&mut self) {
        if self.created {
            let _ = std::fs::remove_file(&self.staged_path);
            self.created = false;
        }
    }
}
