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

    // Upgrade untouched built-in theme files before the scan reads them.
    upgrade_builtin_themes_if_unedited(&dir);

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
    if !themes.iter().any(|t| t.name == "Creditor") {
        themes.push(creditor_theme());
    }
    if !themes.iter().any(|t| t.name == "Monokai") {
        themes.push(monokai_theme());
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
    write_builtin_if_missing(&dir, "creditor.json", &creditor_theme());
    write_builtin_if_missing(&dir, "monokai.json", &monokai_theme());

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

/// A built-in theme file whose colours have changed between releases.
struct BuiltinUpgrade {
    file: &'static str,
    /// Exactly the keys and values shipped before the change. A file holding
    /// anything else (changed value, extra or missing key) is a user edit.
    previous: &'static [(&'static str, &'static str)],
    current: fn() -> Theme,
}

const BUILTIN_UPGRADES: [BuiltinUpgrade; 6] = [
    // Pre-Doublet Default Dark.
    BuiltinUpgrade {
        file: "default-dark.json",
        previous: &[
            ("background", "#1E1E1E"),
            ("surface", "#373737"),
            ("text", "#DCDCDC"),
            ("primary", "#0078D7"),
            ("success", "#4ade80"),
            ("error", "#f87171"),
        ],
        current: default_dark_theme,
    },
    // Dark themes before the saturation and depth tuning.
    BuiltinUpgrade {
        file: "jessica-dark.json",
        previous: &[
            ("background", "#1E1E1E"),
            ("surface", "#373737"),
            ("text", "#DCDCDC"),
            ("primary", "#6F2D86"),
            ("success", "#4ade80"),
            ("error", "#f87171"),
        ],
        current: jessica_dark_theme,
    },
    BuiltinUpgrade {
        file: "solarised-dark.json",
        previous: &[
            ("background", "#002B36"),
            ("surface", "#0A4050"),
            ("text", "#93A1A1"),
            ("primary", "#268BD2"),
            ("success", "#4ade80"),
            ("error", "#f87171"),
        ],
        current: solarised_dark_theme,
    },
    BuiltinUpgrade {
        file: "nord.json",
        previous: &[
            ("background", "#2E3440"),
            ("surface", "#434C5E"),
            ("text", "#ECEFF4"),
            ("primary", "#88C0D0"),
            ("success", "#4ade80"),
            ("error", "#f87171"),
        ],
        current: nord_theme,
    },
    BuiltinUpgrade {
        file: "vempire.json",
        previous: &[
            ("background", "#282A36"),
            ("surface", "#44475A"),
            ("text", "#F8F8F2"),
            ("primary", "#BD93F9"),
            ("success", "#4ade80"),
            ("error", "#f87171"),
            ("accent", "#FF79C6"),
        ],
        current: vempire_theme,
    },
    // Monokai before the deeper grounds.
    BuiltinUpgrade {
        file: "monokai.json",
        previous: &[
            ("background", "#272822"),
            ("surface", "#3E3D32"),
            ("text", "#F8F8F2"),
            ("primary", "#F92672"),
            ("success", "#A6E22E"),
            ("error", "#F92672"),
        ],
        current: monokai_theme,
    },
];

fn holds_previous_colours(theme: &Theme, previous: &[(&str, &str)]) -> bool {
    theme.colors.len() == previous.len()
        && previous.iter().all(|(key, value)| {
            theme
                .colors
                .get(*key)
                .is_some_and(|v| v.trim().eq_ignore_ascii_case(value))
        })
}

/// Create each built-in theme file that is missing, and replace one that still
/// holds exactly the previously shipped colours with the current built-in. Any
/// other content is a user edit and is left alone.
fn upgrade_builtin_themes_if_unedited(dir: &Path) {
    for upgrade in &BUILTIN_UPGRADES {
        let path = dir.join(upgrade.file);
        let current = (upgrade.current)();
        let Ok(contents) = fs::read_to_string(&path) else {
            if !path.exists() && dir.is_dir() {
                write_theme_file(&path, &current);
            }
            continue;
        };
        if let Ok(existing) = serde_json::from_str::<Theme>(&contents) {
            if holds_previous_colours(&existing, upgrade.previous) {
                write_theme_file(&path, &current);
            }
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
        "#101014", // background
        "#24242B", // surface
        "#DCDCDC", // text
        "#5B2EBC", // primary
        "#00ea79", // success
        "#ff525a", // error
    )
}

fn solarised_dark_theme() -> Theme {
    theme_from_core(
        "Solarised Dark",
        "#101014", // background
        "#24242B", // surface
        "#93A1A1", // text
        "#17B3A4", // primary
        "#00ea79", // success
        "#ff525a", // error
    )
}

fn nord_theme() -> Theme {
    theme_from_core(
        "Nord", "#1C222D", // background
        "#2E3747", // surface
        "#ECEFF4", // text
        "#76C3D8", // primary
        "#00ea79", // success
        "#ff525a", // error
    )
}

fn vempire_theme() -> Theme {
    let mut t = theme_from_core(
        "Vempire", "#101014", // background
        "#24242B", // surface
        "#F8F8F2", // text
        "#B57CFF", // primary
        "#00ea79", // success
        "#ff525a", // error
    );
    // Vempire uses a distinct accent colour
    t.colors.insert("accent".into(), "#FF79C6".into());
    t
}

fn creditor_theme() -> Theme {
    let mut t = theme_from_core(
        "Creditor", "#26070F", // background
        "#47101A", // surface
        "#F5E9CF", // text
        "#D9A21B", // primary
        "#4FD98A", // success
        "#E5483F", // error
    );
    // Neutral grey for the Pending/Encoding/Skipped/Probing badges, gold for Cancelled
    t.colors.insert("surface-bright".into(), "#342E30".into());
    t.colors.insert("icon-cancelled".into(), "#E3B02A".into());
    t
}

fn monokai_theme() -> Theme {
    theme_from_core(
        "Monokai", "#161710", // background
        "#2B2B1F", // surface
        "#F8F8F2", // text
        "#F92672", // primary
        "#A6E22E", // success
        "#F92672", // error
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT_DARK_FILE: &str = "default-dark.json";

    fn scratch_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("histv-themes-test-{}-{tag}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn read_theme(dir: &Path, file: &str) -> Theme {
        let text = fs::read_to_string(dir.join(file)).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn upgrade_for(file: &str) -> &'static BuiltinUpgrade {
        BUILTIN_UPGRADES.iter().find(|u| u.file == file).unwrap()
    }

    /// The theme as an earlier release shipped it.
    fn previous_theme(file: &str) -> Theme {
        let upgrade = upgrade_for(file);
        Theme {
            name: (upgrade.current)().name,
            colors: upgrade
                .previous
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    fn write_previous(dir: &Path, file: &str) {
        write_theme_file(&dir.join(file), &previous_theme(file));
    }

    #[test]
    fn old_content_file_is_upgraded() {
        let dir = scratch_dir("upgrade");
        write_previous(&dir, DEFAULT_DARK_FILE);
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(
            read_theme(&dir, DEFAULT_DARK_FILE).colors,
            default_dark_theme().colors
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn edited_file_is_preserved() {
        let dir = scratch_dir("edited");
        let mut edited = previous_theme(DEFAULT_DARK_FILE);
        edited.colors.insert("primary".into(), "#FF00AA".into());
        write_theme_file(&dir.join(DEFAULT_DARK_FILE), &edited);
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(read_theme(&dir, DEFAULT_DARK_FILE).colors, edited.colors);

        // An extra key is also an edit.
        let mut extra = previous_theme(DEFAULT_DARK_FILE);
        extra.colors.insert("accent".into(), "#FFFFFF".into());
        write_theme_file(&dir.join(DEFAULT_DARK_FILE), &extra);
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(read_theme(&dir, DEFAULT_DARK_FILE).colors, extra.colors);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_is_created() {
        let dir = scratch_dir("missing");
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(
            read_theme(&dir, DEFAULT_DARK_FILE).colors,
            default_dark_theme().colors
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn new_content_file_is_unchanged() {
        let dir = scratch_dir("current");
        let path = dir.join(DEFAULT_DARK_FILE);
        write_theme_file(&path, &default_dark_theme());
        let before = fs::read_to_string(&path).unwrap();
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn jessica_dark_old_is_upgraded_case_insensitively() {
        let dir = scratch_dir("jessica-old");
        let mut old = previous_theme("jessica-dark.json");
        old.colors.insert("primary".into(), "#6f2d86".into());
        write_theme_file(&dir.join("jessica-dark.json"), &old);
        upgrade_builtin_themes_if_unedited(&dir);
        let upgraded = read_theme(&dir, "jessica-dark.json");
        assert_eq!(upgraded.colors, jessica_dark_theme().colors);
        assert_eq!(upgraded.colors["primary"], "#5B2EBC");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn jessica_dark_edited_is_kept() {
        let dir = scratch_dir("jessica-edited");
        let mut edited = previous_theme("jessica-dark.json");
        edited.colors.insert("primary".into(), "#7A1FA2".into());
        write_theme_file(&dir.join("jessica-dark.json"), &edited);
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(read_theme(&dir, "jessica-dark.json").colors, edited.colors);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn vempire_old_with_accent_is_upgraded() {
        let dir = scratch_dir("vempire-old");
        write_previous(&dir, "vempire.json");
        upgrade_builtin_themes_if_unedited(&dir);
        let upgraded = read_theme(&dir, "vempire.json");
        assert_eq!(upgraded.colors, vempire_theme().colors);
        assert_eq!(upgraded.colors["accent"], "#FF79C6");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn vempire_with_extra_or_missing_key_is_kept() {
        let dir = scratch_dir("vempire-keys");
        let path = dir.join("vempire.json");

        let mut extra = previous_theme("vempire.json");
        extra.colors.insert("text-muted".into(), "#999999".into());
        write_theme_file(&path, &extra);
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(read_theme(&dir, "vempire.json").colors, extra.colors);

        let mut missing = previous_theme("vempire.json");
        missing.colors.remove("accent");
        write_theme_file(&path, &missing);
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(read_theme(&dir, "vempire.json").colors, missing.colors);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn every_builtin_upgrades_then_stays_put() {
        let dir = scratch_dir("all");
        for upgrade in &BUILTIN_UPGRADES {
            write_previous(&dir, upgrade.file);
        }
        upgrade_builtin_themes_if_unedited(&dir);
        for upgrade in &BUILTIN_UPGRADES {
            let current = (upgrade.current)();
            assert!(
                !holds_previous_colours(&current, upgrade.previous),
                "{} colours did not change",
                upgrade.file
            );
            assert_eq!(read_theme(&dir, upgrade.file).colors, current.colors);
        }
        let before = fs::read_to_string(dir.join("nord.json")).unwrap();
        upgrade_builtin_themes_if_unedited(&dir);
        assert_eq!(fs::read_to_string(dir.join("nord.json")).unwrap(), before);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn shipped_json_matches_builtin_fns() {
        let shipped = [
            (
                include_str!("../themes/default-dark.json"),
                default_dark_theme(),
            ),
            (
                include_str!("../themes/default-light.json"),
                default_light_theme(),
            ),
            (
                include_str!("../themes/jessica-dark.json"),
                jessica_dark_theme(),
            ),
            (
                include_str!("../themes/solarised-dark.json"),
                solarised_dark_theme(),
            ),
            (include_str!("../themes/nord.json"), nord_theme()),
            (include_str!("../themes/vempire.json"), vempire_theme()),
            (include_str!("../themes/creditor.json"), creditor_theme()),
            (include_str!("../themes/monokai.json"), monokai_theme()),
        ];
        for (json, builtin) in shipped {
            let file: Theme = serde_json::from_str(json).unwrap();
            assert_eq!(file.name, builtin.name);
            assert_eq!(file.colors, builtin.colors, "{}", builtin.name);
        }
    }
}
