use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

/// All persisted user settings (§13.2).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[serde(default)]
pub struct AppConfig {
    pub theme: String,
    pub output_folder: String,
    pub output_mode: String, // "folder" | "beside" | "replace"
    pub overwrite: bool,
    pub delete_source: bool,
    pub save_log: bool,
    pub show_toast: bool,
    pub post_action: String,
    pub post_countdown: u32,
    pub custom_command: String,
    pub video_codec: String, // "HEVC" | "H.264"
    pub target_bitrate: u32,
    pub qp_i: u32,
    pub qp_p: u32,
    pub crf: u32,
    pub rate_control_mode: String, // "QP" | "CRF"
    pub hdr: bool,
    pub audio_codec: String, // "AC3" | "EAC3" | "AAC" | "Copy"
    pub audio_bitrate_cap: u32,
    pub auto_clear_completed: bool,
    pub log_drawer_open: bool,
    pub peak_multiplier: f64,
    pub threads: u32,
    pub low_priority: bool,
    pub precision_mode: bool,
    #[serde(default)]
    pub compatibility_mode: bool,
    #[serde(default)]
    pub preserve_av1: bool,
    #[serde(default)]
    pub force_local: bool,
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            theme: "Default Dark".to_string(),
            output_folder: "output".to_string(),
            output_mode: "folder".to_string(),
            overwrite: false,
            delete_source: false,
            save_log: false,
            show_toast: false,
            post_action: "None".to_string(),
            post_countdown: 0,
            custom_command: String::new(),
            video_codec: "HEVC".to_string(),
            target_bitrate: 4,
            qp_i: 20,
            qp_p: 22,
            crf: 20,
            rate_control_mode: "QP".to_string(),
            hdr: false,
            audio_codec: "AC3".to_string(),
            audio_bitrate_cap: 640,
            auto_clear_completed: false,
            log_drawer_open: false,
            peak_multiplier: 1.5,
            threads: 0,
            low_priority: false,
            precision_mode: false,
            compatibility_mode: false,
            preserve_av1: false,
            force_local: false,
        }
    }
}

/// Get the directory where config.json lives.
///
/// Uses the platform-standard app data directory (via Tauri's path API),
/// falling back to the directory containing the executable if unavailable.
/// This avoids writing to read-only locations (AppImage mounts, /usr/bin)
/// and follows OS conventions (Linux: ~/.local/share, macOS: ~/Library,
/// Windows: AppData\Roaming).
fn config_dir(app: &AppHandle) -> PathBuf {
    // Prefer platform-standard app data directory
    if let Ok(dir) = app.path().app_data_dir() {
        let _ = fs::create_dir_all(&dir);
        return dir;
    }
    // Fallback: directory containing the executable
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.to_path_buf();
        }
    }
    PathBuf::from(".")
}

fn config_path(app: &AppHandle) -> PathBuf {
    config_dir(app).join("config.json")
}

pub fn load_config(app: &AppHandle) -> AppConfig {
    let path = config_path(app);

    // Migrate: if the new location is empty but the old exe-relative config
    // exists, copy it over so users don't lose settings on upgrade.
    if !path.exists() {
        if let Ok(exe) = std::env::current_exe() {
            if let Some(exe_dir) = exe.parent() {
                let legacy = exe_dir.join("config.json");
                if legacy.exists() && legacy != path {
                    eprintln!(
                        "Migrating config from {} to {}",
                        legacy.display(),
                        path.display()
                    );
                    let _ = fs::copy(&legacy, &path);
                }
            }
        }
    }

    if path.exists() {
        match fs::read_to_string(&path) {
            Ok(contents) => match serde_json::from_str::<AppConfig>(&contents) {
                Ok(cfg) => return cfg,
                Err(e) => {
                    eprintln!("Malformed config.json, using defaults: {e}");
                }
            },
            Err(e) => {
                eprintln!("Could not read config.json: {e}");
            }
        }
    }
    AppConfig::default()
}

pub fn save_config(app: &AppHandle, config: &AppConfig) -> Result<(), Box<dyn std::error::Error>> {
    let path = config_path(app);
    let json = serde_json::to_string_pretty(config)?;
    fs::write(path, json)?;
    Ok(())
}
