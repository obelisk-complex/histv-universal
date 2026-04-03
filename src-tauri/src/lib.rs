pub mod disk_monitor;
#[cfg(feature = "dovi")]
pub mod dovi_pipeline;
pub mod dovi_tools;
pub mod encoder;
pub mod events;
pub mod ffmpeg;
#[cfg(feature = "dovi")]
pub mod hdr10plus_pipeline;
pub mod hevc_utils;
pub mod mkv_tags;
pub mod probe;
pub mod queue;
pub mod remote;
pub mod staging;
pub mod webp_decode;

#[cfg(feature = "custom-protocol")]
mod config;
#[cfg(feature = "custom-protocol")]
mod tauri_batch_control;
#[cfg(feature = "custom-protocol")]
mod tauri_sink;
#[cfg(feature = "custom-protocol")]
mod themes;

use std::sync::atomic::AtomicBool;
#[cfg(feature = "custom-protocol")]
use std::sync::{atomic::Ordering, Arc};
use tokio::sync::{Mutex, RwLock};

pub use encoder::EncoderInfo;
pub use events::{BatchControl, EventSink};
pub use probe::ProbeResult;
pub use queue::{AddResult, BatchState, QueueItem, QueueItemStatus};

#[cfg(feature = "custom-protocol")]
pub use config::AppConfig;
#[cfg(feature = "custom-protocol")]
pub use themes::Theme;

/// Shared application state accessible from all Tauri commands.
pub struct AppState {
    pub queue: RwLock<Vec<QueueItem>>,
    pub batch: Mutex<BatchState>,
    pub detected_video_encoders: RwLock<Vec<EncoderInfo>>,
    pub detected_audio_encoders: RwLock<Vec<String>>,
    #[cfg(feature = "custom-protocol")]
    pub config: RwLock<AppConfig>,
    pub encoder_detection_done: AtomicBool,
    pub ffmpeg_missing: AtomicBool,
    #[cfg(feature = "custom-protocol")]
    pub themes: RwLock<Vec<Theme>>,
}

// ── Tauri commands ──────────────────────────────────────────────
//
// All commands below are GUI-only and gated behind the custom-protocol feature.

#[cfg(feature = "custom-protocol")]
mod gui_commands {
    use super::*;
    use tauri::Emitter;

    /// Wraps an `EventSink` and translates local (compact) queue indices
    /// back to original queue positions for `queue_item_updated` /
    /// `queue_item_probed` events, so the frontend sees the right rows.
    struct IndexMappingSink {
        inner: Arc<dyn EventSink>,
        /// `index_map[local_idx] == original_queue_idx`
        index_map: Vec<usize>,
    }

    impl EventSink for IndexMappingSink {
        fn log(&self, message: &str) {
            self.inner.log(message);
        }
        fn file_progress(
            &self,
            percent: f64,
            time_secs: f64,
            total_secs: f64,
            pass: Option<(u8, u8)>,
        ) {
            self.inner
                .file_progress(percent, time_secs, total_secs, pass);
        }
        fn batch_progress(&self, current: u32, total: usize) {
            self.inner.batch_progress(current, total);
        }
        fn batch_status(&self, message: &str) {
            self.inner.batch_status(message);
        }
        fn queue_item_updated(&self, index: usize, status: &str) {
            let mapped = self.index_map.get(index).copied().unwrap_or(index);
            self.inner.queue_item_updated(mapped, status);
        }
        fn queue_item_probed(&self, index: usize) {
            let mapped = self.index_map.get(index).copied().unwrap_or(index);
            self.inner.queue_item_probed(mapped);
        }
        fn batch_started(&self) {
            self.inner.batch_started();
        }
        fn batch_finished(&self, done: u32, failed: u32, skipped: u32, duration: &str) {
            self.inner.batch_finished(done, failed, skipped, duration);
        }
        fn ffmpeg_stderr(&self, line: &str) {
            self.inner.ffmpeg_stderr(line);
        }
        fn batch_command(&self, cmd: &str) {
            self.inner.batch_command(cmd);
        }
        fn ffmpeg_download_progress(&self, message: &str) {
            self.inner.ffmpeg_download_progress(message);
        }
        fn toast(&self, message: &str) {
            self.inner.toast(message);
        }
        fn post_batch(&self, action: &str, countdown: u32) {
            self.inner.post_batch(action, countdown);
        }
        fn wave_progress(&self, wave: u32, total_waves: u32, file_in_wave: u32, wave_size: u32) {
            self.inner
                .wave_progress(wave, total_waves, file_in_wave, wave_size);
        }
        fn wave_status(&self, message: &str) {
            self.inner.wave_status(message);
        }
        fn batch_time_estimate(&self, elapsed_secs: f64, remaining_secs: f64) {
            self.inner.batch_time_estimate(elapsed_secs, remaining_secs);
        }
        fn wave_time_estimate(&self, elapsed_secs: f64, remaining_secs: f64) {
            self.inner.wave_time_estimate(elapsed_secs, remaining_secs);
        }
    }

    #[tauri::command]
    pub fn is_flatpak() -> bool {
        std::env::var("FLATPAK_ID").is_ok()
    }

    #[tauri::command]
    pub async fn get_themes(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<Theme>, String> {
        let t = state.themes.read().await;
        Ok(t.clone())
    }

    #[tauri::command]
    pub async fn load_themes(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<Vec<Theme>, String> {
        let loaded = themes::scan_themes_folder(&app);
        let mut t = state.themes.write().await;
        *t = loaded.clone();
        Ok(loaded)
    }

    #[tauri::command]
    pub async fn get_config(state: tauri::State<'_, Arc<AppState>>) -> Result<AppConfig, String> {
        let c = state.config.read().await;
        Ok(c.clone())
    }

    #[tauri::command]
    pub async fn save_config(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
        config: AppConfig,
    ) -> Result<(), String> {
        let mut c = state.config.write().await;
        *c = config.clone();
        config::save_config(&app, &config).map_err(|e| e.to_string())
    }

    #[tauri::command]
    pub async fn get_encoder_detection_status(
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<bool, String> {
        Ok(state.encoder_detection_done.load(Ordering::Acquire))
    }

    #[tauri::command]
    pub async fn get_ffmpeg_missing_status(
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<bool, String> {
        Ok(state.ffmpeg_missing.load(Ordering::Acquire))
    }

    #[tauri::command]
    pub async fn get_detected_encoders(
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<(Vec<EncoderInfo>, Vec<String>), String> {
        let ve = state.detected_video_encoders.read().await;
        let ae = state.detected_audio_encoders.read().await;
        Ok((ve.clone(), ae.clone()))
    }

    #[tauri::command]
    pub async fn add_files_to_queue(
        state: tauri::State<'_, Arc<AppState>>,
        paths: Vec<String>,
    ) -> Result<AddResult, String> {
        let mut q = state.queue.write().await;
        let result = queue::add_paths_to_queue(&mut q, &paths);
        Ok(result)
    }

    #[tauri::command]
    pub async fn remove_queue_items(
        state: tauri::State<'_, Arc<AppState>>,
        mut indices: Vec<usize>,
    ) -> Result<(), String> {
        let mut q = state.queue.write().await;
        queue::remove_items(&mut q, &mut indices);
        Ok(())
    }

    #[tauri::command]
    pub async fn clear_completed(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut q = state.queue.write().await;
        q.retain(|item| item.status != QueueItemStatus::Done);
        Ok(())
    }

    #[tauri::command]
    pub async fn clear_non_pending(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut q = state.queue.write().await;
        queue::clear_non_pending(&mut q);
        Ok(())
    }

    #[tauri::command]
    pub async fn clear_all_queue(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut q = state.queue.write().await;
        q.clear();
        Ok(())
    }

    #[tauri::command]
    pub async fn requeue_items(
        state: tauri::State<'_, Arc<AppState>>,
        indices: Vec<usize>,
    ) -> Result<(), String> {
        let mut q = state.queue.write().await;
        queue::requeue_items(&mut q, &indices);
        Ok(())
    }

    #[tauri::command]
    pub async fn requeue_all(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
        let mut q = state.queue.write().await;
        queue::requeue_all(&mut q);
        Ok(())
    }

    #[tauri::command]
    pub async fn move_queue_item(
        state: tauri::State<'_, Arc<AppState>>,
        from: usize,
        to: usize,
    ) -> Result<(), String> {
        let mut q = state.queue.write().await;
        queue::move_item(&mut q, from, to);
        Ok(())
    }

    #[tauri::command]
    pub async fn reveal_file(path: String) -> Result<(), String> {
        let p = std::path::Path::new(&path);
        if !p.exists() {
            return Err(format!("Path does not exist: {path}"));
        }
        #[cfg(target_os = "windows")]
        {
            // Use explorer.exe directly with /select, — safe because explorer
            // treats the argument as a file path, not a shell command.
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
            if let Some(parent) = p.parent() {
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
        let p = std::path::Path::new(&path);
        if !p.exists() {
            return Err(format!("Path does not exist: {path}"));
        }
        // Reject UNC paths on Windows to prevent NTLM relay attacks.
        #[cfg(target_os = "windows")]
        if path.starts_with("\\\\") {
            return Err("UNC paths are not supported".to_string());
        }
        #[cfg(target_os = "windows")]
        {
            // Use ShellExecuteW semantics via explorer — avoids cmd.exe shell
            // parsing which is vulnerable to argument injection.
            tokio::process::Command::new("explorer")
                .arg(&path)
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
    pub async fn get_queue(
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<Vec<QueueItem>, String> {
        let q = state.queue.read().await;
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
            let q = state.queue.read().await;
            if index >= q.len() {
                return Err("Index out of range".to_string());
            }
            q[index].full_path.clone()
        };

        // Update status to Probing
        {
            let mut q = state.queue.write().await;
            if index < q.len() {
                q[index].status = QueueItemStatus::Probing;
            }
        }
        sink.queue_item_updated(index, "Probing");

        let result = probe::probe_file(&file_path, &sink).await;

        // Update the queue item with probe results
        {
            let mut q = state.queue.write().await;
            if index < q.len() {
                match &result {
                    Ok(pr) => {
                        // Lightweight MKV tag repair: fix stale statistics
                        // so the queue shows the real bitrate from import.
                        let repair = crate::mkv_tags::repair_after_probe(
                            &file_path,
                            pr.duration_secs,
                            &pr.audio_streams,
                        );
                        q[index].probe = pr.clone();
                        q[index].status = QueueItemStatus::Pending;

                        if let Some(bps) = repair {
                            let corrected_mbps = bps as f64 / 1_000_000.0;
                            q[index].probe.video_bitrate_bps = bps as f64;
                            q[index].probe.video_bitrate_mbps = corrected_mbps;
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
                let q = state.queue.read().await;
                if index >= q.len() {
                    continue;
                }
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
                        n,
                        if n == 1 { "" } else { "s" },
                        mbps
                    ));
                    {
                        let mut q = state.queue.write().await;
                        if index < q.len() {
                            q[index].probe.video_bitrate_bps = bps as f64;
                            q[index].probe.video_bitrate_mbps = mbps;
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

        sink.log(&format!(
            "[repair] Complete: {} of {} file{} updated",
            repaired,
            total,
            if total == 1 { "" } else { "s" }
        ));

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
                let q = state.queue.read().await;
                if index >= q.len() {
                    continue;
                }
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
                        n,
                        if n == 1 { "" } else { "s" },
                        mbps
                    ));
                    {
                        let mut q = state.queue.write().await;
                        if index < q.len() {
                            q[index].probe.video_bitrate_bps = bps as f64;
                            q[index].probe.video_bitrate_mbps = mbps;
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

        sink.log(&format!(
            "[deep-repair] Complete: {} of {} file{} updated",
            repaired,
            total,
            if total == 1 { "" } else { "s" }
        ));

        Ok(repaired)
    }

    #[tauri::command]
    pub async fn preflight_check(
        state: tauri::State<'_, Arc<AppState>>,
    ) -> Result<Vec<encoder::PreflightWarning>, String> {
        let q = state.queue.read().await;
        Ok(encoder::preflight_scan(&q))
    }

    #[tauri::command]
    pub async fn start_batch(
        app: tauri::AppHandle,
        state: tauri::State<'_, Arc<AppState>>,
        settings: encoder::BatchRequest,
    ) -> Result<(), String> {
        let state_arc = state.inner().clone();
        let sink = Arc::new(tauri_sink::TauriSink::new(app.clone()));

        // Extract GUI-only fields before converting to BatchSettings
        let show_toast = settings.show_toast;
        let post_action = settings.post_action.clone();
        let post_countdown = settings.post_countdown;
        let overwrite = settings.overwrite;

        // Convert the validated request into BatchSettings (applies clamping)
        let mut batch_settings = settings.into_batch_settings();

        // Resolve relative output paths (AppImage fix)
        if std::path::Path::new(&batch_settings.output_folder).is_relative() {
            let base = encoder::resolve_base_dir();
            let resolved = base.join(&batch_settings.output_folder);
            sink.log(&format!(
                "[batch] Resolved relative output '{}' to '{}'",
                batch_settings.output_folder,
                resolved.display()
            ));
            batch_settings.output_folder = resolved.to_string_lossy().to_string();
        }

        // Validate output folder (only in folder mode)
        if batch_settings.output_mode == "folder" {
            let out_path = std::path::Path::new(&batch_settings.output_folder);
            if !out_path.exists() {
                std::fs::create_dir_all(out_path).map_err(|e| {
                    format!(
                        "Could not create output folder '{}': {e}",
                        batch_settings.output_folder
                    )
                })?;
            }
            let test_path = out_path.join(".histv_write_test");
            std::fs::write(&test_path, b"").map_err(|e| {
                format!(
                    "Output folder '{}' is not writable: {e}",
                    batch_settings.output_folder
                )
            })?;
            let _ = std::fs::remove_file(&test_path);
        }

        // Reset batch state and set running
        {
            let mut b = state_arc.batch.lock().await;
            b.running = true;
            b.cancel_current = false;
            b.cancel_all = false;
            b.paused = false;
            b.overwrite_always = overwrite;
            b.hw_fallback_offered = false;
            b.overwrite_response = None;
            b.fallback_response = None;
        }

        // Clone only the pending items for the encoding loop.
        // Previously the entire queue was cloned; now we build a compact
        // vec of just the pending items plus an index map so the encoder
        // works with local indices 0..N while the sink translates them
        // back to original queue positions for the frontend.
        let (mut pending_items, index_map): (Vec<queue::QueueItem>, Vec<usize>) = {
            let q = state_arc.queue.read().await;
            let mut items = Vec::new();
            let mut map = Vec::new();
            for (i, item) in q.iter().enumerate() {
                if item.status == queue::QueueItemStatus::Pending {
                    items.push(item.clone());
                    map.push(i); // map[local_idx] == original_queue_idx
                }
            }
            (items, map)
        };

        if pending_items.is_empty() {
            let mut b = state_arc.batch.lock().await;
            b.running = false;
            return Err("No pending files in the queue.".into());
        }

        // Create batch control wrapping the GUI's shared state
        let batch_ctrl = Arc::new(tauri_batch_control::GuiBatchControl::new(
            state_arc.clone(),
            app.clone(),
        ));

        // Build wave plan for remote file staging.
        // The planner receives the compact pending_items vec and local
        // indices (0..N). Its output (WaveItem queue_index values) will
        // therefore also be local indices, which is what run_encode_loop
        // expects when iterating over the compact vec.
        let local_indices: Vec<usize> = (0..pending_items.len()).collect();
        let wave_plan = {
            let staging_dir = staging::resolve_staging_dir(None);
            let mut mount_cache = remote::MountCache::new();
            staging::WavePlanner::plan(
                &pending_items,
                &local_indices,
                &mut mount_cache,
                &staging_dir,
                batch_settings.force_local,
                false, // remote_never: GUI always uses auto detection
            )
        };

        // Spawn the encoding loop in the background
        let state_for_task = state_arc.clone();
        let app_for_task = app.clone();
        let detected_encoders_for_task = {
            let ve = state_arc.detected_video_encoders.read().await;
            ve.clone()
        };

        // Wrap the sink so queue_item_updated / queue_item_probed calls
        // translate local (compact) indices back to original queue positions.
        let mapping_sink = Arc::new(IndexMappingSink {
            inner: sink.clone() as Arc<dyn EventSink>,
            index_map: index_map.clone(),
        });

        tokio::spawn(async move {
            let (done, failed, skipped, _was_cancelled) = encoder::run_encode_loop(
                mapping_sink.as_ref(),
                batch_ctrl.as_ref(),
                &mut pending_items,
                &batch_settings,
                &detected_encoders_for_task,
                Some(wave_plan),
                None, // GUI does not use disk monitoring
            )
            .await;

            // Sync statuses from the compact pending_items vec back to
            // the shared queue using the index map.
            {
                let mut q = state_for_task.queue.write().await;
                for (local_idx, item) in pending_items.iter().enumerate() {
                    if let Some(&orig_idx) = index_map.get(local_idx) {
                        if orig_idx < q.len() {
                            q[orig_idx].status = item.status.clone();
                        }
                    }
                }
            }
            let _ = app_for_task.emit("queue-sync-complete", ());

            // GUI-specific post-batch actions
            if show_toast {
                // Use the inner sink for toast/post-batch (no index translation needed)
                mapping_sink.toast(&format!(
                    "Done: {}  Failed: {}  Skipped: {}",
                    done, failed, skipped
                ));
            }

            if post_action != "None" {
                mapping_sink.post_batch(&post_action, post_countdown);
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
            let mut ve = state.detected_video_encoders.write().await;
            *ve = video;
        }
        {
            let mut ae = state.detected_audio_encoders.write().await;
            *ae = audio;
        }
        state.encoder_detection_done.store(true, Ordering::Release);
        state.ffmpeg_missing.store(false, Ordering::Release);
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

    #[cfg(feature = "downloader")]
    #[tauri::command]
    pub async fn download_mp4box(app: tauri::AppHandle) -> Result<(), String> {
        use tauri::Manager;
        let sink = tauri_sink::TauriSink::new(app.clone());
        dovi_tools::download_mp4box(&sink).await?;

        // Re-resolve MP4Box path now that it's installed
        let resource_dir = app.path().resource_dir().ok();
        dovi_tools::reinit(resource_dir.as_deref(), &sink);
        Ok(())
    }

    #[cfg(not(feature = "downloader"))]
    #[tauri::command]
    pub async fn download_mp4box(_app: tauri::AppHandle) -> Result<(), String> {
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
        queue: RwLock::new(Vec::new()),
        batch: Mutex::new(BatchState::default()),
        detected_video_encoders: RwLock::new(Vec::new()),
        detected_audio_encoders: RwLock::new(Vec::new()),
        config: RwLock::new(AppConfig::default()),
        encoder_detection_done: AtomicBool::new(false),
        ffmpeg_missing: AtomicBool::new(false),
        themes: RwLock::new(Vec::new()),
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
            preflight_check,
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
            download_mp4box,
            reveal_file,
            open_file,
        ])
        .setup(move |app| {
            let sink = tauri_sink::TauriSink::new(app.handle().clone());

            // Resolve ffmpeg/ffprobe binary paths (sidecar or PATH)
            let resource_dir = app.path().resource_dir().ok();
            ffmpeg::init(resource_dir.as_deref(), None, &sink);

            // Resolve DV tool paths (MP4Box)
            dovi_tools::init(resource_dir.as_deref(), &sink);

            // Load config
            let loaded_config = config::load_config(&app.handle());
            let themes_loaded = themes::scan_themes_folder(&app.handle());

            let state = app_state.clone();
            let app_handle = app.handle().clone();

            // Set config and themes synchronously via blocking
            tauri::async_runtime::block_on(async {
                {
                    let mut c = state.config.write().await;
                    *c = loaded_config;
                }
                {
                    let mut t = state.themes.write().await;
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
                    state_for_detect
                        .ffmpeg_missing
                        .store(true, Ordering::Relaxed);
                    let _ = handle_for_detect.emit("ffmpeg-missing", ());
                    detect_sink.log(
                        "[detect] ffmpeg not found - waiting for user to install or download it",
                    );
                    return;
                }

                let (video, audio) = encoder::detect_encoders(&detect_sink).await;
                {
                    let mut ve = state_for_detect.detected_video_encoders.write().await;
                    *ve = video;
                }
                {
                    let mut ae = state_for_detect.detected_audio_encoders.write().await;
                    *ae = audio;
                }
                state_for_detect
                    .encoder_detection_done
                    .store(true, Ordering::Release);
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
