//! Abstraction over the OS keyring (Keychain / Credential Manager / Secret
//! Service) for the vault's opt-in "remember on this device" convenience
//! unlock (see docs/superpowers/specs/2026-07-15-encrypted-credential-storage-design.md).
//! Plain Rust — no `gpui_component` here (CLAUDE.md §1 boundary).
//!
//! This is always a **local-only shortcut**: the password-derived
//! `[vault].wrapped_master_key` remains the canonical encryption regardless
//! of whether this store has anything cached. Any failure here (keyring
//! unavailable, entry missing, OS denied access) must be treated as a
//! cache miss, never an error that blocks unlocking via password.

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;

const SERVICE: &str = "caracal-vault";

/// Where the raw 32-byte master key is cached for convenience unlock,
/// namespaced by `vault_id` so it can't collide with anything else.
pub trait SecretStore {
    fn get(&self, vault_id: &str) -> Option<[u8; 32]>;
    fn set(&self, vault_id: &str, key: &[u8; 32]);
    fn clear(&self, vault_id: &str);
}

/// The real OS-backed store.
pub struct OsSecretStore;

impl SecretStore for OsSecretStore {
    fn get(&self, vault_id: &str) -> Option<[u8; 32]> {
        let entry = match keyring::Entry::new(SERVICE, vault_id) {
            Ok(e) => e,
            Err(e) => {
                log::warn!("OS keyring unavailable: could not create entry for convenience unlock: {e}");
                return None;
            }
        };
        // Stored (and read back) as base64 text via `get_password`/
        // `set_password`, not raw bytes via `get_secret`/`set_secret` — a
        // real 32-byte random key is essentially guaranteed to contain a
        // NUL byte or invalid-UTF-8 sequence somewhere, and at least one
        // Secret Service backend in the wild (KWallet's `ksecretd` compat
        // layer, confirmed 2026-07-15) doesn't round-trip that reliably
        // through the binary `SetSecret`/`GetSecret` D-Bus calls — it came
        // back truncated. Base64 is always plain ASCII, so it round-trips
        // safely through any backend regardless of whether it treats
        // secrets as opaque bytes or text.
        let encoded = match entry.get_password() {
            Ok(s) => s,
            Err(e) => {
                log::info!(
                    "no convenience-unlock key cached in the OS keyring for this vault (or read failed): {e}"
                );
                return None;
            }
        };
        let decoded = match B64.decode(&encoded) {
            Ok(b) => b,
            Err(e) => {
                log::warn!("convenience unlock: cached OS keyring value isn't valid base64, ignoring: {e}");
                return None;
            }
        };
        match decoded.try_into() {
            Ok(bytes) => {
                log::info!("convenience unlock: found a cached key in the OS keyring");
                Some(bytes)
            }
            Err(_) => {
                log::warn!("convenience unlock: cached OS keyring secret has the wrong length, ignoring");
                None
            }
        }
    }

    fn set(&self, vault_id: &str, key: &[u8; 32]) {
        let Ok(entry) = keyring::Entry::new(SERVICE, vault_id) else {
            log::warn!("OS keyring unavailable: could not create entry for convenience unlock");
            return;
        };
        match entry.set_password(&B64.encode(key)) {
            Ok(()) => log::info!("convenience unlock: stored the key in the OS keyring"),
            Err(e) => log::warn!("failed to store convenience-unlock key in the OS keyring: {e}"),
        }
    }

    fn clear(&self, vault_id: &str) {
        let Ok(entry) = keyring::Entry::new(SERVICE, vault_id) else {
            return;
        };
        // Already absent is not an error worth logging.
        let _ = entry.delete_credential();
    }
}

/// In-memory fake for tests — avoids touching the real OS keyring (real
/// backends are inconsistently available in CI, especially headless Linux
/// without a Secret Service D-Bus daemon running).
#[cfg(test)]
pub struct FakeSecretStore(std::sync::Mutex<std::collections::HashMap<String, [u8; 32]>>);

#[cfg(test)]
impl FakeSecretStore {
    pub fn new() -> Self {
        Self(std::sync::Mutex::new(std::collections::HashMap::new()))
    }
}

#[cfg(test)]
impl SecretStore for FakeSecretStore {
    fn get(&self, vault_id: &str) -> Option<[u8; 32]> {
        self.0.lock().unwrap().get(vault_id).copied()
    }

    fn set(&self, vault_id: &str, key: &[u8; 32]) {
        self.0.lock().unwrap().insert(vault_id.to_string(), *key);
    }

    fn clear(&self, vault_id: &str) {
        self.0.lock().unwrap().remove(vault_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fake_store_roundtrip() {
        let store = FakeSecretStore::new();
        assert_eq!(store.get("vault-a"), None);
        store.set("vault-a", &[5u8; 32]);
        assert_eq!(store.get("vault-a"), Some([5u8; 32]));
    }

    #[test]
    fn fake_store_clear_removes_entry() {
        let store = FakeSecretStore::new();
        store.set("vault-a", &[5u8; 32]);
        store.clear("vault-a");
        assert_eq!(store.get("vault-a"), None);
    }

    #[test]
    fn fake_store_namespaces_by_vault_id() {
        let store = FakeSecretStore::new();
        store.set("vault-a", &[1u8; 32]);
        store.set("vault-b", &[2u8; 32]);
        assert_eq!(store.get("vault-a"), Some([1u8; 32]));
        assert_eq!(store.get("vault-b"), Some([2u8; 32]));
    }
}
