//! Persisted app config: the list of saved SSH connections shown in the
//! right-dock "已保存的连接" panel. Plain Rust — **no `gpui_component`** here
//! (CLAUDE.md §1 boundary); the panel calls [`load`]/[`save`].
//!
//! Stored at `$XDG_CONFIG_HOME/caracal/connections.toml` (else
//! `~/.config/caracal/connections.toml`).
//!
//! ⚠️ SECURITY / TODO: `password` is persisted in **plaintext**, matching the
//! current Phase-4 plaintext-password reality (see `SshConfig`). This is a known
//! limitation — a later phase should move secrets to the OS keyring.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::terminal::ssh::SshConfig;

/// One saved connection entry.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedConnection {
    /// Display name (falls back to `user@host` if empty).
    #[serde(default)]
    pub name: String,
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    pub user: String,
    #[serde(default)]
    pub password: String,
}

fn default_port() -> u16 {
    22
}

impl SavedConnection {
    /// The connection parameters used to actually dial (see `workspace.rs`).
    pub fn to_ssh_config(&self) -> SshConfig {
        SshConfig {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            password: self.password.clone(),
        }
    }

    /// What to show as the row's primary label.
    pub fn display_name(&self) -> String {
        if self.name.trim().is_empty() {
            format!("{}@{}", self.user, self.host)
        } else {
            self.name.clone()
        }
    }

    /// `user@host:port`, the secondary/muted label.
    pub fn subtitle(&self) -> String {
        format!("{}@{}:{}", self.user, self.host, self.port)
    }
}

/// The whole persisted config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
}

/// `~/.config/caracal/connections.toml`.
pub fn config_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("caracal").join("connections.toml")
}

/// Load the config. Missing file → default (empty). A parse error is logged and
/// also yields the default, so a corrupt file never crashes startup.
pub fn load() -> AppConfig {
    let path = config_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return AppConfig::default(),
    };
    match toml::from_str(&text) {
        Ok(cfg) => cfg,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            AppConfig::default()
        }
    }
}

/// Persist the config, creating the parent directory if needed.
pub fn save(cfg: &AppConfig) -> anyhow::Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = toml::to_string_pretty(cfg)?;
    std::fs::write(&path, text)?;
    Ok(())
}
