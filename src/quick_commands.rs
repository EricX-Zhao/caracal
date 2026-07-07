//! Persisted quick commands: saved command snippets sent to the focused
//! terminal from the bottom quick-commands drawer. Plain Rust — no
//! `gpui_component` here (CLAUDE.md §1 boundary).
//!
//! Stored at `$XDG_CONFIG_HOME/caracal/quick_commands.toml` (else
//! `~/.config/caracal/quick_commands.toml`).
//!
//! ⚠️ SECURITY: `command` is persisted in **plaintext**, same caveat as
//! `config.rs`'s `SavedConnection::password` — a user-saved command that
//! embeds a token/password (e.g. a `curl -H "Authorization: Bearer …"`) is
//! stored unencrypted on disk. A later phase should move secrets to the OS
//! keyring, same as the connections-password TODO.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// How a quick command reaches the terminal: sent + Enter, or just placed on
/// the input line for the user to review/edit first.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExecutionMode {
    Execute,
    Append,
}

impl Default for ExecutionMode {
    fn default() -> Self {
        ExecutionMode::Execute
    }
}

/// One saved quick command.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct QuickCommand {
    pub id: String,
    pub label: String,
    pub command: String,
    #[serde(default)]
    pub execution_mode: ExecutionMode,
}

/// The whole persisted quick-commands file.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct QuickCommandsFile {
    #[serde(default)]
    pub commands: Vec<QuickCommand>,
}

/// `~/.config/caracal/quick_commands.toml`.
pub fn quick_commands_path() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".to_string());
            PathBuf::from(home).join(".config")
        });
    base.join("caracal").join("quick_commands.toml")
}

/// Load quick commands. Missing file → empty. A parse error is logged and
/// also yields empty, so a corrupt file never crashes startup.
pub fn load() -> Vec<QuickCommand> {
    let path = quick_commands_path();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    match toml::from_str::<QuickCommandsFile>(&text) {
        Ok(file) => file.commands,
        Err(e) => {
            log::warn!("failed to parse {}: {e}", path.display());
            Vec::new()
        }
    }
}

/// Persist quick commands, creating the parent directory if needed.
pub fn save(commands: &[QuickCommand]) -> anyhow::Result<()> {
    let path = quick_commands_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let file = QuickCommandsFile {
        commands: commands.to_vec(),
    };
    let text = toml::to_string_pretty(&file)?;
    std::fs::write(&path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_execution_mode_is_execute() {
        let toml_text = r#"
            [[commands]]
            id = "id-1"
            label = "List files"
            command = "ls -la"
        "#;
        let file: QuickCommandsFile = toml::from_str(toml_text).expect("must parse");
        assert_eq!(file.commands.len(), 1);
        assert_eq!(file.commands[0].execution_mode, ExecutionMode::Execute);
    }

    #[test]
    fn round_trip_preserves_fields() {
        let commands = vec![QuickCommand {
            id: "id-1".to_string(),
            label: "Docker ps".to_string(),
            command: "docker ps".to_string(),
            execution_mode: ExecutionMode::Append,
        }];
        let file = QuickCommandsFile {
            commands: commands.clone(),
        };
        let text = toml::to_string_pretty(&file).expect("serialize");
        let parsed: QuickCommandsFile = toml::from_str(&text).expect("deserialize");
        assert_eq!(parsed.commands.len(), 1);
        assert_eq!(parsed.commands[0].id, "id-1");
        assert_eq!(parsed.commands[0].label, "Docker ps");
        assert_eq!(parsed.commands[0].command, "docker ps");
        assert_eq!(parsed.commands[0].execution_mode, ExecutionMode::Append);
    }

    #[test]
    fn empty_file_yields_empty_commands() {
        let file: QuickCommandsFile = toml::from_str("").expect("empty file must parse");
        assert!(file.commands.is_empty());
    }
}
