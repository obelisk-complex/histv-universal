use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::collections::VecDeque;
use std::path::Path;

use crate::probe::ProbeResult;

/// Supported video file extensions (§5.2).
const SUPPORTED_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "ts", "m2ts", "mts", "wmv", "mov", "webm", "m4v", "mpg", "mpeg", "vob",
    "flv", "3gp", "3g2", "ogv", "rmvb", "rm", "asf", "f4v", "y4m", "gif", "apng", "webp",
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
    pub source_bytes: u64,
    #[serde(flatten)]
    pub probe: ProbeResult,
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

/// Result from add_paths_to_queue - includes the starting index so the
/// frontend can probe by index range without O(N*M) path lookups (#2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AddResult {
    pub start_index: usize,
    pub count: usize,
}

/// Check if a file path has a supported video extension (#11).
/// Uses `eq_ignore_ascii_case` against each entry in the constant array
/// instead of allocating a lowercase String per path.
fn is_supported_extension(path: &str) -> bool {
    let p = Path::new(path);
    if let Some(ext) = p.extension() {
        let ext_os = ext.to_string_lossy();
        SUPPORTED_EXTENSIONS
            .iter()
            .any(|&supported| ext_os.eq_ignore_ascii_case(supported))
    } else {
        false
    }
}

/// Iteratively collect supported files from a list of paths (files and folders).
/// Uses a VecDeque work queue instead of recursive allocation (#13).
fn collect_files(paths: &[String]) -> Vec<String> {
    let mut result = Vec::new();
    let mut work: VecDeque<std::path::PathBuf> =
        paths.iter().map(std::path::PathBuf::from).collect();

    while let Some(path) = work.pop_front() {
        if path.is_dir() {
            if let Ok(entries) = std::fs::read_dir(&path) {
                for entry in entries.flatten() {
                    // Use entry.file_type() which avoids an extra stat() on
                    // most platforms - critical on network mounts where each
                    // stat is a round-trip.
                    match entry.file_type() {
                        Ok(ft) if ft.is_dir() => work.push_back(entry.path()),
                        Ok(ft) if ft.is_file() => {
                            let p = entry.path();
                            // Reject non-UTF-8 paths on Unix rather than
                            // silently mangling them via to_string_lossy.
                            #[cfg(unix)]
                            if p.to_str().is_none() {
                                continue;
                            }
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
            // Reject non-UTF-8 paths on Unix rather than silently
            // mangling them via to_string_lossy.
            #[cfg(unix)]
            if path.to_str().is_none() {
                continue;
            }
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

        let source_bytes = std::fs::metadata(&file_path).map(|m| m.len()).unwrap_or(0);

        let item = QueueItem {
            full_path: file_path,
            file_name,
            base_name,
            status: QueueItemStatus::Pending,
            source_bytes,
            probe: ProbeResult::default(),
        };
        queue.push(item);
    }

    AddResult {
        start_index,
        count: queue.len() - start_index,
    }
}

/// Remove queue items by indices (#12 - sort in place via mutable slice).
pub fn remove_items(queue: &mut Vec<QueueItem>, indices: &mut [usize]) {
    indices.sort_unstable();
    let mut prev = None;
    for &idx in indices.iter().rev() {
        // Skip duplicates
        if prev == Some(idx) {
            continue;
        }
        prev = Some(idx);
        if idx < queue.len() {
            queue.remove(idx);
        }
    }
}

/// Reset selected items back to Pending (re-queue).
/// Only resets items in a terminal state (Done, Failed, Skipped, Cancelled).
pub fn requeue_items(queue: &mut [QueueItem], indices: &[usize]) {
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
pub fn requeue_all(queue: &mut [QueueItem]) {
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
    queue.retain(|item| {
        item.status == QueueItemStatus::Pending
            || item.status == QueueItemStatus::Probing
            || item.status == QueueItemStatus::Encoding
    });
}

/// Move a queue item from one index to another.
pub fn move_item(queue: &mut Vec<QueueItem>, from: usize, to: usize) {
    if from >= queue.len() || to >= queue.len() || from == to {
        return;
    }
    let item = queue.remove(from);
    queue.insert(to, item);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_item(name: &str, status: QueueItemStatus) -> QueueItem {
        QueueItem {
            full_path: format!("/tmp/{name}"),
            file_name: name.to_string(),
            base_name: name.to_string(),
            status,
            source_bytes: 0,
            probe: ProbeResult::default(),
        }
    }

    #[test]
    fn is_supported_extension_known() {
        assert!(is_supported_extension("video.mkv"));
        assert!(is_supported_extension("video.mp4"));
        assert!(is_supported_extension("video.avi"));
        assert!(is_supported_extension("video.webm"));
    }

    #[test]
    fn is_supported_extension_unsupported() {
        assert!(!is_supported_extension("doc.txt"));
        assert!(!is_supported_extension("pic.jpg"));
        assert!(!is_supported_extension("doc.pdf"));
    }

    #[test]
    fn is_supported_extension_case_insensitive() {
        assert!(is_supported_extension("video.MKV"));
        assert!(is_supported_extension("video.Mp4"));
    }

    #[test]
    fn is_supported_extension_none() {
        assert!(!is_supported_extension("noext"));
    }

    #[test]
    fn remove_items_middle() {
        let mut queue = vec![
            make_item("A", QueueItemStatus::Pending),
            make_item("B", QueueItemStatus::Pending),
            make_item("C", QueueItemStatus::Pending),
            make_item("D", QueueItemStatus::Pending),
            make_item("E", QueueItemStatus::Pending),
        ];
        remove_items(&mut queue, &mut [1, 3]);
        let names: Vec<&str> = queue.iter().map(|i| i.file_name.as_str()).collect();
        assert_eq!(names, vec!["A", "C", "E"]);
    }

    #[test]
    fn remove_items_empty_indices() {
        let mut queue = vec![
            make_item("A", QueueItemStatus::Pending),
            make_item("B", QueueItemStatus::Pending),
            make_item("C", QueueItemStatus::Pending),
        ];
        remove_items(&mut queue, &mut []);
        assert_eq!(queue.len(), 3);
    }

    #[test]
    fn requeue_done_and_failed() {
        let mut queue = vec![
            make_item("A", QueueItemStatus::Done),
            make_item("B", QueueItemStatus::Failed),
            make_item("C", QueueItemStatus::Cancelled),
        ];
        requeue_items(&mut queue, &[0, 1, 2]);
        assert_eq!(queue[0].status, QueueItemStatus::Pending);
        assert_eq!(queue[1].status, QueueItemStatus::Pending);
        assert_eq!(queue[2].status, QueueItemStatus::Pending);
    }

    #[test]
    fn requeue_leaves_pending_alone() {
        let mut queue = vec![make_item("A", QueueItemStatus::Pending)];
        requeue_items(&mut queue, &[0]);
        assert_eq!(queue[0].status, QueueItemStatus::Pending);
    }

    #[test]
    fn requeue_all_mixed() {
        let mut queue = vec![
            make_item("A", QueueItemStatus::Pending),
            make_item("B", QueueItemStatus::Probing),
            make_item("C", QueueItemStatus::Encoding),
            make_item("D", QueueItemStatus::Done),
            make_item("E", QueueItemStatus::Failed),
            make_item("F", QueueItemStatus::Skipped),
            make_item("G", QueueItemStatus::Cancelled),
        ];
        requeue_all(&mut queue);
        assert_eq!(queue[0].status, QueueItemStatus::Pending);
        assert_eq!(queue[1].status, QueueItemStatus::Probing);
        assert_eq!(queue[2].status, QueueItemStatus::Encoding);
        assert_eq!(queue[3].status, QueueItemStatus::Pending);
        assert_eq!(queue[4].status, QueueItemStatus::Pending);
        assert_eq!(queue[5].status, QueueItemStatus::Pending);
        assert_eq!(queue[6].status, QueueItemStatus::Pending);
    }

    #[test]
    fn clear_non_pending_retains_active() {
        let mut queue = vec![
            make_item("A", QueueItemStatus::Pending),
            make_item("B", QueueItemStatus::Probing),
            make_item("C", QueueItemStatus::Encoding),
            make_item("D", QueueItemStatus::Done),
            make_item("E", QueueItemStatus::Failed),
            make_item("F", QueueItemStatus::Skipped),
            make_item("G", QueueItemStatus::Cancelled),
        ];
        clear_non_pending(&mut queue);
        assert_eq!(queue.len(), 3);
        let names: Vec<&str> = queue.iter().map(|i| i.file_name.as_str()).collect();
        assert_eq!(names, vec!["A", "B", "C"]);
    }

    #[test]
    fn move_item_forward() {
        let mut queue = vec![
            make_item("A", QueueItemStatus::Pending),
            make_item("B", QueueItemStatus::Pending),
            make_item("C", QueueItemStatus::Pending),
            make_item("D", QueueItemStatus::Pending),
        ];
        move_item(&mut queue, 0, 2);
        let names: Vec<&str> = queue.iter().map(|i| i.file_name.as_str()).collect();
        assert_eq!(names, vec!["B", "C", "A", "D"]);
    }

    #[test]
    fn move_item_backward() {
        let mut queue = vec![
            make_item("A", QueueItemStatus::Pending),
            make_item("B", QueueItemStatus::Pending),
            make_item("C", QueueItemStatus::Pending),
            make_item("D", QueueItemStatus::Pending),
        ];
        move_item(&mut queue, 3, 1);
        let names: Vec<&str> = queue.iter().map(|i| i.file_name.as_str()).collect();
        assert_eq!(names, vec!["A", "D", "B", "C"]);
    }
}
