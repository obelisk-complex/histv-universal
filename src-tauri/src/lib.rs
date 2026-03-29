pub mod events;
pub mod encoder;
pub mod ffmpeg;
pub mod mkv_tags;
pub mod probe;
pub mod queue;
pub mod remote;
pub mod disk_monitor;
pub mod staging;
pub mod webp_decode;

#[cfg(feature = "custom-protocol")]
mod config;
#[cfg(feature = "custom-protocol")]
mod tauri_sink;
#[cfg(feature = "custom-protocol")]
mod tauri_batch_control;
#[cfg(feature = "custom-protocol")]
mod themes;

use std::sync::atomic::AtomicBool;
#[cfg(feature = "custom-protocol")]
use std::sync::{atomic::Ordering, Arc};
use tokio::sync::Mutex;

pub use encoder::EncoderInfo;
pub use events::{EventSink, BatchControl};
pub use probe::ProbeResult;
pub use queue::{AddResult, BatchState, QueueItem, QueueItemStatus};

#[cfg(feature = "custom-protocol")]
pub use config::AppConfig;
#[cfg(feature = "custom-protocol")]
pub use themes::Theme;

/// Shared application state accessible from all Tauri commands.
pub struct AppState {
    pub queue: Mutex<Vec<QueueItem>>,
    pub batch: Mutex<BatchState>,
    pub detected_video_encoders: Mutex<Vec<EncoderInfo>>,
    pub detected_audio_encoders: Mutex<Vec<String>>,
    #[cfg(feature = "custom-protocol")]
    pub config: Mutex<AppConfig>,
    pub encoder_detection_done: AtomicBool,
    pub ffmpeg_missing: AtomicBool,
    #[cfg(feature = "custom-protocol")]
    pub themes: Mutex<Vec<Theme>>,
}

// ── Tauri commands ──────────────────────────────────────────────
//
// All commands below are GUI-only and gated behind the custom-protocol feature.

#[cfg(feature = "custom-protocol")]
mod gui_commands {
    use super::*;
    use tauri::Emitter;
	
	#[tauri::command]
	#[allow(dead_code)]
    pub fn is_flatpak() -> bool {
        std::env::var("FLATPAK_ID").is_ok()
    }

    #[tauri::command]
    pub async fn get_themes(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<Theme>, String> {
        let t = state.themes.lock().await;
        Ok(t.clone())
    }

    #[tauri::command]
    pub async fn load_themes(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<Vec<Theme>, String> {
        let loaded = themes::scan_themes_folder(&app);
        let mut t = state.themes.lock().await;
        *t = loaded.clone();
        Ok(loaded)
    }

    #[tauri::command]
    pub async fn get_config(state: tauri::State<'_, Arc<AppState>>) -> Result<AppConfig, String> {
        let c = state.config.lock().await;
        Ok(c.clone())
    }

    #[tauri::command]
    pub async fn save_config(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
        config: AppConfig,
    ) -> Result<(), String> {
        let mut c = state.config.lock().await;
        *c = config.clone();
        config::save_config(&app, &config).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn get_encoder_detection_status(
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<bool, String> {
        Ok(state.encoder_detection_done.load(Ordering::Relaxed))
    }

    #[tauri::command]
    pub async fn get_ffmpeg_missing_status(
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<bool, String> {
        Ok(state.ffmpeg_missing.load(Ordering::Relaxed))
    }

    #[tauri::command]
    pub async fn get_detected_encoders(
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<(Vec<EncoderInfo>, Vec<String>), String> {
        let ve = state.detected_video_encoders.lock().await;
        let ae = state.detected_audio_encoders.lock().await;
        Ok((ve.clone(), ae.clone()))
    }

    #[tauri::command]
    pub async fn add_files_to_queue(
        state: tauri::State<'_, Arc<AppState>>,
        paths: Vec<String>,
    ) -> Result<AddResult, String> {
        let mut q = state.queue.lock().await;
        let result = queue::add_paths_to_queue(&mut q, &paths);
        Ok(result)
    }

    #[tauri::command]
    pub async fn remove_queue_items(
        state: tauri::State<'_, Arc<AppState>>,
        mut indices: Vec<usize>,
    ) -> Result<(), String> {
        let mut q = state.queue.lock().await;
        queue::remove_items(&mut q, &mut indices);
        Ok(())
    }

    #[tauri::command]
    pub async fn clear_completed(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut q = state.queue.lock().await;
        q.retain(|item| item.status != QueueItemStatus::Done);
        Ok(())
    }

    #[tauri::command]
    pub async fn clear_non_pending(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut q = state.queue.lock().await;
        queue::clear_non_pending(&mut q);
        Ok(())
    }

    #[tauri::command]
    pub async fn clear_all_queue(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut q = state.queue.lock().await;
        q.clear();
        Ok(())
    }

    #[tauri::command]
    pub async fn requeue_items(
        state: tauri::State<'_, Arc<AppState>>,
        indices: Vec<usize>,
    ) -> Result<(), String> {
        let mut q = state.queue.lock().await;
        queue::requeue_items(&mut q, &indices);
        Ok(())
    }

    #[tauri::command]
    pub async fn requeue_all(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut q = state.queue.lock().await;
        queue::requeue_all(&mut q);
        Ok(())
    }

    #[tauri::command]
    pub async fn move_queue_item(
        state: tauri::State<'_, Arc<AppState>>,
        from: usize,
        to: usize,
    ) -> Result<(), String> {
        let mut q = state.queue.lock().await;
        queue::move_item(&mut q, from, to);
        Ok(())
    }

    #[tauri::command]
    pub async fn reveal_file(path: String) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            tokio::process::Command::new("explorer")
                .args(["/select,", &path])
                .spawn()
                .map_err(|e| format!("Could not reveal file: {e}"))?;
        }
        #[cfg(target_os = "macos")]
        {
            tokio::process::Command::new("open")
                .args(["-R", &path])
                .spawn()
                .map_err(|e| format!("Could not reveal file: {e}"))?;
        }
        #[cfg(target_os = "linux")]
        {
            if let Some(parent) = std::path::Path::new(&path).parent() {
                tokio::process::Command::new("xdg-open")
                    .arg(parent.to_string_lossy().as_ref())
                    .spawn()
                    .map_err(|e| format!("Could not reveal file: {e}"))?;
            }
        }
        Ok(())
    }

    #[tauri::command]
    pub async fn open_file(path: String) -> Result<(), String> {
        #[cfg(target_os = "windows")]
        {
            tokio::process::Command::new("cmd")
                .args(["/C", "start", "", &path])
                .spawn()
                .map_err(|e| format!("Could not open file: {e}"))?;
        }
        #[cfg(target_os = "macos")]
        {
            tokio::process::Command::new("open")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("Could not open file: {e}"))?;
        }
        #[cfg(target_os = "linux")]
        {
            tokio::process::Command::new("xdg-open")
                .arg(&path)
                .spawn()
                .map_err(|e| format!("Could not open file: {e}"))?;
        }
        Ok(())
    }

    #[tauri::command]
    pub async fn get_queue(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<QueueItem>, String> {
        let q = state.queue.lock().await;
        Ok(q.clone())
    }

    #[tauri::command]
    pub async fn probe_file(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
        index: usize,
    ) -> Result<ProbeResult, String> {
        let sink = tauri_sink::TauriSink::new(app.clone());

        let file_path = {
            let q = state.queue.lock().await;
            if index >= q.len() {
                return Err("Index out of range".to_string());
            }
            q[index].full_path.clone()
        };

        // Update status to Probing
        {
            let mut q = state.queue.lock().await;
            if index < q.len() {
                q[index].status = QueueItemStatus::Probing;
            }
        }
        sink.queue_item_updated(index, "Probing");

        let result = probe::probe_file(&file_path, &sink).await;

        // Update the queue item with probe results
        {
            let mut q = state.queue.lock().await;
            if index < q.len() {
                match &result {
                    Ok(pr) => {
                        q[index].video_codec = pr.video_codec.clone();
                        q[index].video_width = pr.video_width;
                        q[index].video_height = pr.video_height;
                        q[index].video_bitrate_bps = pr.video_bitrate_bps;
                        q[index].video_bitrate_mbps = pr.video_bitrate_mbps;
                        q[index].is_hdr = pr.is_hdr;
                        q[index].color_transfer = pr.color_transfer.clone();
                        q[index].audio_streams = pr.audio_streams.clone();
                        q[index].duration_secs = pr.duration_secs;
                        q[index].status = QueueItemStatus::Pending;

                        // Lightweight MKV tag repair: fix stale statistics
                        // so the queue shows the real bitrate from import.
                        if file_path.ends_with(".mkv") && pr.duration_secs > 0.0 {
                            if let Ok(file_size) = std::fs::metadata(&file_path).map(|m| m.len()) {
                                let audio_total_bps: u64 = pr.audio_streams.iter()
                                    .map(|s| s.bitrate_kbps as u64 * 1000)
                                    .sum();
                                if let Ok((n, bps)) = crate::mkv_tags::lightweight_repair(
                                    std::path::Path::new(&file_path),
                                    file_size, pr.duration_secs, audio_total_bps, None,
                                ) {
                                    if n > 0 {
                                        let corrected_mbps = bps as f64 / 1_000_000.0;
                                        q[index].video_bitrate_bps = bps as f64;
                                        q[index].video_bitrate_mbps = corrected_mbps;
                                    }
                                }
                            }
                        }
                    }
                    Err(_) => {
                        q[index].status = QueueItemStatus::Failed;
                    }
                }
            }
        }
        sink.queue_item_probed(index);

        result
    }

    /// Repair stale MKV stream statistics tags on queued files.
    /// Probes each file, computes correct values, and patches in-place.
    /// Returns the number of files that were updated.
    #[tauri::command]
    pub async fn repair_tags(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
        indices: Vec<usize>,
    ) -> Result<u32, String> {
        let sink = tauri_sink::TauriSink::new(app.clone());
        let mut repaired: u32 = 0;
        let total = indices.len();

        for (i, &index) in indices.iter().enumerate() {
            let file_path = {
                let q = state.queue.lock().await;
                if index >= q.len() { continue; }
                q[index].full_path.clone()
            };

            let path = std::path::Path::new(&file_path);
            let fname = path.file_name().unwrap_or_default().to_string_lossy();
            sink.log(&format!("[repair] ({}/{}) {}", i + 1, total, fname));

            match crate::mkv_tags::repair_file_tags(path, &sink).await {
                Ok((n, bps)) if n > 0 => {
                    let mbps = bps as f64 / 1_000_000.0;
                    sink.log(&format!(
                        "[repair] Updated {} tag{} (video: {:.2}Mbps)",
                        n, if n == 1 { "" } else { "s" }, mbps
                    ));
                    {
                        let mut q = state.queue.lock().await;
                        if index < q.len() {
                            q[index].video_bitrate_bps = bps as f64;
                            q[index].video_bitrate_mbps = mbps;
                        }
                    }
                    sink.queue_item_probed(index);
                    repaired += 1;
                }
                Ok(_) => {
                    sink.log("[repair] No statistics tags to update");
                }
                Err(e) => {
                    sink.log(&format!("[repair] ERROR: {}", e));
                }
            }
        }

        sink.log(&format!("[repair] Complete: {} of {} file{} updated",
            repaired, total, if total == 1 { "" } else { "s" }));

        Ok(repaired)
    }

    /// Deep repair: scan every packet to compute exact statistics,
    /// then patch all MKV tags with byte-accurate values.
    /// Returns the number of files updated.
    #[tauri::command]
    pub async fn deep_repair_tags(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
        indices: Vec<usize>,
    ) -> Result<u32, String> {
        let sink = tauri_sink::TauriSink::new(app.clone());
        let mut repaired: u32 = 0;
        let total = indices.len();

        for (i, &index) in indices.iter().enumerate() {
            let file_path = {
                let q = state.queue.lock().await;
                if index >= q.len() { continue; }
                q[index].full_path.clone()
            };

            let path = std::path::Path::new(&file_path);
            let fname = path.file_name().unwrap_or_default().to_string_lossy();
            sink.log(&format!("[deep-repair] ({}/{}) {}", i + 1, total, fname));

            match crate::mkv_tags::deep_repair(path, &sink).await {
                Ok((n, bps)) if n > 0 => {
                    let mbps = bps as f64 / 1_000_000.0;
                    sink.log(&format!(
                        "[deep-repair] Updated {} tag{} (video: {:.2}Mbps)",
                        n, if n == 1 { "" } else { "s" }, mbps
                    ));
                    {
                        let mut q = state.queue.lock().await;
                        if index < q.len() {
                            q[index].video_bitrate_bps = bps as f64;
                            q[index].video_bitrate_mbps = mbps;
                        }
                    }
                    sink.queue_item_probed(index);
                    repaired += 1;
                }
                Ok(_) => {
                    sink.log("[deep-repair] No statistics tags to update");
                }
                Err(e) => {
                    sink.log(&format!("[deep-repair] ERROR: {}", e));
                }
            }
        }

        sink.log(&format!("[deep-repair] Complete: {} of {} file{} updated",
            repaired, total, if total == 1 { "" } else { "s" }));

        Ok(repaired)
    }

    #[tauri::command]
    pub async fn start_batch(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
        settings: serde_json::Value,
    ) -> Result<(), String> {
        let state_arc = state.inner().clone();
        let sink = Arc::new(tauri_sink::TauriSink::new(app.clone()));

        // Parse settings from the frontend JSON
        let output_mode = settings["outputMode"]
            .as_str()
            .unwrap_or("folder")
            .to_string();

        let raw_output_folder = settings["outputFolder"]
            .as_str()
            .unwrap_or("output")
            .to_string();

        // Resolve relative output paths (AppImage fix)
        let output_folder = if std::path::Path::new(&raw_output_folder).is_relative() {
            let base = encoder::resolve_base_dir();
            let resolved = base.join(&raw_output_folder);
            sink.log(&format!(
                "[batch] Resolved relative output '{}' to '{}'",
                raw_output_folder, resolved.display()
            ));
            resolved.to_string_lossy().to_string()
        } else {
            raw_output_folder
        };

        let batch_settings = encoder::BatchSettings {
            output_folder,
            output_container: settings["outputContainer"]
                .as_str().unwrap_or("mkv").to_string(),
            output_mode,
            threshold: settings["targetBitrate"]
                .as_f64().unwrap_or(5.0),
            qp_i: settings["qpI"].as_u64().unwrap_or(20) as u32,
            qp_p: settings["qpP"].as_u64().unwrap_or(22) as u32,
            crf_val: settings["crf"].as_u64().unwrap_or(20) as u32,
            rate_control_mode: settings["rateControlMode"]
                .as_str().unwrap_or("QP").to_string(),
            video_encoder: settings["videoEncoder"]
                .as_str().unwrap_or("libx265").to_string(),
            codec_family: match settings["codecFamily"].as_str().unwrap_or("HEVC") {
    "H.264" | "h264" => "h264",
    _ => "hevc",
}.to_string(),
            audio_encoder: settings["audioEncoder"]
                .as_str().unwrap_or("ac3").to_string(),
            audio_cap: settings["audioBitrateCap"]
                .as_u64().unwrap_or(640) as u32,
            pix_fmt: settings["pixFmt"]
                .as_str().unwrap_or("yuv420p").to_string(),
            delete_source: settings["deleteSource"]
                .as_bool().unwrap_or(false),
            save_log: settings["saveLog"]
                .as_bool().unwrap_or(false),
            post_command: None, // GUI uses post-batch action events instead
            peak_multiplier: settings["peakMultiplier"]
                .as_f64().unwrap_or(1.5),
            threads: settings["threads"]
                .as_u64().unwrap_or(0) as u32,
            low_priority: settings["lowPriority"]
                .as_bool().unwrap_or(false),
            precision_mode: settings["precisionMode"]
                .as_bool().unwrap_or(false),
        };

        let show_toast = settings["showToast"].as_bool().unwrap_or(false);
        let post_action = settings["postAction"]
            .as_str().unwrap_or("None").to_string();
        let post_countdown: u32 = settings["postCountdown"]
            .as_u64().unwrap_or(0) as u32;

        // Validate output folder (only in folder mode)
        if batch_settings.output_mode == "folder" {
            let out_path = std::path::Path::new(&batch_settings.output_folder);
            if !out_path.exists() {
                std::fs::create_dir_all(out_path)
                    .map_err(|e| format!("Could not create output folder '{}': {e}", batch_settings.output_folder))?;
            }
            let test_path = out_path.join(".histv_write_test");
            std::fs::write(&test_path, b"")
                .map_err(|e| format!("Output folder '{}' is not writable: {e}", batch_settings.output_folder))?;
            let _ = std::fs::remove_file(&test_path);
        }

        // Reset batch state and set running
        {
            let mut b = state_arc.batch.lock().await;
            b.running = true;
            b.cancel_current = false;
            b.cancel_all = false;
            b.paused = false;
            b.overwrite_always = settings["overwrite"]
                .as_bool().unwrap_or(false);
            b.hw_fallback_offered = false;
            b.overwrite_response = None;
            b.fallback_response = None;
        }

        // Clone queue items for the encoding loop (#14).
        // The encoder indexes by original queue position, so we clone the
        // full queue but record which indices are pending. After the loop,
        // only those indices are synced back (#15).
        let (mut queue_items, pending_indices): (Vec<queue::QueueItem>, Vec<usize>) = {
            let q = state_arc.queue.lock().await;
            let pending: Vec<usize> = q.iter()
                .enumerate()
                .filter(|(_, item)| item.status == queue::QueueItemStatus::Pending)
                .map(|(i, _)| i)
                .collect();
            (q.clone(), pending)
        };

        if pending_indices.is_empty() {
            let mut b = state_arc.batch.lock().await;
            b.running = false;
            return Err("No pending files in the queue.".into());
        }

        // Create batch control wrapping the GUI's shared state
        let batch_ctrl = Arc::new(
            tauri_batch_control::GuiBatchControl::new(
                state_arc.clone(),
                app.clone(),
            )
        );

        // Spawn the encoding loop in the background
        let state_for_task = state_arc.clone();
        let sink_for_task = sink.clone();
        let app_for_task = app.clone();
        tokio::spawn(async move {
            let (done, failed, skipped, _was_cancelled) =
                encoder::run_encode_loop(
                    sink_for_task.as_ref(),
                    batch_ctrl.as_ref(),
                    &mut queue_items,
                    &batch_settings,
                ).await;

            // Sync only the items that were pending at batch start (#15).
            // Non-pending items were never touched by the encoder and don't
            // need their status copied back.
            //
            // Note: the encoder's batch_finished event fires before this sync
            // (it's called inside run_encode_loop). The frontend re-fetches the
            // queue on batch-finished, so we emit queue-sync-complete afterwards
            // to trigger a second fetch with the final statuses.
            {
                let mut q = state_for_task.queue.lock().await;
                for &i in &pending_indices {
                    if i < q.len() && i < queue_items.len() {
                        q[i].status = queue_items[i].status.clone();
                    }
                }
            }
            let _ = app_for_task.emit("queue-sync-complete", ());

            // GUI-specific post-batch actions
            if show_toast {
                sink_for_task.toast(&format!(
                    "Done: {}  Failed: {}  Skipped: {}",
                    done, failed, skipped
                ));
            }

            if post_action != "None" {
                sink_for_task.post_batch(&post_action, post_countdown);
            }

            // Mark batch as finished
            {
                let mut b = state_for_task.batch.lock().await;
                b.running = false;
            }
        });

        Ok(())
    }

    #[tauri::command]
    pub async fn cancel_current(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut b = state.batch.lock().await;
        b.cancel_current = true;
        Ok(())
    }

    #[tauri::command]
    pub async fn cancel_all(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut b = state.batch.lock().await;
        b.cancel_all = true;
        Ok(())
    }

    #[tauri::command]
    pub async fn toggle_pause(state: tauri::State<'_, Arc<AppState>>) -> Result<bool, String> {
        let mut b = state.batch.lock().await;
        b.paused = !b.paused;
        Ok(b.paused)
    }

    #[tauri::command]
    pub async fn is_batch_running(state: tauri::State<'_, Arc<AppState>>) -> Result<bool, String> {
        let b = state.batch.lock().await;
        Ok(b.running)
    }

    #[tauri::command]
    pub async fn respond_overwrite(
        state: tauri::State<'_, Arc<AppState>>,
        response: String,
    ) -> Result<(), String> {
        let mut b = state.batch.lock().await;
        b.overwrite_response = Some(response);
        Ok(())
    }

    #[tauri::command]
    pub async fn respond_fallback(
        state: tauri::State<'_, Arc<AppState>>,
        response: String,
    ) -> Result<(), String> {
        let mut b = state.batch.lock().await;
        b.fallback_response = Some(response);
        Ok(())
    }

    #[tauri::command]
    pub async fn execute_post_batch_action(action: String) -> Result<(), String> {
        encoder::execute_post_action(&action).await
    }

    #[tauri::command]
    pub async fn check_ffmpeg_available() -> Result<bool, String> {
        Ok(ffmpeg::is_available().await)
    }

    #[tauri::command]
    pub fn get_ffmpeg_dir() -> Result<String, String> {
        ffmpeg::exe_dir()
            .map(|d| d.to_string_lossy().to_string())
            .ok_or_else(|| "Could not determine executable directory".to_string())
    }

    #[cfg(feature = "downloader")]
    #[tauri::command]
    pub async fn download_ffmpeg(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<(), String> {
        use tauri::Manager;

        let sink = tauri_sink::TauriSink::new(app.clone());

        let target_dir = ffmpeg::app_data_bin_dir()
            .ok_or_else(|| "Could not determine app data directory".to_string())?;

        // Create the directory if it doesn't exist
        std::fs::create_dir_all(&target_dir)
            .map_err(|e| format!("Could not create directory {}: {e}", target_dir.display()))?;

        ffmpeg::download_to_dir(&target_dir, &sink).await?;

        // Re-resolve binary paths now that ffmpeg is in app-data
        let resource_dir = app.path().resource_dir().ok();
        ffmpeg::reinit(resource_dir.as_deref(), &sink);

        // Re-run encoder detection now that ffmpeg is available
        sink.log("[detect] ffmpeg downloaded, re-running encoder detection...");
        let (video, audio) = encoder::detect_encoders(&sink).await;
        {
            let mut ve = state.detected_video_encoders.lock().await;
            *ve = video;
        }
        {
            let mut ae = state.detected_audio_encoders.lock().await;
            *ae = audio;
        }
        state.encoder_detection_done.store(true, Ordering::Relaxed);
        state.ffmpeg_missing.store(false, Ordering::Relaxed);
        let _ = app.emit("encoder-detection-done", ());
        Ok(())
    }

    #[cfg(not(feature = "downloader"))]
    #[tauri::command]
    pub async fn download_ffmpeg(
        _app: tauri::AppHandle,
        _state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<(), String> {
        Err("Download feature not available in this build.".to_string())
    }
}

// ── App entry ───────────────────────────────────────────────────

#[cfg(feature = "custom-protocol")]
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    use gui_commands::*;
    use tauri::{Emitter, Manager};

    let app_state = Arc::new(AppState {
        queue: Mutex::new(Vec::new()),
        batch: Mutex::new(BatchState::default()),
        detected_video_encoders: Mutex::new(Vec::new()),
        detected_audio_encoders: Mutex::new(Vec::new()),
        config: Mutex::new(AppConfig::default()),
        encoder_detection_done: AtomicBool::new(false),
        ffmpeg_missing: AtomicBool::new(false),
        themes: Mutex::new(Vec::new()),
    });

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_clipboard_manager::init())
        .plugin(tauri_plugin_fs::init())
        .plugin(tauri_plugin_shell::init())
        .manage(app_state.clone())
        .invoke_handler(tauri::generate_handler![
		    is_flatpak,
            get_themes,
            load_themes,
            get_config,
            save_config,
            get_encoder_detection_status,
            get_ffmpeg_missing_status,
            get_detected_encoders,
            add_files_to_queue,
            remove_queue_items,
            clear_completed,
            clear_non_pending,
            clear_all_queue,
            requeue_items,
            requeue_all,
            move_queue_item,
            get_queue,
            probe_file,
            repair_tags,
            deep_repair_tags,
            start_batch,
            cancel_current,
            cancel_all,
            toggle_pause,
            is_batch_running,
            respond_overwrite,
            respond_fallback,
            execute_post_batch_action,
            check_ffmpeg_available,
            get_ffmpeg_dir,
            download_ffmpeg,
            reveal_file,
            open_file,
        ])
        .setup(move |app| {
            let sink = tauri_sink::TauriSink::new(app.handle().clone());

            // Resolve ffmpeg/ffprobe binary paths (sidecar or PATH)
            let resource_dir = app.path().resource_dir().ok();
            ffmpeg::init(resource_dir.as_deref(), None, &sink);

            // Load config
            let loaded_config = config::load_config(&app.handle());
            let themes_loaded = themes::scan_themes_folder(&app.handle());

            let state = app_state.clone();
            let app_handle = app.handle().clone();

            // Set config and themes synchronously via blocking
            tauri::async_runtime::block_on(async {
                {
                    let mut c = state.config.lock().await;
                    *c = loaded_config;
                }
                {
                    let mut t = state.themes.lock().await;
                    *t = themes_loaded;
                }
            });

            // Spawn background encoder detection (with ffmpeg availability check)
            let state_for_detect = app_state.clone();
            let handle_for_detect = app_handle.clone();
            tauri::async_runtime::spawn(async move {
                let detect_sink = tauri_sink::TauriSink::new(handle_for_detect.clone());

                // Check if ffmpeg is reachable first
                if !ffmpeg::is_available().await {
                    state_for_detect.ffmpeg_missing.store(true, Ordering::Relaxed);
                    let _ = handle_for_detect.emit("ffmpeg-missing", ());
                    detect_sink.log("[detect] ffmpeg not found - waiting for user to install or download it");
                    return;
                }

                let (video, audio) =
                    encoder::detect_encoders(&detect_sink).await;
                {
                    let mut ve = state_for_detect.detected_video_encoders.lock().await;
                    *ve = video;
                }
                {
                    let mut ae = state_for_detect.detected_audio_encoders.lock().await;
                    *ae = audio;
                }
                state_for_detect.encoder_detection_done.store(true, Ordering::Relaxed);
                let _ = handle_for_detect.emit("encoder-detection-done", ());
            });

            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

// Note: Window close confirmation during a batch (§16.4) is handled in the
// frontend via the beforeunload event combined with checking is_batch_running.
// Tauri v2's on_window_event with CloseRequested can be added here if the
// frontend approach proves insufficient on a particular platform.