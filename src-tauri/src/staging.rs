//! Remote file staging for the CLI encoding loop.
//!
//! When a file resides on a remote mount, the `StagingContext` copies it
//! to local storage before encoding. ffmpeg reads from the local copy
//! but writes output directly to the final destination. The local copy
//! is cleaned up after encoding completes (success or failure).

use std::path::{Path, PathBuf};

use crate::events::EventSink;
use crate::queue::QueueItem;
use crate::remote::MountCache;

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
        let base = std::env::var("TMPDIR").unwrap_or_else(|_| "/tmp".to_string());
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
    ///
    /// Uses `tokio::fs::copy` so the Tokio runtime worker thread is not
    /// blocked during large file copies on slow storage.
    pub async fn stage_file(
        source_path: &Path,
        staging_dir: &Path,
        queue_index: usize,
        sink: &dyn EventSink,
    ) -> Option<Self> {
        // Ensure staging directory exists
        if let Err(e) = tokio::fs::create_dir_all(staging_dir).await {
            sink.log(&format!(
                "  WARNING: Could not create staging directory '{}': {e} - encoding in-place",
                staging_dir.display()
            ));
            return None;
        }

        // Check free space: need at least 1.1x source size
        let source_size = std::fs::metadata(source_path).map(|m| m.len()).unwrap_or(0);

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

        // Copy the file using async I/O to avoid blocking the runtime
        let size_str = crate::disk_monitor::format_bytes(source_size);
        sink.log(&format!(
            "  Staging input to {} ({})...",
            staged_path.display(),
            size_str
        ));

        let start = std::time::Instant::now();
        match tokio::fs::copy(source_path, &staged_path).await {
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
                let _ = tokio::fs::remove_file(&staged_path).await;
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

// ── Wave-based staging (Phase 3) ─────────────────────────────────

/// A single item in a wave plan: either a local file or a wave of remote files.
#[derive(Debug)]
pub enum WaveItem {
    /// Encode this local file directly (no staging needed).
    Local { queue_index: usize },
    /// A wave of remote files to stage together.
    Wave {
        indices: Vec<usize>,
        total_stage_bytes: u64,
    },
}

/// Plans how to group remote files into staging waves.
///
/// The planner walks the queue in order. Consecutive remote files accumulate
/// into a wave until adding the next file would exceed `available_space * 0.9`.
/// Local files between remote files flush any pending wave and insert
/// themselves as `WaveItem::Local`.
pub struct WavePlanner;

impl WavePlanner {
    /// Build a wave plan from the pending queue items.
    ///
    /// - `queue`: the full queue (only items at `pending_indices` are considered)
    /// - `pending_indices`: indices of pending items in the queue
    /// - `mount_cache`: for remote detection (mutated for caching)
    /// - `staging_dir`: path to the staging directory (for free-space query)
    /// - `force_local`: if true, treat all files as local (skip staging)
    /// - `remote_never`: if true, never stage (--remote never)
    pub fn plan(
        queue: &[QueueItem],
        pending_indices: &[usize],
        mount_cache: &mut MountCache,
        staging_dir: &Path,
        force_local: bool,
        remote_never: bool,
    ) -> Vec<WaveItem> {
        // If staging is disabled, return all files as local
        if force_local || remote_never {
            return pending_indices
                .iter()
                .map(|&idx| WaveItem::Local { queue_index: idx })
                .collect();
        }

        // Query free space on the staging partition; use 90% as budget.
        // This is a snapshot at plan time. Since waves are staged and cleaned
        // up sequentially at runtime, the effective budget per-wave is at least
        // this much (cleanup frees space for the next wave). Over-constraining
        // is safe; under-constraining would risk running out of disk space.
        let wave_budget: u64 = crate::disk_monitor::partition_free_space(staging_dir)
            .map(|(_, free)| (free as f64 * 0.9) as u64)
            .unwrap_or(u64::MAX);

        let mut plan: Vec<WaveItem> = Vec::with_capacity(pending_indices.len());
        let mut wave_indices: Vec<usize> = Vec::with_capacity(16);
        let mut wave_bytes: u64 = 0;

        for &idx in pending_indices {
            let item = &queue[idx];
            let is_remote = mount_cache.is_remote(Path::new(&item.full_path));

            if !is_remote {
                // Local file: flush any pending wave, then add as Local
                if !wave_indices.is_empty() {
                    plan.push(WaveItem::Wave {
                        indices: std::mem::take(&mut wave_indices),
                        total_stage_bytes: wave_bytes,
                    });
                    wave_bytes = 0;
                }
                plan.push(WaveItem::Local { queue_index: idx });
                continue;
            }

            // Remote file: try to add to the current wave
            let file_bytes = item.source_bytes;

            // If this single file exceeds the budget, give it its own wave
            if file_bytes >= wave_budget {
                // Flush any pending wave first
                if !wave_indices.is_empty() {
                    plan.push(WaveItem::Wave {
                        indices: std::mem::take(&mut wave_indices),
                        total_stage_bytes: wave_bytes,
                    });
                    wave_bytes = 0;
                }
                plan.push(WaveItem::Wave {
                    indices: vec![idx],
                    total_stage_bytes: file_bytes,
                });
                continue;
            }

            // Would adding this file exceed the budget?
            if wave_bytes + file_bytes > wave_budget && !wave_indices.is_empty() {
                // Flush the current wave
                plan.push(WaveItem::Wave {
                    indices: std::mem::take(&mut wave_indices),
                    total_stage_bytes: wave_bytes,
                });
                wave_bytes = 0;
            }

            wave_indices.push(idx);
            wave_bytes += file_bytes;
        }

        // Flush any remaining wave
        if !wave_indices.is_empty() {
            plan.push(WaveItem::Wave {
                indices: wave_indices,
                total_stage_bytes: wave_bytes,
            });
        }

        plan
    }
}
