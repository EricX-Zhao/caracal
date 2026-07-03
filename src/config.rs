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

use crate::panels::icons::AppIcon;
use crate::terminal::ssh::SshConfig;

/// Connection type: SSH or local terminal.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionType {
    Ssh,
    Local,
}

impl Default for ConnectionType {
    fn default() -> Self {
        ConnectionType::Ssh
    }
}

/// A group (folder) that contains connections or other groups.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SavedConnectionGroup {
    /// Unique identifier (UUID).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Parent group ID. `None` means this is a root-level group.
    #[serde(default)]
    pub parent_id: Option<String>,
    /// Sort order among siblings.
    #[serde(default)]
    pub sort_order: i32,
}

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
    /// Group this connection belongs to. `None` means ungrouped (root level).
    #[serde(default)]
    pub group_id: Option<String>,
    /// Connection type (SSH or local terminal).
    #[serde(default)]
    pub conn_type: ConnectionType,
    /// User-selected icon name. `None` means auto-resolve from `conn_type`.
    #[serde(default)]
    pub icon: Option<String>,
    /// Shell path for local terminal connections.
    #[serde(default)]
    pub shell_path: Option<String>,
    /// Working directory for local terminal connections.
    #[serde(default)]
    pub working_dir: Option<String>,
    /// Optional description shown in tooltip.
    #[serde(default)]
    pub description: Option<String>,
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
            match self.conn_type {
                ConnectionType::Ssh => format!("{}@{}", self.user, self.host),
                ConnectionType::Local => {
                    if let Some(ref shell) = self.shell_path {
                        shell.split('/').last().unwrap_or(shell).to_string()
                    } else {
                        "local".to_string()
                    }
                }
            }
        } else {
            self.name.clone()
        }
    }

    /// Secondary/muted label shown below the name.
    pub fn subtitle(&self) -> String {
        match self.conn_type {
            ConnectionType::Ssh => format!("{}@{}:{}", self.user, self.host, self.port),
            ConnectionType::Local => {
                if let Some(ref wd) = self.working_dir {
                    wd.clone()
                } else if let Some(ref shell) = self.shell_path {
                    shell.clone()
                } else {
                    "local terminal".to_string()
                }
            }
        }
    }

    /// Resolve the icon for this connection.
    pub fn resolve_icon(&self) -> AppIcon {
        if let Some(ref icon_name) = self.icon {
            // Try to match user-specified icon name
            match icon_name.as_str() {
                "terminal" => return AppIcon::Terminal,
                "laptop" | "code" => return AppIcon::LocalTerminal,
                "server" | "harddrive" => return AppIcon::SavedConnections,
                "network" => return AppIcon::Network,
                _ => {}
            }
        }
        // Auto-resolve from connection type
        match self.conn_type {
            ConnectionType::Ssh => AppIcon::Terminal,
            ConnectionType::Local => AppIcon::LocalTerminal,
        }
    }

    /// Lines shown in the tooltip. Each line is (label, value).
    #[allow(dead_code)]
    pub fn tooltip_lines(&self) -> Vec<(String, String)> {
        let mut lines = Vec::new();
        match self.conn_type {
            ConnectionType::Ssh => {
                lines.push(("Host".to_string(), self.host.clone()));
                lines.push(("Port".to_string(), self.port.to_string()));
                lines.push(("User".to_string(), self.user.clone()));
            }
            ConnectionType::Local => {
                if let Some(ref shell) = self.shell_path {
                    lines.push(("Shell".to_string(), shell.clone()));
                }
                if let Some(ref wd) = self.working_dir {
                    lines.push(("Working Dir".to_string(), wd.clone()));
                }
            }
        }
        if let Some(ref desc) = self.description {
            if !desc.trim().is_empty() {
                lines.push(("Description".to_string(), desc.clone()));
            }
        }
        lines
    }
}

/// The whole persisted config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
    #[serde(default)]
    pub groups: Vec<SavedConnectionGroup>,
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
