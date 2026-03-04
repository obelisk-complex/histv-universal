use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use tauri::AppHandle;
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub name: String,
    pub colors: HashMap<String, String>,
}

/// Locate the themes/ folder adjacent to the application binary.
fn themes_dir(app: &AppHandle) -> PathBuf {
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            return dir.join("themes");
        }
    }
    app.path()
        .resource_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("themes")
}

/// Scan the themes/ folder and parse every .json file into a Theme.
/// Falls back to built-in defaults if the folder is empty or missing.
pub fn scan_themes_folder(app: &AppHandle) -> Vec<Theme> {
    let dir = themes_dir(app);
    let mut themes = Vec::new();

    if dir.exists() && dir.is_dir() {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().map_or(false, |e| e == "json") {
                    if let Ok(contents) = fs::read_to_string(&path) {
                        match serde_json::from_str::<Theme>(&contents) {
                            Ok(theme) => themes.push(theme),
                            Err(e) => {
                                eprintln!(
                                    "Malformed theme file {}: {e}",
                                    path.display()
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    // Ensure we always have at least the two built-in themes
    if !themes.iter().any(|t| t.name == "Default Dark") {
        themes.insert(0, default_dark_theme());
    }
    if !themes.iter().any(|t| t.name == "Default Light") {
        themes.push(default_light_theme());
    }

    // Ensure themes/ folder exists and write built-in themes if missing
    if !dir.exists() {
        let _ = fs::create_dir_all(&dir);
    }
    write_builtin_if_missing(&dir, "default-dark.json", &default_dark_theme());
    write_builtin_if_missing(&dir, "default-light.json", &default_light_theme());

    themes
}

fn write_builtin_if_missing(dir: &PathBuf, filename: &str, theme: &Theme) {
    let path = dir.join(filename);
    if !path.exists() {
        if let Ok(json) = serde_json::to_string_pretty(theme) {
            let _ = fs::write(path, json);
        }
    }
}

fn default_dark_theme() -> Theme {
    let mut colors = HashMap::new();
    colors.insert("primary".into(), "#0078D7".into());
    colors.insert("secondary".into(), "#6C757D".into());
    colors.insert("accent".into(), "#0078D7".into());
    colors.insert("neutral".into(), "#2A2A2A".into());
    colors.insert("base-100".into(), "#1E1E1E".into());
    colors.insert("base-200".into(), "#282828".into());
    colors.insert("base-300".into(), "#373737".into());
    colors.insert("base-content".into(), "#DCDCDC".into());
    colors.insert("info".into(), "#1E3C5A".into());
    colors.insert("success".into(), "#19411F".into());
    colors.insert("warning".into(), "#463C14".into());
    colors.insert("error".into(), "#501E1E".into());
    Theme {
        name: "Default Dark".into(),
        colors,
    }
}

fn default_light_theme() -> Theme {
    let mut colors = HashMap::new();
    colors.insert("primary".into(), "#0078D7".into());
    colors.insert("secondary".into(), "#6C757D".into());
    colors.insert("accent".into(), "#0078D7".into());
    colors.insert("neutral".into(), "#E0E0E0".into());
    colors.insert("base-100".into(), "#FFFFFF".into());
    colors.insert("base-200".into(), "#F5F5F5".into());
    colors.insert("base-300".into(), "#E0E0E0".into());
    colors.insert("base-content".into(), "#1E1E1E".into());
    colors.insert("info".into(), "#DBEAFE".into());
    colors.insert("success".into(), "#DCFCE7".into());
    colors.insert("warning".into(), "#FEF9C3".into());
    colors.insert("error".into(), "#FEE2E2".into());
    Theme {
        name: "Default Light".into(),
        colors,
    }
}
