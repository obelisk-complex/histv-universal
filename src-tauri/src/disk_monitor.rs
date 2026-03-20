//! Disk-space estimation and monitoring for batch encoding.
//!
//! Provides pre-batch disk-space estimates and runtime partition monitoring.
//! The pre-batch estimate uses probed file metadata and encoding decisions to
//! predict peak disk usage without any additional I/O.

use std::path::{Path, PathBuf};

use crate::encoder::EncodeDecision;
use crate::queue::QueueItem;

/// Per-file disk-space estimate.
#[derive(Debug, Clone)]
pub struct FileEstimate {
    pub source_bytes: u64,
    pub estimated_output_bytes: u64,
    /// Peak transient bytes: source + output coexisting before source deletion.
    pub peak_transient_bytes: u64,
    /// Net bytes added if --delete-source is active (output - source, clamped to 0).
    pub net_with_delete: i64,
}

/// Batch-level disk-space estimate.
#[derive(Debug, Clone)]
pub struct BatchEstimate {
    /// Total estimated output bytes across all files.
    pub total_output_bytes: u64,
    /// Peak additional bytes needed on the output partition (without --delete-source).
    pub peak_additional_bytes: u64,
    /// Peak additional bytes with --delete-source active.
    pub peak_additional_bytes_with_delete: u64,
    /// Net change in disk usage after the entire batch (with --delete-source).
    pub net_change_with_delete: i64,
    /// Net change without --delete-source.
    pub net_change_without_delete: i64,
}

/// Estimate disk-space impact for a single file given its encoding decision.
pub fn estimate_file(item: &QueueItem, decision: &EncodeDecision) -> FileEstimate {
    let source_bytes = std::fs::metadata(&item.full_path)
        .map(|m| m.len())
        .unwrap_or(0);

    let estimated_output_bytes = match decision {
        EncodeDecision::Copy => {
            // Remux: output ~= source size
            source_bytes
        }
        EncodeDecision::Vbr { target_bps, .. } => {
            // VBR: estimate from target bitrate * duration
            if item.duration_secs > 0.0 {
                ((*target_bps as f64 * item.duration_secs) / 8.0) as u64
            } else {
                source_bytes // Can't estimate without duration
            }
        }
        EncodeDecision::Cqp { .. } | EncodeDecision::Crf { .. } => {
            // Quality-based: output is unpredictable. Use source size as
            // worst case (the post-encode size check remuxes if output
            // exceeds source, so output <= source in the final result).
            source_bytes
        }
    };

    let peak_transient_bytes = source_bytes + estimated_output_bytes;
    let net_with_delete = estimated_output_bytes as i64 - source_bytes as i64;

    FileEstimate {
        source_bytes,
        estimated_output_bytes,
        peak_transient_bytes,
        net_with_delete,
    }
}

/// Estimate total disk-space impact for a batch of files.
pub fn estimate_batch(
    items: &[&QueueItem],
    decisions: &[EncodeDecision],
) -> BatchEstimate {
    let mut total_output_bytes: u64 = 0;
    let mut peak_additional: u64 = 0;
    let mut peak_additional_delete: u64 = 0;
    let mut net_delete: i64 = 0;
    let mut net_no_delete: i64 = 0;

    for (item, decision) in items.iter().zip(decisions.iter()) {
        let est = estimate_file(item, decision);

        total_output_bytes += est.estimated_output_bytes;

        // Without --delete-source, every output file is additional space
        peak_additional += est.estimated_output_bytes;
        net_no_delete += est.estimated_output_bytes as i64;

        // With --delete-source, the peak transient is one file's worth
        // (source + output coexist briefly), but after deletion only the
        // output remains. The worst case is the single largest transient.
        if est.peak_transient_bytes > peak_additional_delete {
            peak_additional_delete = est.peak_transient_bytes;
        }
        net_delete += est.net_with_delete;
    }

    BatchEstimate {
        total_output_bytes,
        peak_additional_bytes: peak_additional,
        peak_additional_bytes_with_delete: peak_additional_delete,
        net_change_with_delete: net_delete,
        net_change_without_delete: net_no_delete,
    }
}

/// Query free space on the partition containing the given path.
/// Returns (total_bytes, free_bytes) or None if the query fails.
pub fn partition_free_space(path: &Path) -> Option<(u64, u64)> {
    // Ensure the path exists (or use its parent) for the query
    let query_path = if path.exists() {
        path.to_path_buf()
    } else if let Some(parent) = path.parent() {
        if parent.exists() {
            parent.to_path_buf()
        } else {
            return None;
        }
    } else {
        return None;
    };

    #[cfg(unix)]
    {
        partition_free_space_unix(&query_path)
    }

    #[cfg(windows)]
    {
        partition_free_space_windows(&query_path)
    }

    #[cfg(not(any(unix, windows)))]
    {
        let _ = query_path;
        None
    }
}

#[cfg(unix)]
fn partition_free_space_unix(path: &Path) -> Option<(u64, u64)> {
    // Use `df` which is available on all Unix systems and avoids
    // platform-specific statvfs struct layout differences.
    // `df -P` uses POSIX output format with 512-byte blocks.
    // `df -k` uses 1024-byte blocks and is more widely consistent.
    let output = std::process::Command::new("df")
        .args(["-k", &path.to_string_lossy()])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    // Second line contains the data: Filesystem 1K-blocks Used Available Use% Mounted_on
    let data_line = stdout.lines().nth(1)?;
    let fields: Vec<&str> = data_line.split_whitespace().collect();
    if fields.len() < 4 {
        return None;
    }

    // fields[1] = total 1K-blocks, fields[3] = available 1K-blocks
    let total_kb: u64 = fields[1].parse().ok()?;
    let avail_kb: u64 = fields[3].parse().ok()?;

    Some((total_kb * 1024, avail_kb * 1024))
}

#[cfg(windows)]
fn partition_free_space_windows(path: &Path) -> Option<(u64, u64)> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let wide_path: Vec<u16> = OsStr::new(path)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();

    let mut free_bytes_available: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_free_bytes: u64 = 0;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            lpDirectoryName: *const u16,
            lpFreeBytesAvailableToCaller: *mut u64,
            lpTotalNumberOfBytes: *mut u64,
            lpTotalNumberOfFreeBytes: *mut u64,
        ) -> i32;
    }

    // Safety: GetDiskFreeSpaceExW is a well-defined Windows API call.
    let success = unsafe {
        GetDiskFreeSpaceExW(
            wide_path.as_ptr(),
            &mut free_bytes_available,
            &mut total_bytes,
            &mut total_free_bytes,
        )
    };

    if success != 0 {
        Some((total_bytes, free_bytes_available))
    } else {
        None
    }
}

/// Format a byte count as a human-readable string (e.g. "23.4GB").
pub fn format_bytes(bytes: u64) -> String {
    const GB: f64 = 1_000_000_000.0;
    const MB: f64 = 1_000_000.0;
    let b = bytes as f64;
    if b >= GB {
        format!("{:.1}GB", b / GB)
    } else {
        format!("{:.1}MB", b / MB)
    }
}

/// Format a signed byte count (for net change estimates).
pub fn format_bytes_signed(bytes: i64) -> String {
    let prefix = if bytes >= 0 { "+" } else { "" };
    let abs = bytes.unsigned_abs();
    format!("{}{}", prefix, format_bytes(abs))
}

// ── Runtime disk monitoring ────────────────────────────────────

/// Runtime disk monitor for the encoding loop. Checks partition usage
/// between files and pauses when the limit is exceeded.
pub struct DiskMonitor {
    output_path: PathBuf,
    staging_path: Option<PathBuf>,
    limit_pct: u8,
    resume_bytes: u64,
    baseline_free: u64,
}

impl DiskMonitor {
    /// Create a new disk monitor. Records the current free space as the
    /// baseline for the resume threshold.
    ///
    /// Returns `None` if disk-aware mode is off (disk_limit is "off").
    pub fn new(
        disk_limit: &str,
        disk_resume: Option<u8>,
        output_path: &Path,
        staging_path: Option<&Path>,
    ) -> Option<Self> {
        if disk_limit == "off" || disk_limit.is_empty() {
            return None;
        }

        let limit_pct: u8 = match disk_limit.parse() {
            Ok(v) if (50..=99).contains(&v) => v,
            _ => {
                eprintln!("WARNING: Invalid --disk-limit '{}', ignoring", disk_limit);
                return None;
            }
        };

        let (total, free) = partition_free_space(output_path)?;

        let resume_bytes = if let Some(pct) = disk_resume {
            // Resume when free space reaches this percentage of total
            (total as f64 * (pct as f64 / 100.0)) as u64
        } else {
            // Default: resume when free space returns to baseline
            free
        };

        Some(Self {
            output_path: output_path.to_path_buf(),
            staging_path: staging_path.map(|p| p.to_path_buf()),
            limit_pct,
            resume_bytes,
            baseline_free: free,
        })
    }

    /// Check disk usage and wait if over the limit (#18 - now async).
    /// Returns `true` if encoding should continue, `false` if cancelled
    /// during wait.
    pub async fn check_and_wait(
        &self,
        sink: &dyn crate::events::EventSink,
        batch_control: &dyn crate::events::BatchControl,
    ) -> bool {
        // Check output partition
        if self.is_over_limit(&self.output_path, sink) {
            if !self.wait_for_space(&self.output_path, sink, batch_control).await {
                return false;
            }
        }

        // Check staging partition if different
        if let Some(ref staging) = self.staging_path {
            if self.is_over_limit(staging, sink) {
                if !self.wait_for_space(staging, sink, batch_control).await {
                    return false;
                }
            }
        }

        true
    }

    fn is_over_limit(&self, path: &Path, sink: &dyn crate::events::EventSink) -> bool {
        let (total, free) = match partition_free_space(path) {
            Some(v) => v,
            None => return false,
        };

        if total == 0 {
            return false;
        }

        let used_pct = ((total - free) as f64 / total as f64 * 100.0) as u8;
        if used_pct >= self.limit_pct {
            sink.log(&format!(
                "Disk usage at {}% ({} free). Pausing until free space recovers to {}...",
                used_pct,
                format_bytes(free),
                format_bytes(self.resume_bytes),
            ));
            true
        } else {
            false
        }
    }

    /// Wait for disk space to recover (#18).
    /// Uses `tokio::time::sleep` instead of `std::thread::sleep` to avoid
    /// blocking the async runtime's executor thread.
    async fn wait_for_space(
        &self,
        path: &Path,
        sink: &dyn crate::events::EventSink,
        batch_control: &dyn crate::events::BatchControl,
    ) -> bool {
        loop {
            if batch_control.should_cancel_all() {
                return false;
            }

            tokio::time::sleep(std::time::Duration::from_secs(30)).await;

            if batch_control.should_cancel_all() {
                return false;
            }

            if let Some((_, free)) = partition_free_space(path) {
                if free >= self.resume_bytes {
                    sink.log(&format!(
                        "Disk space recovered ({} free). Resuming...",
                        format_bytes(free),
                    ));
                    return true;
                }
            }
        }
    }

    /// The baseline free space recorded at monitor creation.
    pub fn baseline_free(&self) -> u64 {
        self.baseline_free
    }
}