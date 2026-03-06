use serde::{Deserialize, Serialize};
use std::path::Path;

/// Supported video file extensions (§5.2).
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "ts", "m2ts", "wmv", "mov", "webm",
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

fn is_supported_extension(path: &str) -> bool {
    let p = Path::new(path);
    if let Some(ext) = p.extension() {
        let ext_lower = ext.to_string_lossy().to_lowercase();
        SUPPORTED_EXTENSIONS.contains(&ext_lower.as_str())
    } else {
        false
    }
}

/// Recursively collect supported files from a list of paths (files and folders).
fn collect_files(paths: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    for path_str in paths {
        let path = Path::new(path_str);
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(path) {
                let child_paths: Vec<String> = entries
                    .flatten()
                    .map(|e| e.path().to_string_lossy().to_string())
                    .collect();
                result.extend(collect_files(&child_paths));
            }
        } else if path.is_file() && is_supported_extension(path_str) {
            result.push(path_str.clone());
        }
    }
    result
}

/// Add files/folders to the queue, filtering by extension and deduplicating.
pub fn add_paths_to_queue(queue: &mut Vec<QueueItem>, paths: &[String]) -> Vec<QueueItem> {
    let files = collect_files(paths);
    let mut added = Vec::new();

    for file_path in files {
        // Duplicate check by full path
        if queue.iter().any(|item| item.full_path == file_path) {
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
        };
        queue.push(item.clone());
        added.push(item);
    }

    added
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