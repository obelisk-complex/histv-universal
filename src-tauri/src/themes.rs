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

    // Ensure we always have at least the three built-in themes
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

fn jessica_dark_theme() -> Theme {
    let mut colors = HashMap::new();
    colors.insert("primary".into(), "#6F2D86".into());
    colors.insert("secondary".into(), "#6C757D".into());
    colors.insert("accent".into(), "#6F2D86".into());
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
        name: "Jessica Dark".into(),
        colors,
    }
}

fn solarised_dark_theme() -> Theme {
    let mut colors = HashMap::new();
    colors.insert("primary".into(), "#268BD2".into());
    colors.insert("secondary".into(), "#586E75".into());
    colors.insert("accent".into(), "#2AA198".into());
    colors.insert("neutral".into(), "#073642".into());
    colors.insert("base-100".into(), "#002B36".into());
    colors.insert("base-200".into(), "#073642".into());
    colors.insert("base-300".into(), "#0A4050".into());
    colors.insert("base-content".into(), "#93A1A1".into());
    colors.insert("info".into(), "#0D3D56".into());
    colors.insert("success".into(), "#0D3D2A".into());
    colors.insert("warning".into(), "#3D3A0D".into());
    colors.insert("error".into(), "#4A1A1A".into());
    Theme {
        name: "Solarised Dark".into(),
        colors,
    }
}

fn nord_theme() -> Theme {
    let mut colors = HashMap::new();
    colors.insert("primary".into(), "#88C0D0".into());
    colors.insert("secondary".into(), "#616E88".into());
    colors.insert("accent".into(), "#81A1C1".into());
    colors.insert("neutral".into(), "#3B4252".into());
    colors.insert("base-100".into(), "#2E3440".into());
    colors.insert("base-200".into(), "#3B4252".into());
    colors.insert("base-300".into(), "#434C5E".into());
    colors.insert("base-content".into(), "#ECEFF4".into());
    colors.insert("info".into(), "#2E3D50".into());
    colors.insert("success".into(), "#2E4038".into());
    colors.insert("warning".into(), "#4A4530".into());
    colors.insert("error".into(), "#4A2E2E".into());
    Theme {
        name: "Nord".into(),
        colors,
    }
}

fn vempire_theme() -> Theme {
    let mut colors = HashMap::new();
    colors.insert("primary".into(), "#BD93F9".into());
    colors.insert("secondary".into(), "#6272A4".into());
    colors.insert("accent".into(), "#FF79C6".into());
    colors.insert("neutral".into(), "#44475A".into());
    colors.insert("base-100".into(), "#282A36".into());
    colors.insert("base-200".into(), "#2D2F3D".into());
    colors.insert("base-300".into(), "#44475A".into());
    colors.insert("base-content".into(), "#F8F8F2".into());
    colors.insert("info".into(), "#1A2744".into());
    colors.insert("success".into(), "#1A3A2A".into());
    colors.insert("warning".into(), "#3D3A1A".into());
    colors.insert("error".into(), "#4A1A2A".into());
    Theme {
        name: "Vempire".into(),
        colors,
    }
}
