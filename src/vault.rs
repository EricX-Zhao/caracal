//! Vault orchestration: one-time migration from plaintext, password-based
//! unlock, forgotten-password reset, and merging an imported vault into the
//! current one. Built on `crypto.rs`'s primitives; operates on
//! `config::AppConfig`. Plain Rust — no `gpui_component` here (CLAUDE.md §1
//! boundary).
//!
//! Not yet called from the running app — Task 4 wires this in. Until then
//! this module is exercised only by its own tests below.

use anyhow::{Result, anyhow};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

use crate::config::{AppConfig, SshKeyEntry, VaultMeta, generate_id};
use crate::crypto::{self, MasterKey};

/// One-time setup: generates a fresh master key, wraps it under a
/// password-derived key, encrypts every existing connection's plaintext
/// secret in place, and converts each key-file-auth connection's
/// `private_key_path` into a shared, encrypted `ssh_keys` entry (deduped by
/// content hash — two connections pointing at the same physical file get
/// one shared entry, not two). Returns the unwrapped master key so the
/// caller doesn't have to immediately re-prompt for the password it was
/// just given.
pub fn migrate(cfg: &mut AppConfig, password: &str) -> Result<MasterKey> {
    let salt = crypto::generate_salt();
    let wrapping_key = crypto::derive_wrapping_key(password, &salt)?;
    let master = MasterKey::generate();
    let wrapped_master_key = master.wrap(&wrapping_key);

    // path -> ssh_keys id, so two connections sharing one physical key file
    // become one shared entry instead of two.
    let mut path_to_key_id: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    for conn in &mut cfg.connections {
        conn.encrypted_password = master.encrypt_str(&conn.password);
        conn.password.clear();

        if conn.auth_method == "key" {
            if let Some(path) = conn.private_key_path.clone() {
                let key_id = match path_to_key_id.get(&path) {
                    Some(id) => id.clone(),
                    None => {
                        let content = std::fs::read(&path)
                            .map_err(|e| anyhow!("failed to read private key {path}: {e}"))?;
                        let id = generate_id();
                        cfg.ssh_keys.push(SshKeyEntry {
                            id: id.clone(),
                            name: std::path::Path::new(&path)
                                .file_name()
                                .map(|n| n.to_string_lossy().to_string())
                                .unwrap_or_else(|| path.clone()),
                            source_path: Some(path.clone()),
                            encrypted_content: master.encrypt_bytes(&content),
                        });
                        path_to_key_id.insert(path.clone(), id.clone());
                        id
                    }
                };
                conn.private_key_id = Some(key_id);
            }
            conn.private_key_path = None;
        }

        if let Some(passphrase) = conn.private_key_passphrase.take() {
            conn.encrypted_key_passphrase = Some(master.encrypt_str(&passphrase));
        }
    }

    cfg.vault = Some(VaultMeta {
        vault_id: generate_id(),
        kdf: "argon2id".to_string(),
        salt: B64.encode(salt),
        kdf_mem_kib: argon2::Params::DEFAULT_M_COST,
        kdf_time: argon2::Params::DEFAULT_T_COST,
        kdf_parallelism: argon2::Params::DEFAULT_P_COST,
        wrapped_master_key,
    });

    Ok(master)
}

/// Normal-startup unlock: derives the wrapping key from `password` and
/// `cfg.vault`'s salt, then unwraps the master key. Fails (wrong password)
/// if `cfg.vault` is `None` or the AEAD unwrap fails.
pub fn unlock(cfg: &AppConfig, password: &str) -> Result<MasterKey> {
    let vault = cfg.vault.as_ref().ok_or_else(|| anyhow!("vault is not set up"))?;
    let salt = B64
        .decode(&vault.salt)
        .map_err(|e| anyhow!("corrupt vault salt: {e}"))?;
    let wrapping_key = crypto::derive_wrapping_key(password, &salt)?;
    MasterKey::unwrap(&vault.wrapped_master_key, &wrapping_key)
}

/// Forgotten-password escape hatch: discards `vault` and `ssh_keys`
/// entirely, and clears every connection's secret fields (rather than
/// leaving them as dangling references to a now-gone vault/key). Connection
/// *metadata* (host/user/port/name/groups) is left untouched. The caller is
/// expected to immediately follow this with `migrate` once the user has
/// chosen a new master password.
pub fn reset(cfg: &mut AppConfig) {
    cfg.vault = None;
    cfg.ssh_keys.clear();
    for conn in &mut cfg.connections {
        conn.encrypted_password.clear();
        conn.encrypted_key_passphrase = None;
        conn.private_key_id = None;
    }
}

/// Merges `source`'s groups/connections/ssh_keys into `dest`, re-encrypting
/// every secret under `dest_key` (secrets are decrypted with `source_key`
/// first — the two vaults almost certainly have different master keys).
/// Connections and groups are appended, never overwritten (matches
/// nyaterm's import behavior). `ssh_keys` are deduped by content hash so
/// re-importing the same file twice doesn't create duplicate copies of the
/// same physical key.
pub fn import_merge(dest: &mut AppConfig, dest_key: &MasterKey, source: &AppConfig, source_key: &MasterKey) -> Result<()> {
    use std::collections::HashMap;

    // Re-encrypt every source ssh_key under dest_key, deduping by a hash of
    // its *decrypted* content against dest's existing (also decrypted)
    // keys.
    let mut dest_content_hash_to_id: HashMap<String, String> = HashMap::new();
    for key in &dest.ssh_keys {
        let content = dest_key.decrypt_bytes(&key.encrypted_content)?;
        dest_content_hash_to_id.insert(content_hash(&content), key.id.clone());
    }

    let mut source_id_to_dest_id: HashMap<String, String> = HashMap::new();
    for key in &source.ssh_keys {
        let content = source_key.decrypt_bytes(&key.encrypted_content)?;
        let hash = content_hash(&content);
        let dest_id = match dest_content_hash_to_id.get(&hash) {
            Some(existing) => existing.clone(),
            None => {
                let new_id = generate_id();
                dest.ssh_keys.push(SshKeyEntry {
                    id: new_id.clone(),
                    name: key.name.clone(),
                    source_path: key.source_path.clone(),
                    encrypted_content: dest_key.encrypt_bytes(&content),
                });
                dest_content_hash_to_id.insert(hash, new_id.clone());
                new_id
            }
        };
        source_id_to_dest_id.insert(key.id.clone(), dest_id);
    }

    dest.groups.extend(source.groups.iter().cloned());

    for conn in &source.connections {
        let mut conn = conn.clone();
        let password = source_key.decrypt_str(&conn.encrypted_password)?;
        conn.encrypted_password = dest_key.encrypt_str(&password);
        if let Some(ct) = &conn.encrypted_key_passphrase {
            let passphrase = source_key.decrypt_str(ct)?;
            conn.encrypted_key_passphrase = Some(dest_key.encrypt_str(&passphrase));
        }
        if let Some(source_key_id) = &conn.private_key_id {
            conn.private_key_id = source_id_to_dest_id.get(source_key_id).cloned();
        }
        dest.connections.push(conn);
    }

    Ok(())
}

fn content_hash(bytes: &[u8]) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ConnectionType, SavedConnection};

    fn base_connection(host: &str) -> SavedConnection {
        SavedConnection {
            name: String::new(),
            host: host.to_string(),
            port: 22,
            user: "root".to_string(),
            password: "hunter2".to_string(),
            encrypted_password: String::new(),
            group_id: None,
            conn_type: ConnectionType::Ssh,
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
            private_key_id: None,
            encrypted_key_passphrase: None,
            sort_order: 0,
        }
    }

    #[test]
    fn migrate_then_unlock_with_same_password_succeeds() {
        let mut cfg = AppConfig { connections: vec![base_connection("a.example.com")], ..Default::default() };
        migrate(&mut cfg, "correct horse battery staple").unwrap();
        let master = unlock(&cfg, "correct horse battery staple").unwrap();
        assert_eq!(master.decrypt_str(&cfg.connections[0].encrypted_password).unwrap(), "hunter2");
    }

    #[test]
    fn migrate_clears_the_plaintext_password_field() {
        let mut cfg = AppConfig { connections: vec![base_connection("a.example.com")], ..Default::default() };
        migrate(&mut cfg, "pw").unwrap();
        assert!(cfg.connections[0].password.is_empty());
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        assert!(!serialized.contains("hunter2"), "plaintext password must not survive migration");
    }

    #[test]
    fn unlock_with_wrong_password_fails() {
        let mut cfg = AppConfig { connections: vec![base_connection("a.example.com")], ..Default::default() };
        migrate(&mut cfg, "correct password").unwrap();
        assert!(unlock(&cfg, "wrong password").is_err());
    }

    #[test]
    fn unlock_before_migration_fails() {
        let cfg = AppConfig::default();
        assert!(unlock(&cfg, "anything").is_err());
    }

    #[test]
    fn reset_clears_secrets_but_keeps_connection_metadata() {
        let mut cfg = AppConfig { connections: vec![base_connection("a.example.com")], ..Default::default() };
        migrate(&mut cfg, "pw").unwrap();
        reset(&mut cfg);
        assert!(cfg.vault.is_none());
        assert!(cfg.ssh_keys.is_empty());
        assert!(cfg.connections[0].encrypted_password.is_empty());
        assert_eq!(cfg.connections[0].host, "a.example.com", "metadata must survive a reset");
    }

    #[test]
    fn import_merge_appends_connections_and_reencrypts_under_dest_key() {
        let mut dest = AppConfig::default();
        let dest_key = migrate(&mut dest, "dest-pw").unwrap();

        let mut source = AppConfig { connections: vec![base_connection("imported.example.com")], ..Default::default() };
        let source_key = migrate(&mut source, "source-pw").unwrap();

        import_merge(&mut dest, &dest_key, &source, &source_key).unwrap();

        assert_eq!(dest.connections.len(), 1);
        assert_eq!(dest.connections[0].host, "imported.example.com");
        assert_eq!(dest_key.decrypt_str(&dest.connections[0].encrypted_password).unwrap(), "hunter2");
    }

    #[test]
    fn import_merge_dedups_ssh_keys_by_content_on_repeated_import() {
        let mut dest = AppConfig::default();
        let dest_key = migrate(&mut dest, "dest-pw").unwrap();

        let mut source = AppConfig::default();
        let source_key = MasterKey::generate();
        source.ssh_keys.push(SshKeyEntry {
            id: "src-key-1".to_string(),
            name: "id_ed25519".to_string(),
            source_path: None,
            encrypted_content: source_key.encrypt_bytes(b"same-key-bytes"),
        });

        import_merge(&mut dest, &dest_key, &source, &source_key).unwrap();
        import_merge(&mut dest, &dest_key, &source, &source_key).unwrap();

        assert_eq!(dest.ssh_keys.len(), 1, "importing the same key twice must not duplicate it");
    }
}
