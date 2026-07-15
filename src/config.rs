//! Persisted app config: the list of saved connections shown in the
//! right-dock "会话" panel. Plain Rust — **no `gpui_component`** here
//! (CLAUDE.md §1 boundary); the panel calls [`load`]/[`save`].
//!
//! Stored at `~/.caracal/connections.toml` (see `paths::app_dir`).
//!
//! ⚠️ SECURITY / TODO: `password` and `private_key_passphrase` are both
//! persisted in **plaintext**, matching the current Phase-4 plaintext-secret
//! reality (see `SshConfig`). This is a known limitation — a later phase
//! should move secrets to the OS keyring.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::panels::icons::AppIcon;
use crate::terminal::serial::SerialConfig;
use crate::terminal::ssh::{SshAuth, SshConfig};
use crate::terminal::telnet::TelnetConfig;

/// Connection type: SSH, local terminal, Telnet, or serial port.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionType {
    Ssh,
    Local,
    Telnet,
    Serial,
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
    /// Connection type (SSH, local terminal, Telnet, or serial port).
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
    /// Device path, e.g. "/dev/ttyUSB0" or "COM3". Serial only.
    #[serde(default)]
    pub serial_port: Option<String>,
    /// Serial only. Defaults to 115200 if unset.
    #[serde(default)]
    pub baud_rate: Option<u32>,
    /// Serial only: 5/6/7/8. Defaults to 8 if unset.
    #[serde(default)]
    pub data_bits: Option<u8>,
    /// Serial only: "none" | "odd" | "even". Defaults to "none" if unset.
    #[serde(default)]
    pub parity: Option<String>,
    /// Serial only: 1 | 2. Defaults to 1 if unset.
    #[serde(default)]
    pub stop_bits: Option<u8>,
    /// Serial only: "none" | "software" | "hardware". Defaults to "none" if unset.
    #[serde(default)]
    pub flow_control: Option<String>,
    /// Optional description shown in tooltip.
    #[serde(default)]
    pub description: Option<String>,
    /// `"password"` | `"key"`. Defaults to `"password"` for connections
    /// saved before this field existed.
    #[serde(default = "default_auth_method")]
    pub auth_method: String,
    /// Path to a private key file. Only meaningful when `auth_method ==
    /// "key"`.
    #[serde(default)]
    pub private_key_path: Option<String>,
    /// Optional passphrase to decrypt an encrypted private key. Only
    /// meaningful when `auth_method == "key"`.
    #[serde(default)]
    pub private_key_passphrase: Option<String>,
    /// `base64(nonce || ciphertext)` encryption of `password`, via
    /// `crypto::MasterKey::encrypt_str`. Additive alongside `password` for
    /// now (see `vault::migrate`) — Task 4 of the encrypted-credential-
    /// storage plan removes `password` once every connection has been
    /// migrated and every call site reads this field instead.
    #[serde(default)]
    pub encrypted_password: String,
    /// `base64(nonce || ciphertext)` encryption of `private_key_passphrase`.
    /// Additive alongside it for now — same migration note as
    /// `encrypted_password`.
    #[serde(default)]
    pub encrypted_key_passphrase: Option<String>,
    /// References an `AppConfig.ssh_keys` entry by id. Additive alongside
    /// `private_key_path` for now — `vault::migrate` populates this from
    /// the plaintext path; Task 4 removes `private_key_path` once
    /// `to_ssh_config` reads this instead.
    #[serde(default)]
    pub private_key_id: Option<String>,
    /// Manual ordering within a `group_id` scope (including `None`, the
    /// ungrouped section, which is its own scope). Lower sorts first.
    /// `SortMode::Default` reads this; drag-reorder writes it. New
    /// connections get the count of existing siblings in their scope
    /// (append-to-end), mirroring `SavedConnectionGroup.sort_order`'s
    /// `create_folder` convention.
    #[serde(default)]
    pub sort_order: i32,
}

fn default_auth_method() -> String {
    "password".to_string()
}

fn default_port() -> u16 {
    22
}

impl SavedConnection {
    /// The connection parameters used to actually dial (see `workspace.rs`).
    pub fn to_ssh_config(&self) -> SshConfig {
        let auth = if self.auth_method == "key" {
            SshAuth::PrivateKey {
                path: self.private_key_path.clone().unwrap_or_default(),
                passphrase: self.private_key_passphrase.clone(),
            }
        } else {
            SshAuth::Password(self.password.clone())
        };
        SshConfig {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            auth,
        }
    }

    /// Telnet connection parameters. No credentials: telnet login happens
    /// interactively in the terminal, same as typing at a raw `telnet` prompt.
    pub fn to_telnet_config(&self) -> TelnetConfig {
        TelnetConfig {
            host: self.host.clone(),
            port: self.port,
        }
    }

    /// Serial port parameters, applying the documented defaults (115200 8N1,
    /// no flow control) for any field that was never set.
    pub fn to_serial_config(&self) -> SerialConfig {
        SerialConfig {
            port: self.serial_port.clone().unwrap_or_default(),
            baud_rate: self.baud_rate.unwrap_or(115_200),
            data_bits: self.data_bits.unwrap_or(8),
            parity: self.parity.clone().unwrap_or_else(|| "none".to_string()),
            stop_bits: self.stop_bits.unwrap_or(1),
            flow_control: self
                .flow_control
                .clone()
                .unwrap_or_else(|| "none".to_string()),
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
                ConnectionType::Telnet => format!("{}:{}", self.host, self.port),
                ConnectionType::Serial => {
                    if let Some(ref port) = self.serial_port {
                        port.split('/').last().unwrap_or(port).to_string()
                    } else {
                        "serial".to_string()
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
            ConnectionType::Telnet => format!("{}:{}", self.host, self.port),
            ConnectionType::Serial => {
                let port = self.serial_port.as_deref().unwrap_or("?");
                let baud = self.baud_rate.unwrap_or(115_200);
                format!("{port} @ {baud}bps")
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
                "server" | "harddrive" => return AppIcon::Sessions,
                "network" => return AppIcon::Network,
                "telnet" => return AppIcon::Telnet,
                "serial" | "cpu" => return AppIcon::SerialPort,
                _ => {}
            }
        }
        // Auto-resolve from connection type
        match self.conn_type {
            ConnectionType::Ssh => AppIcon::Terminal,
            ConnectionType::Local => AppIcon::LocalTerminal,
            ConnectionType::Telnet => AppIcon::Telnet,
            ConnectionType::Serial => AppIcon::SerialPort,
        }
    }

    /// Lines shown in the tooltip. Each line is (label, value).
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
            ConnectionType::Telnet => {
                lines.push(("Host".to_string(), self.host.clone()));
                lines.push(("Port".to_string(), self.port.to_string()));
            }
            ConnectionType::Serial => {
                if let Some(ref port) = self.serial_port {
                    lines.push(("Port".to_string(), port.clone()));
                }
                lines.push((
                    "Baud".to_string(),
                    self.baud_rate.unwrap_or(115_200).to_string(),
                ));
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

/// Vault metadata: how `[[connections]]`'s `encrypted_*` fields and
/// `[[ssh_keys]]`'s `encrypted_content` are protected. Absent (`None` on
/// `AppConfig.vault`) means the file predates encryption and needs
/// one-time migration (see `vault::migrate`).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VaultMeta {
    /// Namespaces this vault's OS-keyring convenience-unlock entry.
    pub vault_id: String,
    pub kdf: String,
    /// Base64-encoded random salt for `crypto::derive_wrapping_key`.
    pub salt: String,
    pub kdf_mem_kib: u32,
    pub kdf_time: u32,
    pub kdf_parallelism: u32,
    /// `base64(nonce || ciphertext)` — the master key, encrypted with the
    /// password-derived wrapping key. See `crypto::MasterKey::wrap`.
    pub wrapped_master_key: String,
}

/// A named, shared SSH private key. Connections reference one by `id`
/// instead of embedding their own copy, so reusing one physical key across
/// many servers doesn't duplicate it, and rotating a key updates every
/// connection that uses it.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SshKeyEntry {
    pub id: String,
    pub name: String,
    /// Where this key was originally read from. Informational only (e.g.
    /// a future "reload from disk" action) — never used to locate the key
    /// at connect time, since the path is meaningless after an export/
    /// import to another machine.
    #[serde(default)]
    pub source_path: Option<String>,
    /// `base64(nonce || ciphertext)` of the raw key file bytes.
    pub encrypted_content: String,
}

/// The whole persisted config.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct AppConfig {
    #[serde(default)]
    pub connections: Vec<SavedConnection>,
    #[serde(default)]
    pub groups: Vec<SavedConnectionGroup>,
    /// `None` until the vault is set up (see `vault::migrate`).
    #[serde(default)]
    pub vault: Option<VaultMeta>,
    #[serde(default)]
    pub ssh_keys: Vec<SshKeyEntry>,
}

/// `~/.caracal/connections.toml`.
pub fn config_path() -> PathBuf {
    crate::paths::app_dir().join("connections.toml")
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

/// A simple, sufficiently-unique-in-practice id: `id-<nanoseconds since
/// epoch>`. Shared by connections, groups, and (as of the encrypted vault)
/// `VaultMeta.vault_id` / `SshKeyEntry.id`.
pub fn generate_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    format!("id-{nanos}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base_connection(conn_type: ConnectionType) -> SavedConnection {
        SavedConnection {
            name: String::new(),
            host: "example.com".to_string(),
            port: 23,
            user: String::new(),
            password: String::new(),
            group_id: None,
            conn_type,
            icon: None,
            shell_path: None,
            working_dir: None,
            serial_port: None,
            baud_rate: None,
            data_bits: None,
            parity: None,
            stop_bits: None,
            flow_control: None,
            description: None,
            auth_method: "password".to_string(),
            private_key_path: None,
            private_key_passphrase: None,
            encrypted_password: String::new(),
            encrypted_key_passphrase: None,
            private_key_id: None,
            sort_order: 0,
        }
    }

    #[test]
    fn telnet_display_name_and_subtitle_are_host_port() {
        let conn = base_connection(ConnectionType::Telnet);
        assert_eq!(conn.display_name(), "example.com:23");
        assert_eq!(conn.subtitle(), "example.com:23");
    }

    #[test]
    fn telnet_to_telnet_config_carries_host_and_port_only() {
        let conn = base_connection(ConnectionType::Telnet);
        let cfg = conn.to_telnet_config();
        assert_eq!(cfg.host, "example.com");
        assert_eq!(cfg.port, 23);
    }

    #[test]
    fn serial_display_name_uses_last_path_component() {
        let mut conn = base_connection(ConnectionType::Serial);
        conn.serial_port = Some("/dev/ttyUSB0".to_string());
        assert_eq!(conn.display_name(), "ttyUSB0");
    }

    #[test]
    fn serial_display_name_falls_back_when_port_unset() {
        let conn = base_connection(ConnectionType::Serial);
        assert_eq!(conn.display_name(), "serial");
    }

    #[test]
    fn serial_subtitle_shows_port_and_baud() {
        let mut conn = base_connection(ConnectionType::Serial);
        conn.serial_port = Some("/dev/ttyUSB0".to_string());
        conn.baud_rate = Some(9600);
        assert_eq!(conn.subtitle(), "/dev/ttyUSB0 @ 9600bps");
    }

    #[test]
    fn to_serial_config_applies_documented_defaults() {
        let mut conn = base_connection(ConnectionType::Serial);
        conn.serial_port = Some("/dev/ttyUSB0".to_string());
        let cfg = conn.to_serial_config();
        assert_eq!(cfg.port, "/dev/ttyUSB0");
        assert_eq!(cfg.baud_rate, 115_200);
        assert_eq!(cfg.data_bits, 8);
        assert_eq!(cfg.parity, "none");
        assert_eq!(cfg.stop_bits, 1);
        assert_eq!(cfg.flow_control, "none");
    }

    #[test]
    fn to_serial_config_honors_explicit_values() {
        let mut conn = base_connection(ConnectionType::Serial);
        conn.serial_port = Some("/dev/ttyUSB0".to_string());
        conn.baud_rate = Some(9600);
        conn.data_bits = Some(7);
        conn.parity = Some("even".to_string());
        conn.stop_bits = Some(2);
        conn.flow_control = Some("hardware".to_string());
        let cfg = conn.to_serial_config();
        assert_eq!(cfg.baud_rate, 9600);
        assert_eq!(cfg.data_bits, 7);
        assert_eq!(cfg.parity, "even");
        assert_eq!(cfg.stop_bits, 2);
        assert_eq!(cfg.flow_control, "hardware");
    }

    #[test]
    fn resolve_icon_auto_resolves_new_connection_types() {
        assert_eq!(
            base_connection(ConnectionType::Telnet).resolve_icon(),
            AppIcon::Telnet
        );
        assert_eq!(
            base_connection(ConnectionType::Serial).resolve_icon(),
            AppIcon::SerialPort
        );
    }

    #[test]
    fn to_ssh_config_uses_password_auth_by_default() {
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.password = "hunter2".to_string();
        let cfg = conn.to_ssh_config();
        assert!(matches!(cfg.auth, crate::terminal::ssh::SshAuth::Password(p) if p == "hunter2"));
    }

    #[test]
    fn to_ssh_config_uses_private_key_auth_when_selected() {
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.auth_method = "key".to_string();
        conn.private_key_path = Some("/home/user/.ssh/id_ed25519".to_string());
        conn.private_key_passphrase = Some("secret".to_string());
        let cfg = conn.to_ssh_config();
        match cfg.auth {
            crate::terminal::ssh::SshAuth::PrivateKey { path, passphrase } => {
                assert_eq!(path, "/home/user/.ssh/id_ed25519");
                assert_eq!(passphrase.as_deref(), Some("secret"));
            }
            _ => panic!("expected PrivateKey auth"),
        }
    }

    #[test]
    fn old_connection_without_auth_fields_still_deserializes_as_password() {
        // Simulates a connections.toml written before this change.
        let toml_text = r#"
            [[connections]]
            host = "old.example.com"
            user = "root"
            password = "hunter2"
            conn_type = "ssh"
        "#;
        let cfg: AppConfig =
            toml::from_str(toml_text).expect("old-format connection must still parse");
        assert_eq!(cfg.connections[0].auth_method, "password");
        assert_eq!(cfg.connections[0].private_key_path, None);
    }

    #[test]
    fn old_config_without_new_fields_still_deserializes() {
        // Simulates a `connections.toml` written before this change: no
        // serial_port/baud_rate/etc keys at all.
        let toml_text = r#"
            [[connections]]
            host = "old.example.com"
            user = "root"
            conn_type = "ssh"
        "#;
        let cfg: AppConfig =
            toml::from_str(toml_text).expect("old-format config must still parse");
        assert_eq!(cfg.connections.len(), 1);
        assert_eq!(cfg.connections[0].serial_port, None);
        assert_eq!(cfg.connections[0].baud_rate, None);
    }

    #[test]
    fn old_config_without_sort_order_still_deserializes() {
        let toml_text = r#"
            [[connections]]
            host = "old.example.com"
            user = "root"
            conn_type = "ssh"
        "#;
        let cfg: AppConfig =
            toml::from_str(toml_text).expect("old-format config must still parse");
        assert_eq!(cfg.connections[0].sort_order, 0);
    }

    #[test]
    fn app_config_without_vault_section_still_deserializes() {
        // Simulates a `connections.toml` written before this change — no
        // [vault] table, no [[ssh_keys]] entries at all.
        let toml_text = r#"
            [[connections]]
            host = "old.example.com"
            user = "root"
            conn_type = "ssh"
        "#;
        let cfg: AppConfig = toml::from_str(toml_text).expect("must still parse");
        assert!(cfg.vault.is_none());
        assert!(cfg.ssh_keys.is_empty());
    }

    #[test]
    fn vault_meta_and_ssh_key_entry_roundtrip() {
        let cfg = AppConfig {
            connections: vec![],
            groups: vec![],
            vault: Some(VaultMeta {
                vault_id: "id-1".to_string(),
                kdf: "argon2id".to_string(),
                salt: "c2FsdA==".to_string(),
                kdf_mem_kib: 19_456,
                kdf_time: 2,
                kdf_parallelism: 1,
                wrapped_master_key: "d3JhcHBlZA==".to_string(),
            }),
            ssh_keys: vec![SshKeyEntry {
                id: "id-2".to_string(),
                name: "id_ed25519".to_string(),
                source_path: Some("/home/user/.ssh/id_ed25519".to_string()),
                encrypted_content: "Y29udGVudA==".to_string(),
            }],
        };
        let text = toml::to_string_pretty(&cfg).unwrap();
        let round_tripped: AppConfig = toml::from_str(&text).unwrap();
        assert_eq!(round_tripped.vault.unwrap().vault_id, "id-1");
        assert_eq!(round_tripped.ssh_keys[0].name, "id_ed25519");
    }
}
