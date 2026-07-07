//! Persisted application-level settings (font, theme, and future preferences),
//! separate from `config.rs`'s connections/groups. Plain Rust — no
//! `gpui_component` here (CLAUDE.md §1 boundary).
//!
//! Stored at `$XDG_CONFIG_HOME/caracal/settings.toml` (else
//! `~/.config/caracal/settings.toml`).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The whole persisted settings file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub appearance: AppearanceSettings,
}

/// Font + theme settings, editable from Settings → Appearance.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// Empty string = bundled default (`terminal::view`'s `DEFAULT_FONT_FAMILY`).
    #[serde(default)]
    pub font_family: String,
    /// Raw point size; `TerminalView::set_font_size` takes `gpui::Pixels`, so
    /// callers convert via `px(settings.appearance.font_size)` — `Pixels`
    /// itself isn't (de)serializable, hence the raw `f32` here.
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// `"dark"` | `"light"`.
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
}

fn default_font_size() -> f32 {
    14.0
}

fn default_theme_mode() -> String {
    "dark".to_string()
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_font_size(),
            theme_mode: default_theme_mode(),
        }
    }
}

/// `~/.config/caracal/settings.toml`.
pub fn settings_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("caracal").join("settings.toml")
}

/// Load settings. Missing file → default. A parse error is logged and also
/// yields the default, so a corrupt file never crashes startup.
pub fn load() -> AppSettings {
    let path = settings_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return AppSettings::default(),
    };
    match toml::from_str(&text) {
        Ok(settings) => settings,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            AppSettings::default()
        }
    }
}

/// Persist settings, creating the parent directory if needed.
pub fn save(settings: &AppSettings) -> anyhow::Result<()> {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(settings)?;
    std::fs::write(&path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_settings_have_expected_values() {
        let settings = AppSettings::default();
        assert_eq!(settings.appearance.font_family, "");
        assert_eq!(settings.appearance.font_size, 14.0);
        assert_eq!(settings.appearance.theme_mode, "dark");
    }

    #[test]
    fn round_trip_preserves_fields() {
        let settings = AppSettings {
            appearance: AppearanceSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                theme_mode: "light".to_string(),
            },
        };
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.appearance.font_family, "Consolas");
        assert_eq!(parsed.appearance.font_size, 16.0);
        assert_eq!(parsed.appearance.theme_mode, "light");
    }

    #[test]
    fn partial_toml_still_deserializes_with_defaults() {
        // Simulates a settings.toml written before a future field is added:
        // an empty [appearance] table should still fill in every default.
        let toml_text = "[appearance]\n";
        let settings: AppSettings =
            toml::from_str(toml_text).expect("partial settings must still parse");
        assert_eq!(settings.appearance.font_family, "");
        assert_eq!(settings.appearance.font_size, 14.0);
        assert_eq!(settings.appearance.theme_mode, "dark");
    }

    #[test]
    fn empty_file_yields_default_appearance() {
        let settings: AppSettings = toml::from_str("").expect("empty file must still parse");
        assert_eq!(settings.appearance.font_size, 14.0);
    }
}
