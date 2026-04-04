use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
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
                if path.extension().is_some_and(|e| e == "json") {
                    if let Ok(contents) = fs::read_to_string(&path) {
                        match serde_json::from_str::<Theme>(&contents) {
                            Ok(theme) => themes.push(theme),
                            Err(e) => {
                                eprintln!("Malformed theme file {}: {e}", path.display());
                            }
                        }
                    }
                }
            }
        }
    }

    // Ensure we always have the built-in themes
    if !themes.iter().any(|t| t.name == "Default Dark") {
        themes.insert(0, default_dark_theme());
    }
    if !themes.iter().any(|t| t.name == "Default Light") {
        themes.push(default_light_theme());
    }
    if !themes.iter().any(|t| t.name == "Jessica Dark") {
        themes.push(jessica_dark_theme());
    }
    if !themes.iter().any(|t| t.name == "Solarised Dark") {
        themes.push(solarised_dark_theme());
    }
    if !themes.iter().any(|t| t.name == "Nord") {
        themes.push(nord_theme());
    }
    if !themes.iter().any(|t| t.name == "Vempire") {
        themes.push(vempire_theme());
    }

    // Ensure themes/ folder exists and write built-in themes if missing
    if !dir.exists() {
        let _ = fs::create_dir_all(&dir);
    }
    write_builtin_if_missing(&dir, "default-dark.json", &default_dark_theme());
    write_builtin_if_missing(&dir, "default-light.json", &default_light_theme());
    write_builtin_if_missing(&dir, "jessica-dark.json", &jessica_dark_theme());
    write_builtin_if_missing(&dir, "solarised-dark.json", &solarised_dark_theme());
    write_builtin_if_missing(&dir, "nord.json", &nord_theme());
    write_builtin_if_missing(&dir, "vempire.json", &vempire_theme());

    themes
}

fn write_builtin_if_missing(dir: &Path, filename: &str, theme: &Theme) {
    let path = dir.join(filename);
    if !path.exists() {
        if let Ok(json) = serde_json::to_string_pretty(theme) {
            let _ = fs::write(path, json);
        }
    }
}

/// Helper: build a Theme from the 6 core keys.
fn theme_from_core(
    name: &str,
    background: &str,
    surface: &str,
    text: &str,
    primary: &str,
    success: &str,
    error: &str,
) -> Theme {
    let mut colors = HashMap::new();
    colors.insert("background".into(), background.into());
    colors.insert("surface".into(), surface.into());
    colors.insert("text".into(), text.into());
    colors.insert("primary".into(), primary.into());
    colors.insert("success".into(), success.into());
    colors.insert("error".into(), error.into());
    Theme {
        name: name.into(),
        colors,
    }
}

fn default_dark_theme() -> Theme {
    theme_from_core(
        "Default Dark",
        "#1E1E1E", // background
        "#373737", // surface
        "#DCDCDC", // text
        "#0078D7", // primary
        "#4ade80", // success
        "#f87171", // error
    )
}

fn default_light_theme() -> Theme {
    theme_from_core(
        "Default Light",
        "#FFFFFF", // background
        "#E0E0E0", // surface
        "#1E1E1E", // text
        "#0078D7", // primary
        "#22c55e", // success
        "#ef4444", // error
    )
}

fn jessica_dark_theme() -> Theme {
    theme_from_core(
        "Jessica Dark",
        "#1E1E1E", // background
        "#373737", // surface
        "#DCDCDC", // text
        "#6F2D86", // primary
        "#4ade80", // success
        "#f87171", // error
    )
}

fn solarised_dark_theme() -> Theme {
    theme_from_core(
        "Solarised Dark",
        "#002B36", // background
        "#0A4050", // surface
        "#93A1A1", // text
        "#268BD2", // primary
        "#4ade80", // success
        "#f87171", // error
    )
}

fn nord_theme() -> Theme {
    theme_from_core(
        "Nord", "#2E3440", // background
        "#434C5E", // surface
        "#ECEFF4", // text
        "#88C0D0", // primary
        "#4ade80", // success
        "#f87171", // error
    )
}

fn vempire_theme() -> Theme {
    let mut t = theme_from_core(
        "Vempire", "#282A36", // background
        "#44475A", // surface
        "#F8F8F2", // text
        "#BD93F9", // primary
        "#4ade80", // success
        "#f87171", // error
    );
    // Vempire uses a distinct accent colour
    t.colors.insert("accent".into(), "#FF79C6".into());
    t
}
