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
    #[serde(default)]
    pub terminal: TerminalSettings,
}

/// UI-level settings, editable from Settings → Appearance. Affects the
/// application chrome (menus, panels, buttons), not the terminal content —
/// see [`TerminalSettings`] for the terminal's own font.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AppearanceSettings {
    /// `"dark"` | `"light"`.
    #[serde(default = "default_theme_mode")]
    pub theme_mode: String,
}

/// Terminal-content settings, editable from Settings → Terminal.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TerminalSettings {
    /// Empty string = bundled default (`terminal::view`'s `DEFAULT_FONT_FAMILY`).
    #[serde(default)]
    pub font_family: String,
    /// Raw point size; `TerminalView::set_font_size` takes `gpui::Pixels`, so
    /// callers convert via `px(settings.terminal.font_size)` — `Pixels`
    /// itself isn't (de)serializable, hence the raw `f32` here.
    #[serde(default = "default_font_size")]
    pub font_size: f32,
    /// Whether the 资源监控 (basic system stats) panel polls the remote
    /// host at all. Off by default — matches nyaterm's own "all off by
    /// default" convention for remote-monitoring panels.
    #[serde(default)]
    pub monitor_basic_enabled: bool,
    /// Poll interval in seconds. Read once when a `MonitorPanel` is
    /// created; changing this in Settings takes effect for panels created
    /// afterward (not a live-reload of already-open panels).
    #[serde(default = "default_monitor_interval_secs")]
    pub monitor_basic_interval_secs: u32,
    /// Scrollback capacity in lines, passed to alacritty's
    /// `Config::scrolling_history` when a new terminal tab is constructed
    /// (`terminal::model::new_term`). Read once per tab; changing this in
    /// Settings only affects tabs opened afterward, never an already-open
    /// tab's grid.
    #[serde(default = "default_scrollback_lines")]
    pub scrollback_lines: u32,
}

fn default_font_size() -> f32 {
    14.0
}

fn default_monitor_interval_secs() -> u32 {
    5
}

fn default_scrollback_lines() -> u32 {
    10_000
}

fn default_theme_mode() -> String {
    "dark".to_string()
}

impl Default for AppearanceSettings {
    fn default() -> Self {
        Self {
            theme_mode: default_theme_mode(),
        }
    }
}

impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            font_family: String::new(),
            font_size: default_font_size(),
            monitor_basic_enabled: false,
            monitor_basic_interval_secs: default_monitor_interval_secs(),
            scrollback_lines: default_scrollback_lines(),
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
        assert_eq!(settings.terminal.font_family, "");
        assert_eq!(settings.terminal.font_size, 14.0);
        assert_eq!(settings.terminal.scrollback_lines, 10_000);
        assert_eq!(settings.appearance.theme_mode, "dark");
    }

    #[test]
    fn round_trip_preserves_fields() {
        let settings = AppSettings {
            appearance: AppearanceSettings {
                theme_mode: "light".to_string(),
            },
            terminal: TerminalSettings {
                font_family: "Consolas".to_string(),
                font_size: 16.0,
                monitor_basic_enabled: true,
                monitor_basic_interval_secs: 10,
                scrollback_lines: 20_000,
            },
        };
        let text = toml::to_string_pretty(&settings).expect("serialize");
        let parsed: AppSettings = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.terminal.font_family, "Consolas");
        assert_eq!(parsed.terminal.font_size, 16.0);
        assert!(parsed.terminal.monitor_basic_enabled);
        assert_eq!(parsed.terminal.monitor_basic_interval_secs, 10);
        assert_eq!(parsed.terminal.scrollback_lines, 20_000);
        assert_eq!(parsed.appearance.theme_mode, "light");
    }

    #[test]
    fn partial_toml_still_deserializes_with_defaults() {
        // Simulates a settings.toml written before a future field is added:
        // empty [appearance]/[terminal] tables should still fill in every
        // default.
        let toml_text = "[appearance]\n[terminal]\n";
        let settings: AppSettings =
            toml::from_str(toml_text).expect("partial settings must still parse");
        assert_eq!(settings.terminal.font_family, "");
        assert_eq!(settings.terminal.font_size, 14.0);
        assert_eq!(settings.terminal.scrollback_lines, 10_000);
        assert_eq!(settings.appearance.theme_mode, "dark");
    }

    #[test]
    fn empty_file_yields_default_appearance() {
        let settings: AppSettings = toml::from_str("").expect("empty file must still parse");
        assert_eq!(settings.terminal.font_size, 14.0);
    }

    #[test]
    fn old_settings_file_without_terminal_table_still_deserializes() {
        // Simulates a settings.toml written by the pre-split version of this
        // module, where font lived under [appearance]. The old font keys are
        // simply dropped (not migrated) — this only proves the file doesn't
        // fail to parse and the new [terminal] section falls back to
        // defaults, matching `AppSettings`'s top-level `#[serde(default)]`.
        let toml_text = r#"
            [appearance]
            font_family = "Consolas"
            font_size = 16.0
            theme_mode = "light"
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert_eq!(settings.appearance.theme_mode, "light");
        assert_eq!(settings.terminal.font_family, "");
        assert_eq!(settings.terminal.font_size, 14.0);
    }

    #[test]
    fn old_settings_file_without_monitor_fields_still_deserializes() {
        // Simulates a settings.toml written before this round: a [terminal]
        // table with only the font keys, no monitor_basic_* keys at all.
        let toml_text = r#"
            [terminal]
            font_family = "Consolas"
            font_size = 16.0
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert!(!settings.terminal.monitor_basic_enabled);
        assert_eq!(settings.terminal.monitor_basic_interval_secs, 5);
    }

    #[test]
    fn old_settings_file_without_scrollback_lines_still_deserializes() {
        // Simulates a settings.toml written before this field existed: a
        // [terminal] table with font + monitor keys, no scrollback_lines key.
        let toml_text = r#"
            [terminal]
            font_family = "Consolas"
            font_size = 16.0
            monitor_basic_enabled = true
            monitor_basic_interval_secs = 10
        "#;
        let settings: AppSettings =
            toml::from_str(toml_text).expect("old-format settings must still parse");
        assert_eq!(settings.terminal.scrollback_lines, 10_000);
    }
}
