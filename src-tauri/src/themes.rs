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

    // Upgrade an untouched pre-Doublet Default Dark file before the scan reads it.
    upgrade_default_dark_if_unedited(&dir);

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
        write_theme_file(&path, theme);
    }
}

fn write_theme_file(path: &Path, theme: &Theme) {
    if let Ok(json) = serde_json::to_string_pretty(theme) {
        let _ = fs::write(path, json);
    }
}

const DEFAULT_DARK_FILE: &str = "default-dark.json";

/// The Default Dark colours shipped before the Doublet redesign.
const PREVIOUS_DEFAULT_DARK: [(&str, &str); 6] = [
    ("background", "#1E1E1E"),
    ("surface", "#373737"),
    ("text", "#DCDCDC"),
    ("primary", "#0078D7"),
    ("success", "#4ade80"),
    ("error", "#f87171"),
];

fn is_previous_default_dark(theme: &Theme) -> bool {
    theme.colors.len() == PREVIOUS_DEFAULT_DARK.len()
        && PREVIOUS_DEFAULT_DARK.iter().all(|(key, value)| {
            theme
                .colors
                .get(*key)
                .is_some_and(|v| v.trim().eq_ignore_ascii_case(value))
        })
}

/// Create default-dark.json if it is missing, and replace it with the current
/// built-in if it still holds exactly the previously shipped colours. Any other
/// content is a user edit and is left alone.
fn upgrade_default_dark_if_unedited(dir: &Path) {
    let path = dir.join(DEFAULT_DARK_FILE);
    let current = default_dark_theme();
    let Ok(contents) = fs::read_to_string(&path) else {
        if !path.exists() && dir.is_dir() {
            write_theme_file(&path, &current);
        }
        return;
    };
    if let Ok(existing) = serde_json::from_str::<Theme>(&contents) {
        if is_previous_default_dark(&existing) {
            write_theme_file(&path, &current);
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
        "#101014", // background
        "#24242B", // surface
        "#E6E6EA", // text
        "#0A6CFF", // primary
        "#22e07a", // success
        "#ff5a5a", // error
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

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "histv-themes-test-{}-{tag}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_theme(dir: &Path) -> Theme {
        let text = fs::read_to_string(dir.join(DEFAULT_DARK_FILE)).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn previous_theme() -> Theme {
        theme_from_core(
            "Default Dark",
            "#1E1E1E",
            "#373737",
            "#DCDCDC",
            "#0078D7",
            "#4ade80",
            "#f87171",
        )
    }

    #[test]
    fn old_content_file_is_upgraded() {
        let dir = scratch_dir("upgrade");
        write_theme_file(&dir.join(DEFAULT_DARK_FILE), &previous_theme());
        upgrade_default_dark_if_unedited(&dir);
        assert_eq!(read_theme(&dir).colors, default_dark_theme().colors);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn edited_file_is_preserved() {
        let dir = scratch_dir("edited");
        let mut edited = previous_theme();
        edited.colors.insert("primary".into(), "#FF00AA".into());
        write_theme_file(&dir.join(DEFAULT_DARK_FILE), &edited);
        upgrade_default_dark_if_unedited(&dir);
        assert_eq!(read_theme(&dir).colors, edited.colors);

        // An extra key is also an edit.
        let mut extra = previous_theme();
        extra.colors.insert("accent".into(), "#FFFFFF".into());
        write_theme_file(&dir.join(DEFAULT_DARK_FILE), &extra);
        upgrade_default_dark_if_unedited(&dir);
        assert_eq!(read_theme(&dir).colors, extra.colors);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_created() {
        let dir = scratch_dir("missing");
        upgrade_default_dark_if_unedited(&dir);
        assert_eq!(read_theme(&dir).colors, default_dark_theme().colors);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_content_file_is_unchanged() {
        let dir = scratch_dir("current");
        let path = dir.join(DEFAULT_DARK_FILE);
        write_theme_file(&path, &default_dark_theme());
        let before = fs::read_to_string(&path).unwrap();
        upgrade_default_dark_if_unedited(&dir);
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        let _ = fs::remove_dir_all(&dir);
    }
}
