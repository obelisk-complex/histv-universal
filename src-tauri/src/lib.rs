mod config;
mod encoder;
mod ffmpeg;
mod probe;
mod queue;
mod themes;

use std::sync::Arc;
use tauri::Emitter;
use tokio::sync::Mutex;

pub use config::AppConfig;
pub use encoder::EncoderInfo;
pub use probe::ProbeResult;
pub use queue::{AddResult, BatchState, QueueItem, QueueItemStatus};
pub use themes::Theme;

/// Shared application state accessible from all Tauri commands.
pub struct AppState {
    pub queue: Mutex<Vec<QueueItem>>,
    pub batch: Mutex<BatchState>,
    pub detected_video_encoders: Mutex<Vec<EncoderInfo>>,
    pub detected_audio_encoders: Mutex<Vec<String>>,
    pub config: Mutex<AppConfig>,
    pub encoder_detection_done: Mutex<bool>,
    pub ffmpeg_missing: Mutex<bool>,
    pub themes: Mutex<Vec<Theme>>,
}

// ── Tauri commands ──────────────────────────────────────────────

#[tauri::command]
async fn get_themes(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<Theme>, String> {
    let t = state.themes.lock().await;
    Ok(t.clone())
}

#[tauri::command]
async fn load_themes(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<Vec<Theme>, String> {
    let loaded = themes::scan_themes_folder(&app);
    let mut t = state.themes.lock().await;
    *t = loaded.clone();
    Ok(loaded)
}

#[tauri::command]
async fn get_config(state: tauri::State<'_, Arc<AppState>>) -> Result<AppConfig, String> {
    let c = state.config.lock().await;
    Ok(c.clone())
}

#[tauri::command]
async fn save_config(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    config: AppConfig,
) -> Result<(), String> {
    let mut c = state.config.lock().await;
    *c = config.clone();
    config::save_config(&app, &config).map_err(|e| e.to_string())
}

#[tauri::command]
async fn get_encoder_detection_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let done = state.encoder_detection_done.lock().await;
    Ok(*done)
}

#[tauri::command]
async fn get_ffmpeg_missing_status(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<bool, String> {
    let missing = state.ffmpeg_missing.lock().await;
    Ok(*missing)
}

#[tauri::command]
async fn get_detected_encoders(
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(Vec<EncoderInfo>, Vec<String>), String> {
    let ve = state.detected_video_encoders.lock().await;
    let ae = state.detected_audio_encoders.lock().await;
    Ok((ve.clone(), ae.clone()))
}

#[tauri::command]
async fn add_files_to_queue(
    state: tauri::State<'_, Arc<AppState>>,
    paths: Vec<String>,
) -> Result<AddResult, String> {
    let mut q = state.queue.lock().await;
    let result = queue::add_paths_to_queue(&mut q, &paths);
    Ok(result)
}

#[tauri::command]
async fn remove_queue_items(
    state: tauri::State<'_, Arc<AppState>>,
    indices: Vec<usize>,
) -> Result<(), String> {
    let mut q = state.queue.lock().await;
    queue::remove_items(&mut q, &indices);
    Ok(())
}

#[tauri::command]
async fn clear_completed(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut q = state.queue.lock().await;
    q.retain(|item| item.status != QueueItemStatus::Done);
    Ok(())
}

#[tauri::command]
async fn clear_non_pending(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut q = state.queue.lock().await;
    queue::clear_non_pending(&mut q);
    Ok(())
}

#[tauri::command]
async fn clear_all_queue(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut q = state.queue.lock().await;
    q.clear();
    Ok(())
}

#[tauri::command]
async fn requeue_items(
    state: tauri::State<'_, Arc<AppState>>,
    indices: Vec<usize>,
) -> Result<(), String> {
    let mut q = state.queue.lock().await;
    queue::requeue_items(&mut q, &indices);
    Ok(())
}

#[tauri::command]
async fn requeue_all(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut q = state.queue.lock().await;
    queue::requeue_all(&mut q);
    Ok(())
}

#[tauri::command]
async fn move_queue_item(
    state: tauri::State<'_, Arc<AppState>>,
    from: usize,
    to: usize,
) -> Result<(), String> {
    let mut q = state.queue.lock().await;
    queue::move_item(&mut q, from, to);
    Ok(())
}

#[tauri::command]
async fn reveal_file(path: String) -> Result<(), String> {
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
        // Try xdg-open on the parent directory
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
async fn open_file(path: String) -> Result<(), String> {
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
async fn get_queue(state: tauri::State<'_, Arc<AppState>>) -> Result<Vec<QueueItem>, String> {
    let q = state.queue.lock().await;
    Ok(q.clone())
}

#[tauri::command]
async fn probe_file(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    index: usize,
) -> Result<ProbeResult, String> {
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
    let _ = app.emit("queue-item-updated", (index, "Probing"));

    let result = probe::probe_file(&file_path, &app).await;

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
                }
                Err(_) => {
                    q[index].status = QueueItemStatus::Failed;
                }
            }
        }
    }
    let _ = app.emit("queue-item-probed", index);

    result
}

#[tauri::command]
async fn start_batch(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
    settings: serde_json::Value,
) -> Result<(), String> {
    let state_arc = state.inner().clone();
    encoder::start_batch_encode(app, state_arc, settings).await
}

#[tauri::command]
async fn cancel_current(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut b = state.batch.lock().await;
    b.cancel_current = true;
    Ok(())
}

#[tauri::command]
async fn cancel_all(state: tauri::State<'_, Arc<AppState>>) -> Result<(), String> {
    let mut b = state.batch.lock().await;
    b.cancel_all = true;
    Ok(())
}

#[tauri::command]
async fn toggle_pause(state: tauri::State<'_, Arc<AppState>>) -> Result<bool, String> {
    let mut b = state.batch.lock().await;
    b.paused = !b.paused;
    Ok(b.paused)
}

#[tauri::command]
async fn is_batch_running(state: tauri::State<'_, Arc<AppState>>) -> Result<bool, String> {
    let b = state.batch.lock().await;
    Ok(b.running)
}

#[tauri::command]
async fn respond_overwrite(
    state: tauri::State<'_, Arc<AppState>>,
    response: String,
) -> Result<(), String> {
    let mut b = state.batch.lock().await;
    b.overwrite_response = Some(response);
    Ok(())
}

#[tauri::command]
async fn respond_fallback(
    state: tauri::State<'_, Arc<AppState>>,
    response: String,
) -> Result<(), String> {
    let mut b = state.batch.lock().await;
    b.fallback_response = Some(response);
    Ok(())
}

#[tauri::command]
async fn execute_post_batch_action(action: String) -> Result<(), String> {
    encoder::execute_post_action(&action).await
}

#[tauri::command]
async fn check_ffmpeg_available() -> Result<bool, String> {
    Ok(ffmpeg::is_available().await)
}

#[tauri::command]
fn get_ffmpeg_dir() -> Result<String, String> {
    ffmpeg::exe_dir()
        .map(|d| d.to_string_lossy().to_string())
        .ok_or_else(|| "Could not determine executable directory".to_string())
}

#[tauri::command]
async fn download_ffmpeg(
    app: tauri::AppHandle,
    state: tauri::State<'_, Arc<AppState>>,
) -> Result<(), String> {
    let target_dir = ffmpeg::app_data_bin_dir()
        .ok_or_else(|| "Could not determine app data directory".to_string())?;

    // Create the directory if it doesn't exist
    std::fs::create_dir_all(&target_dir)
        .map_err(|e| format!("Could not create directory {}: {e}", target_dir.display()))?;

    ffmpeg::download_to_dir(&target_dir, &app).await?;

    // Re-resolve binary paths now that ffmpeg is in app-data
    ffmpeg::reinit(&app.clone());

    // Re-run encoder detection now that ffmpeg is available
    let _ = app.emit("log", "[detect] ffmpeg downloaded, re-running encoder detection...");
    let (video, audio) = encoder::detect_encoders(&app).await;
    {
        let mut ve = state.detected_video_encoders.lock().await;
        *ve = video;
    }
    {
        let mut ae = state.detected_audio_encoders.lock().await;
        *ae = audio;
    }
    {
        let mut done = state.encoder_detection_done.lock().await;
        *done = true;
    }
    {
        let mut missing = state.ffmpeg_missing.lock().await;
        *missing = false;
    }
    let _ = app.emit("encoder-detection-done", ());
    Ok(())
}

// ── App entry ───────────────────────────────────────────────────

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let app_state = Arc::new(AppState {
        queue: Mutex::new(Vec::new()),
        batch: Mutex::new(BatchState::default()),
        detected_video_encoders: Mutex::new(Vec::new()),
        detected_audio_encoders: Mutex::new(Vec::new()),
        config: Mutex::new(AppConfig::default()),
        encoder_detection_done: Mutex::new(false),
        ffmpeg_missing: Mutex::new(false),
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
			// Resolve ffmpeg/ffprobe binary paths (sidecar or PATH)
            ffmpeg::init(&app.handle());
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
                // Check if ffmpeg is reachable first
                if !ffmpeg::is_available().await {
                    {
                        let mut missing = state_for_detect.ffmpeg_missing.lock().await;
                        *missing = true;
                    }
                    let _ = handle_for_detect.emit("ffmpeg-missing", ());
                    let _ = handle_for_detect.emit("log", "[detect] ffmpeg not found — waiting for user to install or download it");
                    return;
                }

                let (video, audio) =
                    encoder::detect_encoders(&handle_for_detect).await;
                {
                    let mut ve = state_for_detect.detected_video_encoders.lock().await;
                    *ve = video;
                }
                {
                    let mut ae = state_for_detect.detected_audio_encoders.lock().await;
                    *ae = audio;
                }
                {
                    let mut done = state_for_detect.encoder_detection_done.lock().await;
                    *done = true;
                }
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