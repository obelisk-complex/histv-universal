use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;

/// Supported video file extensions (§5.2).
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "ts", "m2ts", "wmv", "mov", "webm", "m4v",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub enum QueueItemStatus {
    Pending,
    Probing,
    Encoding,
    Done,
    Failed,
    Skipped,
    Cancelled,
}

impl std::fmt::Display for QueueItemStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "Pending"),
            Self::Probing => write!(f, "Probing"),
            Self::Encoding => write!(f, "Encoding"),
            Self::Done => write!(f, "Done"),
            Self::Failed => write!(f, "Failed"),
            Self::Skipped => write!(f, "Skipped"),
            Self::Cancelled => write!(f, "Cancelled"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct QueueItem {
    pub full_path: String,
    pub file_name: String,
    pub base_name: String,
    pub status: QueueItemStatus,
    pub video_codec: String,
    pub video_width: u32,
    pub video_height: u32,
    pub video_bitrate_bps: f64,
    pub video_bitrate_mbps: f64,
    pub is_hdr: bool,
    pub color_transfer: String,
    pub audio_streams: Vec<AudioStreamInfo>,
    pub duration_secs: f64,
}

/// Per-stream audio metadata collected during probing.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioStreamInfo {
    pub index: u32,
    pub codec: String,
    pub bitrate_kbps: u32,
}

/// Batch control state, shared between the UI thread and the encoding task.
#[derive(Debug, Default)]
pub struct BatchState {
    pub running: bool,
    pub cancel_current: bool,
    pub cancel_all: bool,
    pub paused: bool,
    pub overwrite_always: bool,
    pub hw_fallback_offered: bool,
    pub overwrite_response: Option<String>,
    pub fallback_response: Option<String>,
}

/// Result from add_paths_to_queue — includes the starting index so the
/// frontend can probe by index range without O(N*M) path lookups (#2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddResult {
    pub start_index: usize,
    pub count: usize,
}

fn is_supported_extension(path: &str) -> bool {
    let p = Path::new(path);
    if let Some(ext) = p.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        SUPPORTED_EXTENSIONS.contains(&ext_lower.as_str())
    } else {
        false
    }
}

/// Iteratively collect supported files from a list of paths (files and folders).
/// Uses a VecDeque work queue instead of recursive allocation (#13).
fn collect_files(paths: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut work: VecDeque<std::path::PathBuf> = paths.iter().map(std::path::PathBuf::from).collect();

    while let Some(path) = work.pop_front() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    // Use entry.file_type() which avoids an extra stat() on
                    // most platforms — critical on network mounts where each
                    // stat is a round-trip.
                    match entry.file_type() {
                        Ok(ft) if ft.is_dir() => work.push_back(entry.path()),
                        Ok(ft) if ft.is_file() => {
                            let p = entry.path();
                            let path_str = p.to_string_lossy().to_string();
                            if is_supported_extension(&path_str) {
                                result.push(path_str);
                            }
                        }
                        _ => {}
                    }
                }
            }
        } else if path.is_file() {
            let path_str = path.to_string_lossy().to_string();
            if is_supported_extension(&path_str) {
                result.push(path_str);
            }
        }
    }
    result
}

/// Add files/folders to the queue, filtering by extension and deduplicating.
/// Returns an AddResult with the starting index and count of added items,
/// so the frontend can probe by index range without path lookups (#1, #2).
pub fn add_paths_to_queue(queue: &mut Vec<QueueItem>, paths: &[String]) -> AddResult {
    let files = collect_files(paths);

    // Build a HashSet of existing paths for O(1) dedup lookups (#1)
    let existing: HashSet<String> = queue.iter().map(|item| item.full_path.clone()).collect();
    let start_index = queue.len();

    for file_path in files {
        if existing.contains(&file_path) {
            continue;
        }

        let p = Path::new(&file_path);
        let file_name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let base_name = p
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        let item = QueueItem {
            full_path: file_path,
            file_name,
            base_name,
            status: QueueItemStatus::Pending,
            video_codec: String::new(),
            video_width: 0,
            video_height: 0,
            video_bitrate_bps: 0.0,
            video_bitrate_mbps: 0.0,
            is_hdr: false,
            color_transfer: String::new(),
            audio_streams: Vec::new(),
            duration_secs: 0.0,
        };
        queue.push(item);
    }

    AddResult {
        start_index,
        count: queue.len() - start_index,
    }
}

/// Remove queue items by indices (sorted descending internally).
pub fn remove_items(queue: &mut Vec<QueueItem>, indices: &[usize]) {
    let mut sorted: Vec<usize> = indices.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    for idx in sorted.into_iter().rev() {
        if idx < queue.len() {
            queue.remove(idx);
        }
    }
}

/// Reset selected items back to Pending (re-queue).
/// Only resets items in a terminal state (Done, Failed, Skipped, Cancelled).
pub fn requeue_items(queue: &mut Vec<QueueItem>, indices: &[usize]) {
    for &idx in indices {
        if idx < queue.len() {
            match queue[idx].status {
                QueueItemStatus::Done
                | QueueItemStatus::Failed
                | QueueItemStatus::Skipped
                | QueueItemStatus::Cancelled => {
                    queue[idx].status = QueueItemStatus::Pending;
                }
                _ => {}
            }
        }
    }
}

/// Reset all items in a terminal state back to Pending.
pub fn requeue_all(queue: &mut Vec<QueueItem>) {
    for item in queue.iter_mut() {
        match item.status {
            QueueItemStatus::Done
            | QueueItemStatus::Failed
            | QueueItemStatus::Skipped
            | QueueItemStatus::Cancelled => {
                item.status = QueueItemStatus::Pending;
            }
            _ => {}
        }
    }
}

/// Clear all non-pending items (Done, Failed, Skipped, Cancelled).
pub fn clear_non_pending(queue: &mut Vec<QueueItem>) {
    queue.retain(|item| item.status == QueueItemStatus::Pending
        || item.status == QueueItemStatus::Probing
        || item.status == QueueItemStatus::Encoding);
}

/// Move a queue item from one index to another.
pub fn move_item(queue: &mut Vec<QueueItem>, from: usize, to: usize) {
    if from >= queue.len() || to >= queue.len() || from == to {
        return;
    }
    let item = queue.remove(from);
    queue.insert(to, item);
}