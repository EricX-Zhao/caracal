# Encrypted Credential Storage Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Stop storing SSH passwords, key passphrases, and (new) key-file contents in plaintext in `~/.caracal/connections.toml` — encrypt them at rest behind a user-set master password, and make caracal's existing export/import connection flows work with the encrypted vault.

**Architecture:** AES-256-GCM encrypts every secret field with its own random nonce; a random master key (wrapped by an Argon2id-derived, password-based wrapping key) encrypts those fields; connection *metadata* (host/user/port/name/groups) stays plaintext in the same `connections.toml`. SSH private keys become a shared, named, encrypted `ssh_keys` store instead of a per-connection path, so one physical key reused across many servers isn't duplicated. Unlock happens once at app startup (optionally cached in the OS keyring as a convenience), not per-connection.

**Tech Stack:** Rust, `aes-gcm`, `argon2`, `zeroize`, `base64`, `keyring` (OS keyring convenience unlock only), existing `gpui`/`gpui-component`/`russh`/`toml`/`serde`.

**Reference:** `docs/superpowers/specs/2026-07-15-encrypted-credential-storage-design.md` (design spec — read this first for the *why* behind every decision below).

## Global Constraints

- **caracal is a single binary crate** (`src/main.rs` + `mod` declarations, no `src/lib.rs`) — the whole codebase is one compilation unit. Every task in this plan must leave `cargo build` and `cargo test` fully green; there is no way to land a change in one file while a dependent file elsewhere is still mid-refactor. This is why the data-model cutover (Task 4) is one larger task instead of several tiny ones — see that task's own note.
- **AES-256-GCM** (`aes-gcm` crate) for all field and master-key encryption. A fresh random 12-byte nonce is generated on **every** encryption call — never reuse a nonce for the same key, even across re-saves of the same field.
- **Argon2id** (`argon2` crate) derives the password-based wrapping key. Use `argon2::Params::DEFAULT` (`m_cost = 19456` KiB, `t_cost = 2`, `p_cost = 1` — this is the OWASP floor and is a documented constant in the crate, not a magic number this plan invents).
- **IDs** (`vault_id`, `ssh_keys[].id`) reuse caracal's existing `id-<nanos>` scheme (currently `SessionsPanel::generate_id` in `src/panels/sessions.rs`) — this plan moves it to `pub fn config::generate_id()` rather than adding a `uuid` dependency.
- **Plain Rust only** in `src/crypto.rs`, `src/vault.rs`, `src/keyring_store.rs` — no `gpui_component` (CLAUDE.md §1 boundary), matching the existing rule `src/config.rs` and `src/paths.rs` already follow.
- Unlock happens **once at startup**; no idle-lock/re-lock in this plan.
- A **forgotten master password is unrecoverable by design** — do not add any decrypt-without-password code path, "security question," or similar. The only escape hatch is the "Reset vault" action (Task 5), which discards secrets, not recovers them.
- Every new user-facing string goes through the existing `rust_i18n::t!("Key")` convention, with entries added to `locales/app.yml` (`zh-CN` + `en`, per the existing i18n design).

---

### Task 1: AES-256-GCM + Argon2id crypto primitives

**Files:**
- Modify: `Cargo.toml`
- Create: `src/crypto.rs`
- Modify: `src/main.rs:16-23` (add `mod crypto;` to the existing `mod` block)

**Interfaces:**
- Produces: `crypto::encrypt(key: &[u8; 32], plaintext: &[u8]) -> String`, `crypto::decrypt(key: &[u8; 32], b64: &str) -> anyhow::Result<Vec<u8>>`, `crypto::derive_wrapping_key(password: &str, salt: &[u8]) -> anyhow::Result<[u8; 32]>`, `crypto::generate_salt() -> [u8; 16]`, `crypto::SALT_LEN: usize`, `crypto::MasterKey` (newtype wrapping `zeroize::Zeroizing<[u8; 32]>`) with `MasterKey::generate() -> Self`, `.wrap(&self, wrapping_key: &[u8; 32]) -> String`, `MasterKey::unwrap(wrapped: &str, wrapping_key: &[u8; 32]) -> anyhow::Result<Self>`, `.encrypt_str(&self, plaintext: &str) -> String`, `.decrypt_str(&self, b64: &str) -> anyhow::Result<String>`, `.encrypt_bytes(&self, plaintext: &[u8]) -> String`, `.decrypt_bytes(&self, b64: &str) -> anyhow::Result<Vec<u8>>`. These are consumed by every later task.

- [ ] **Step 1: Add crypto dependencies to `Cargo.toml`**

Add after the existing `dirs = "6"` line (in the "Saved-connections/settings/quick-commands persistence" comment block, since these deps exist for the same reason):

```toml
# Encrypted connection-vault crypto (see docs/superpowers/specs/2026-07-15-encrypted-credential-storage-design.md).
aes-gcm = "0.10"
argon2 = "0.5"
zeroize = "1"
base64 = "0.22"
```

- [ ] **Step 2: Write `src/crypto.rs` with its full test module (failing/not-compiling first)**

```rust
//! Low-level AES-256-GCM + Argon2id primitives for the encrypted connection
//! vault (see docs/superpowers/specs/2026-07-15-encrypted-credential-storage-design.md).
//! Plain Rust — no `gpui_component` here (CLAUDE.md §1 boundary), same rule
//! `config.rs`/`paths.rs` already follow.

use aes_gcm::aead::rand_core::RngCore;
use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use anyhow::{Result, anyhow};
use argon2::{Algorithm, Argon2, Params, Version};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use zeroize::Zeroizing;

/// Length in bytes of the random salt persisted in `[vault].salt`.
pub const SALT_LEN: usize = 16;
const NONCE_LEN: usize = 12;

/// Encrypts `plaintext` with `key`, returning `base64(nonce || ciphertext)`.
/// A fresh random nonce is generated on every call — never reused, not even
/// for repeated encryption of the same plaintext.
pub fn encrypt(key: &[u8; 32], plaintext: &[u8]) -> String {
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .expect("AES-GCM encryption does not fail for in-memory buffers");
    let mut out = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ciphertext);
    B64.encode(out)
}

/// Decrypts a `base64(nonce || ciphertext)` string produced by [`encrypt`].
/// Fails if `b64` isn't valid base64, is shorter than one nonce, or the AEAD
/// authentication tag doesn't match `key` — a wrong key and corrupted data
/// are indistinguishable by design (that's what makes this double as a
/// "wrong password" check when unwrapping the master key).
pub fn decrypt(key: &[u8; 32], b64: &str) -> Result<Vec<u8>> {
    let raw = B64
        .decode(b64)
        .map_err(|e| anyhow!("invalid ciphertext encoding: {e}"))?;
    if raw.len() < NONCE_LEN {
        return Err(anyhow!("ciphertext too short"));
    }
    let (nonce_bytes, ciphertext) = raw.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    cipher
        .decrypt(Nonce::from_slice(nonce_bytes), ciphertext)
        .map_err(|_| anyhow!("decryption failed: wrong password or corrupted data"))
}

/// Derives a 32-byte wrapping key from a master password + salt via
/// Argon2id (OWASP-floor params: `Params::DEFAULT` = 19456 KiB memory, 2
/// iterations, 1 lane). Deterministic: the same password + salt always
/// yields the same key.
pub fn derive_wrapping_key(password: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let argon2 = Argon2::new(Algorithm::Argon2id, Version::V0x13, Params::DEFAULT);
    let mut out = [0u8; 32];
    argon2
        .hash_password_into(password.as_bytes(), salt, &mut out)
        .map_err(|e| anyhow!("Argon2id derivation failed: {e}"))?;
    Ok(out)
}

/// A random salt for [`derive_wrapping_key`], generated once per vault at
/// setup time and persisted in `[vault].salt`.
pub fn generate_salt() -> [u8; SALT_LEN] {
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    salt
}

/// The unlocked vault's master key: encrypts/decrypts every stored secret.
/// Zeroized on drop so it doesn't linger in freed memory.
pub struct MasterKey(pub Zeroizing<[u8; 32]>);

impl MasterKey {
    /// Generates a fresh random master key (setup/migration time).
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        Self(Zeroizing::new(bytes))
    }

    /// Wraps (encrypts) this master key under a password-derived wrapping
    /// key, for storage in `[vault].wrapped_master_key`.
    pub fn wrap(&self, wrapping_key: &[u8; 32]) -> String {
        encrypt(wrapping_key, &self.0[..])
    }

    /// Unwraps a master key previously produced by [`Self::wrap`]. Fails
    /// (wrong password) if `wrapping_key` doesn't match.
    pub fn unwrap(wrapped: &str, wrapping_key: &[u8; 32]) -> Result<Self> {
        let bytes = decrypt(wrapping_key, wrapped)?;
        let arr: [u8; 32] = bytes
            .try_into()
            .map_err(|_| anyhow!("unwrapped master key has the wrong length"))?;
        Ok(Self(Zeroizing::new(arr)))
    }

    /// Encrypts a plaintext string field (password, passphrase) with this
    /// master key.
    pub fn encrypt_str(&self, plaintext: &str) -> String {
        encrypt(&self.0, plaintext.as_bytes())
    }

    /// Decrypts a field previously produced by [`Self::encrypt_str`].
    pub fn decrypt_str(&self, b64: &str) -> Result<String> {
        let bytes = decrypt(&self.0, b64)?;
        String::from_utf8(bytes).map_err(|e| anyhow!("decrypted data is not valid UTF-8: {e}"))
    }

    /// Encrypts raw bytes (SSH private key file contents) with this master
    /// key.
    pub fn encrypt_bytes(&self, plaintext: &[u8]) -> String {
        encrypt(&self.0, plaintext)
    }

    /// Decrypts a field into raw bytes (SSH private key file contents).
    pub fn decrypt_bytes(&self, b64: &str) -> Result<Vec<u8>> {
        decrypt(&self.0, b64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypt_decrypt_roundtrip() {
        let key = [7u8; 32];
        let ct = encrypt(&key, b"hunter2");
        assert_eq!(decrypt(&key, &ct).unwrap(), b"hunter2");
    }

    #[test]
    fn decrypt_with_wrong_key_fails() {
        let key = [7u8; 32];
        let wrong = [9u8; 32];
        let ct = encrypt(&key, b"hunter2");
        assert!(decrypt(&wrong, &ct).is_err());
    }

    #[test]
    fn decrypt_rejects_garbage_input() {
        let key = [7u8; 32];
        assert!(decrypt(&key, "not-valid-base64!!!").is_err());
        assert!(decrypt(&key, "").is_err());
    }

    #[test]
    fn encrypt_nonce_is_random_each_call() {
        let key = [1u8; 32];
        let a = encrypt(&key, b"same-plaintext");
        let b = encrypt(&key, b"same-plaintext");
        assert_ne!(
            a, b,
            "reusing a nonce for the same key is a real AES-GCM vulnerability"
        );
    }

    #[test]
    fn derive_wrapping_key_is_deterministic_for_same_inputs() {
        let salt = generate_salt();
        let a = derive_wrapping_key("correct horse", &salt).unwrap();
        let b = derive_wrapping_key("correct horse", &salt).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn derive_wrapping_key_differs_for_different_passwords() {
        let salt = generate_salt();
        let a = derive_wrapping_key("password-one", &salt).unwrap();
        let b = derive_wrapping_key("password-two", &salt).unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn master_key_wrap_unwrap_roundtrip() {
        let wrapping_key = [3u8; 32];
        let master = MasterKey::generate();
        let wrapped = master.wrap(&wrapping_key);
        let unwrapped = MasterKey::unwrap(&wrapped, &wrapping_key).unwrap();
        assert_eq!(&*master.0, &*unwrapped.0);
    }

    #[test]
    fn master_key_unwrap_with_wrong_password_fails() {
        let wrapping_key = [3u8; 32];
        let wrong_key = [4u8; 32];
        let master = MasterKey::generate();
        let wrapped = master.wrap(&wrapping_key);
        assert!(MasterKey::unwrap(&wrapped, &wrong_key).is_err());
    }

    #[test]
    fn field_encrypt_decrypt_str_roundtrip() {
        let master = MasterKey::generate();
        let ct = master.encrypt_str("hunter2");
        assert_eq!(master.decrypt_str(&ct).unwrap(), "hunter2");
    }

    #[test]
    fn field_encrypt_decrypt_bytes_roundtrip() {
        let master = MasterKey::generate();
        let content = b"-----BEGIN OPENSSH PRIVATE KEY-----\n...";
        let ct = master.encrypt_bytes(content);
        assert_eq!(master.decrypt_bytes(&ct).unwrap(), content);
    }
}
```

- [ ] **Step 3: Register the module in `src/main.rs`**

`src/main.rs` currently has this `mod` block:

```rust
mod assets;
mod config;
mod panels;
mod paths;
mod quick_commands;
mod settings;
mod terminal;
mod workspace;
```

Change to:

```rust
mod assets;
mod config;
mod crypto;
mod panels;
mod paths;
mod quick_commands;
mod settings;
mod terminal;
mod workspace;
```

- [ ] **Step 4: Run the tests**

Run: `cargo test crypto::`
Expected: 11 tests pass. `cargo build` also succeeds — `crypto.rs`'s items aren't called from anywhere else yet, so expect (and ignore) `warning: function/struct is never constructed` dead-code warnings; that's expected until Task 4 wires this module in, not a real problem.

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/crypto.rs src/main.rs
git commit -m "feat: add AES-256-GCM + Argon2id crypto primitives for the connection vault"
```

---

### Task 2: OS keyring abstraction for the convenience-unlock feature

**Files:**
- Modify: `Cargo.toml`
- Create: `src/keyring_store.rs`
- Modify: `src/main.rs:16-24` (add `mod keyring_store;`)

**Interfaces:**
- Consumes: nothing from Task 1 directly (works with raw `[u8; 32]`, not `crypto::MasterKey`, so it has no dependency on `crypto.rs` — keeps this module trivially testable in isolation).
- Produces: `keyring_store::SecretStore` trait with `fn get(&self, vault_id: &str) -> Option<[u8; 32]>`, `fn set(&self, vault_id: &str, key: &[u8; 32])`, `fn clear(&self, vault_id: &str)`; `keyring_store::OsSecretStore` (real impl); `keyring_store::FakeSecretStore` (in-memory, `#[cfg(test)]`-independent so Task 4's UI code can also use it... — no, tests only, see note in Task 4). Consumed by Task 4's unlock flow.

- [ ] **Step 1: Add the `keyring` dependency**

Add to `Cargo.toml`, right after the crypto deps added in Task 1:

```toml
# OS keyring for the opt-in "remember on this device" convenience unlock
# (see the design spec's "Convenience unlock" decision). Feature flags match
# nyaterm's (reference implementation): native Keychain/Credential Manager
# on macOS/Windows, D-Bus Secret Service (sync client) on Linux.
keyring = { version = "3.6", features = ["apple-native", "windows-native", "sync-secret-service", "crypto-rust"] }
```

- [ ] **Step 2: Write `src/keyring_store.rs` with its test module**

```rust
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
        let entry = keyring::Entry::new(SERVICE, vault_id).ok()?;
        let secret = entry.get_secret().ok()?;
        secret.try_into().ok()
    }

    fn set(&self, vault_id: &str, key: &[u8; 32]) {
        let Ok(entry) = keyring::Entry::new(SERVICE, vault_id) else {
            log::warn!("OS keyring unavailable: could not create entry for convenience unlock");
            return;
        };
        if let Err(e) = entry.set_secret(key) {
            log::warn!("failed to store convenience-unlock key in the OS keyring: {e}");
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
```

- [ ] **Step 3: Register the module in `src/main.rs`**

Add `mod keyring_store;` to the same `mod` block as Task 1's Step 3 (alphabetical order, after `mod crypto;`).

- [ ] **Step 4: Run the tests**

Run: `cargo test keyring_store::`
Expected: 3 tests pass. `cargo build` succeeds with expected dead-code warnings on `OsSecretStore` (unused until Task 4).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml Cargo.lock src/keyring_store.rs src/main.rs
git commit -m "feat: add OS-keyring abstraction for opt-in convenience unlock"
```

---

### Task 3: Vault data model (additive) + orchestration logic

This task adds the vault's data types and all its pure business logic
(migrate/unlock/reset/merge), fully unit-tested — but does **not** wire it
into the running app yet, so `SavedConnection.password` etc. keep working
exactly as they do today. This keeps the task reviewable in isolation:
a reviewer can verify the crypto/merge logic is correct without also
having to reason about the UI. Task 4 does the cutover.

**Files:**
- Modify: `src/config.rs` (add types, do **not** remove/rename existing fields yet)
- Modify: `src/panels/sessions.rs:311-317` (`generate_id` → delegate to the new shared one)
- Create: `src/vault.rs`
- Modify: `src/main.rs` (add `mod vault;`)

**Interfaces:**
- Consumes: `crypto::MasterKey`, `crypto::{encrypt, decrypt, derive_wrapping_key, generate_salt, SALT_LEN}` (Task 1).
- Produces: `config::VaultMeta { vault_id, kdf, salt, kdf_mem_kib, kdf_time, kdf_parallelism, wrapped_master_key }`, `config::SshKeyEntry { id, name, source_path: Option<String>, encrypted_content }`, `config::AppConfig.vault: Option<VaultMeta>`, `config::AppConfig.ssh_keys: Vec<SshKeyEntry>`, `config::generate_id() -> String`. `vault::migrate(cfg: &mut AppConfig, password: &str) -> anyhow::Result<crypto::MasterKey>`, `vault::unlock(cfg: &AppConfig, password: &str) -> anyhow::Result<crypto::MasterKey>`, `vault::reset(cfg: &mut AppConfig)`, `vault::import_merge(dest: &mut AppConfig, dest_key: &crypto::MasterKey, source: &AppConfig, source_key: &crypto::MasterKey)`. Consumed by Task 4.

- [ ] **Step 1: Add `VaultMeta`/`SshKeyEntry` types and `AppConfig` fields to `config.rs`**

In `src/config.rs`, add these two new structs right before `pub struct AppConfig` (around what is currently line 285):

```rust
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
```

Then extend `AppConfig` (currently `pub struct AppConfig { connections, groups }`, around line 286-292):

```rust
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
```

- [ ] **Step 2: Add a shared `generate_id` to `config.rs`, delegate `sessions.rs`'s copy to it**

In `src/config.rs`, add near the bottom (before the `#[cfg(test)]` module):

```rust
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
```

In `src/panels/sessions.rs`, replace the existing private copy (lines 311-317):

```rust
    fn generate_id() -> String {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        format!("id-{}", nanos)
    }
```

with:

```rust
    fn generate_id() -> String {
        config::generate_id()
    }
```

(Leave the wrapper method in place rather than rewriting every call site —
`Self::generate_id()` is still called elsewhere in that file. If `config`
isn't already imported in `sessions.rs`, check its `use crate::config::...`
line and add `generate_id` to it — it almost certainly already imports
`AppConfig`/`SavedConnection` from there.)

- [ ] **Step 3: Run existing tests to confirm nothing broke**

Run: `cargo test`
Expected: all existing tests still pass (this step was purely additive — no
type was renamed or removed). `cargo build` succeeds; expect dead-code
warnings on `VaultMeta`/`SshKeyEntry`/the new `AppConfig` fields (nothing
reads them yet) — expected until Task 4.

- [ ] **Step 4: Add a serde round-trip test for the new types**

Add to `config.rs`'s existing `#[cfg(test)] mod tests` block:

```rust
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
```

Run: `cargo test config::`
Expected: pass.

- [ ] **Step 5: Commit the additive data model**

```bash
git add src/config.rs src/panels/sessions.rs
git commit -m "feat: add vault/ssh_keys data model to AppConfig (additive, not yet wired in)"
```

- [ ] **Step 6: Write `src/vault.rs`'s test module first (migrate, unlock, reset, import_merge)**

```rust
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
```

- [ ] **Step 7: Register the module and run the tests**

Add `mod vault;` to `src/main.rs`'s `mod` block (alphabetical, after `mod terminal;`... actually before `mod terminal;`/`mod workspace;` alphabetically it's `t < v < w`, so: `..., mod terminal;, mod vault;, mod workspace;`).

Run: `cargo test vault::`
Expected: all 7 tests pass.

Run: `cargo test`
Expected: the whole suite (crypto, keyring_store, config, vault, and every
pre-existing test) passes. `cargo build` succeeds; `vault.rs`'s public
functions are still unused by the running app (dead-code warnings expected
until Task 4).

- [ ] **Step 8: Commit**

```bash
git add src/vault.rs src/main.rs
git commit -m "feat: add vault orchestration (migrate/unlock/reset/import_merge), unit-tested, not yet wired in"
```

---

### Task 4: Cutover — wire encryption into the running app

**Why this is one large task, not several small ones:** caracal is a
single binary crate (see Global Constraints). The moment
`SavedConnection.password: String` becomes `encrypted_password: String`,
every call site that reads or writes it stops compiling — `to_ssh_config`,
`sessions.rs`'s open/duplicate/persist, `new_connection_window.rs`'s
save/prefill, and `workspace.rs`'s connect flow all have to change
together, or `cargo build` fails outright. Splitting this into "task A:
rename the struct fields" and "task B: fix the call sites" would mean task
A leaves the tree broken, which isn't a state a reviewer can meaningfully
approve. The steps below are still individually small and ordered so you
build one working thing at a time within the task.

**Files:**
- Modify: `src/config.rs` (remove old plaintext fields, add new ones, rewrite `to_ssh_config`)
- Modify: `src/terminal/ssh.rs` (`SshAuth::PrivateKey{path,..}` → `PrivateKeyContent{content,..}`)
- Modify: `src/panels/sessions.rs` (`SessionsEvent::Open` payload, `duplicate`, `persist`)
- Modify: `src/panels/new_connection_window.rs` (ssh_keys picker instead of a path field)
- Modify: `src/workspace.rs` (vault global, startup migration/unlock dialogs, connect-flow decrypt)
- Modify: `locales/app.yml` (new dialog strings)

**Interfaces:**
- Consumes: `crypto::MasterKey` (Task 1), `keyring_store::{SecretStore, OsSecretStore}` (Task 2), `vault::{migrate, unlock}`, `config::{VaultMeta, SshKeyEntry}` (Task 3).
- Produces: `struct VaultKey(pub crypto::MasterKey)` implementing `gpui::Global` (new, in `workspace.rs`) — every later task (5, 6) reads the unlocked master key via `cx.try_global::<VaultKey>()`.

- [ ] **Step 1: Finish the `SavedConnection`/`to_ssh_config` cutover in `config.rs`**

Replace these three fields on `SavedConnection` (currently, per Task 3's
Step 1 context, still `password: String`, `private_key_path:
Option<String>`, `private_key_passphrase: Option<String>` — plus the new
`encrypted_password`/`encrypted_key_passphrase`/`private_key_id` fields
Task 3 already added alongside them):

```rust
    #[serde(default)]
    pub password: String,
```

→ delete this field entirely (keep only `encrypted_password: String` —
drop its `#[serde(default)]` too, since every connection now always has
one once the vault exists):

```rust
    pub encrypted_password: String,
```

```rust
    #[serde(default)]
    pub private_key_path: Option<String>,
```

→ delete this field entirely (it's fully superseded by `private_key_id`,
already added in Task 3).

```rust
    #[serde(default)]
    pub private_key_passphrase: Option<String>,
```

→ delete this field entirely (superseded by `encrypted_key_passphrase`,
already added in Task 3).

Now rewrite `to_ssh_config` (currently infallible, reading the fields just
removed):

```rust
    pub fn to_ssh_config(&self, ssh_keys: &[SshKeyEntry], master_key: &crate::crypto::MasterKey) -> anyhow::Result<SshConfig> {
        let auth = if self.auth_method == "key" {
            let key_id = self
                .private_key_id
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("connection uses key auth but has no key selected"))?;
            let entry = ssh_keys
                .iter()
                .find(|k| k.id == key_id)
                .ok_or_else(|| anyhow::anyhow!("the SSH key this connection uses was not found"))?;
            let content = master_key.decrypt_bytes(&entry.encrypted_content)?;
            let passphrase = self
                .encrypted_key_passphrase
                .as_deref()
                .map(|ct| master_key.decrypt_str(ct))
                .transpose()?;
            SshAuth::PrivateKeyContent { content, passphrase }
        } else {
            let password = master_key.decrypt_str(&self.encrypted_password)?;
            SshAuth::Password(password)
        };
        Ok(SshConfig {
            host: self.host.clone(),
            port: self.port,
            user: self.user.clone(),
            auth,
        })
    }
```

Update the existing test fixtures in `config.rs`'s `#[cfg(test)] mod
tests`: `base_connection` currently builds a `SavedConnection` with
`password: String::new()`. Change every fixture/test in that module to use
`encrypted_password: String::new()` instead of `password`, drop
`private_key_path`, and keep `private_key_passphrase` /
`encrypted_key_passphrase` consistent with the new names. Concretely, in
`base_connection`:

```rust
            password: String::new(),
```
→
```rust
            encrypted_password: String::new(),
```

and delete the `private_key_path: None,` line, and rename
`private_key_passphrase: None,` → `encrypted_key_passphrase: None,`, adding
`private_key_id: None,`.

The two tests `to_ssh_config_uses_password_auth_by_default` and
`to_ssh_config_uses_private_key_auth_when_selected` need rewriting since
`to_ssh_config` is now fallible and needs a master key + ssh_keys slice:

```rust
    #[test]
    fn to_ssh_config_uses_password_auth_by_default() {
        let master = crate::crypto::MasterKey::generate();
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.encrypted_password = master.encrypt_str("hunter2");
        let cfg = conn.to_ssh_config(&[], &master).unwrap();
        assert!(matches!(cfg.auth, crate::terminal::ssh::SshAuth::Password(p) if p == "hunter2"));
    }

    #[test]
    fn to_ssh_config_uses_private_key_auth_when_selected() {
        let master = crate::crypto::MasterKey::generate();
        let ssh_keys = vec![SshKeyEntry {
            id: "key-1".to_string(),
            name: "id_ed25519".to_string(),
            source_path: None,
            encrypted_content: master.encrypt_bytes(b"key-bytes"),
        }];
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.auth_method = "key".to_string();
        conn.private_key_id = Some("key-1".to_string());
        conn.encrypted_key_passphrase = Some(master.encrypt_str("secret"));
        let cfg = conn.to_ssh_config(&ssh_keys, &master).unwrap();
        match cfg.auth {
            crate::terminal::ssh::SshAuth::PrivateKeyContent { content, passphrase } => {
                assert_eq!(content, b"key-bytes");
                assert_eq!(passphrase.as_deref(), Some("secret"));
            }
            _ => panic!("expected PrivateKeyContent auth"),
        }
    }
```

Delete `old_connection_without_auth_fields_still_deserializes_as_password`
and `old_config_without_new_fields_still_deserializes` if they reference
`password =` in raw TOML fixtures expecting the *old* schema to still be
the live one — those scenarios are now covered by Task 3's
`app_config_without_vault_section_still_deserializes` test (a pre-migration
file has `password = "..."` fields that the app no longer defines at all;
migration, not passthrough deserialization, is how that data moves
forward — see Step 6 below for where the actual migration read of the old
`password` field happens). Add one new test capturing the *new*
dangling-reference error path:

```rust
    #[test]
    fn to_ssh_config_errors_when_referenced_key_is_missing() {
        let master = crate::crypto::MasterKey::generate();
        let mut conn = base_connection(ConnectionType::Ssh);
        conn.auth_method = "key".to_string();
        conn.private_key_id = Some("does-not-exist".to_string());
        assert!(conn.to_ssh_config(&[], &master).is_err());
    }
```

- [ ] **Step 2: Update `terminal/ssh.rs`'s `SshAuth` to carry key content, not a path**

Replace (currently `src/terminal/ssh.rs:27-38`):

```rust
/// Authentication method for an SSH connection.
#[derive(Clone, Debug)]
pub enum SshAuth {
    Password(String),
    /// `path` is the private key file on disk; `passphrase` decrypts it if
    /// it's passphrase-protected — `russh::keys::load_secret_key` handles
    /// both encrypted and plain keys through the same call.
    PrivateKey {
        path: String,
        passphrase: Option<String>,
    },
}
```

with:

```rust
/// Authentication method for an SSH connection.
#[derive(Clone, Debug)]
pub enum SshAuth {
    Password(String),
    /// `content` is the private key's raw file bytes (decrypted from the
    /// vault by the caller — see `config::SavedConnection::to_ssh_config`);
    /// `passphrase` decrypts it if it's passphrase-protected.
    /// `russh::keys::decode_secret_key` handles both encrypted and plain
    /// keys through the same call, parsing from an in-memory string instead
    /// of a file path (the vault stores key *content*, not a path — see the
    /// design spec's "SSH key-file auth" decision).
    PrivateKeyContent {
        content: Vec<u8>,
        passphrase: Option<String>,
    },
}
```

Update the import at the top of the file (currently line 19):

```rust
use russh::keys::{PrivateKeyWithHashAlg, load_secret_key};
```
→
```rust
use russh::keys::{PrivateKeyWithHashAlg, decode_secret_key};
```

Update `connect_and_auth` (currently lines 468-476):

```rust
    let auth_result = match auth {
        SshAuth::Password(password) => session.authenticate_password(user, password).await?,
        SshAuth::PrivateKey { path, passphrase } => {
            let key = load_secret_key(&path, passphrase.as_deref())
                .map_err(|e| anyhow!("failed to load private key {path}: {e}"))?;
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            session.authenticate_publickey(user, key).await?
        }
    };
```
→
```rust
    let auth_result = match auth {
        SshAuth::Password(password) => session.authenticate_password(user, password).await?,
        SshAuth::PrivateKeyContent { content, passphrase } => {
            let key_str = String::from_utf8(content)
                .map_err(|e| anyhow!("private key content is not valid UTF-8: {e}"))?;
            let key = decode_secret_key(&key_str, passphrase.as_deref())
                .map_err(|e| anyhow!("failed to parse private key: {e}"))?;
            let key = PrivateKeyWithHashAlg::new(Arc::new(key), None);
            session.authenticate_publickey(user, key).await?
        }
    };
```

- [ ] **Step 3: Fix `sessions.rs`'s call sites**

`SessionsEvent::Open` currently carries a pre-built `SshConfig` (built by
`open_event`, which called the now-removed infallible `to_ssh_config()`).
Since decrypting needs the master key (not available inside
`SessionsPanel`), change the event to carry the `SavedConnection` itself
and let `Workspace` (which will hold the master key, Step 5) do the
fallible conversion.

Find the `SessionsEvent` enum (around line 156) and change its `Open`
variant:

```rust
    Open(SshConfig, String),
```
→
```rust
    Open(SavedConnection, String),
```

Update `open_event` (currently lines 178-188):

```rust
fn open_event(conn: &SavedConnection) -> SessionsEvent {
    match conn.conn_type {
        ConnectionType::Ssh => SessionsEvent::Open(conn.to_ssh_config(), conn.display_name()),
```
→
```rust
fn open_event(conn: &SavedConnection) -> SessionsEvent {
    match conn.conn_type {
        ConnectionType::Ssh => SessionsEvent::Open(conn.clone(), conn.display_name()),
```

(the rest of `open_event`'s match arms — Local/Telnet/Serial — are
unaffected, leave them as-is.)

Fix `duplicate` (currently lines 530-545), which clears credentials on a
duplicated connection — it now needs to write valid *ciphertext* (an
encrypted empty string), not an empty plaintext string, and must handle
the vault possibly not being unlocked (defensive, shouldn't normally
happen — see Step 5's note on `try_global`):

```rust
            new_conn.password = String::new();
            new_conn.private_key_passphrase = None;
```
→
```rust
            if let Some(vault) = cx.try_global::<crate::workspace::VaultKey>() {
                new_conn.encrypted_password = vault.0.encrypt_str("");
            }
            new_conn.encrypted_key_passphrase = None;
            new_conn.private_key_id = None;
```

(`duplicate`'s signature already takes `cx: &mut Context<Self>` — no
signature change needed. `private_key_id` is also cleared, matching the
existing behavior's intent — a duplicated connection shouldn't silently
share the original's key reference without the user re-selecting it,
consistent with how the passphrase was already being cleared.)

Fix `persist` (currently lines 836-845) to round-trip `vault`/`ssh_keys`
instead of dropping them — this requires `SessionsPanel` to hold them,
added in Step 5 below alongside the constructor change; for now just widen
the `AppConfig` literal (this step's edit references `self.vault`/
`self.ssh_keys`, which Step 5 adds as fields — apply Step 5's field
additions to the struct first, then this edit, in a single sitting since
they're in the same file and the compiler needs both to typecheck):

```rust
    fn persist(&self) {
        let cfg = AppConfig {
            connections: self.connections.clone(),
            groups: self.groups.clone(),
        };
        if let Err(e) = config::save(&cfg) {
            log::error!("failed to save connections: {e}");
        }
    }
```
→
```rust
    fn persist(&self) {
        let cfg = AppConfig {
            connections: self.connections.clone(),
            groups: self.groups.clone(),
            vault: self.vault.clone(),
            ssh_keys: self.ssh_keys.clone(),
        };
        if let Err(e) = config::save(&cfg) {
            log::error!("failed to save connections: {e}");
        }
    }
```

(`export_connections`/`import_connections` also construct `AppConfig`
literals and need the same treatment, plus the vault-aware behavior the
design spec calls for — that's Task 6, not this step, to keep this
already-large task from growing further. For now, add `vault:
self.vault.clone(), ssh_keys: self.ssh_keys.clone(),` to
`export_connections`'s literal too so it compiles; leave
`import_connections`'s plain `extend()` behavior as-is for this task — Task
6 replaces it with the password-prompt + `vault::import_merge` flow.)

- [ ] **Step 4: Add `ssh_keys`/`vault` state to `SessionsPanel` and thread it through the constructor**

Add two fields to the `SessionsPanel` struct (after `groups: Vec<SavedConnectionGroup>,`, currently line 221):

```rust
    vault: Option<crate::config::VaultMeta>,
    ssh_keys: Vec<crate::config::SshKeyEntry>,
```

Update `SessionsPanel::new`'s signature and body (currently lines 259-264
+ the `Self { ... }` construction a few lines below):

```rust
    pub fn new(
        connections: Vec<SavedConnection>,
        groups: Vec<SavedConnectionGroup>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
```
→
```rust
    pub fn new(
        connections: Vec<SavedConnection>,
        groups: Vec<SavedConnectionGroup>,
        vault: Option<crate::config::VaultMeta>,
        ssh_keys: Vec<crate::config::SshKeyEntry>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
```

and add `vault,` / `ssh_keys,` to the `Self { ... }` struct literal further
down (alongside the existing `connections,`/`groups,` fields).

Update its one call site in `workspace.rs` — covered by Step 5 below, since
that's also where the rest of the startup sequence changes.

Add a small accessor `SessionsPanel` already needs for Task 6/the picker
UI in Task 5's `new_connection_window.rs` work:

```rust
    pub(crate) fn ssh_keys(&self) -> &[crate::config::SshKeyEntry] {
        &self.ssh_keys
    }
```

- [ ] **Step 5: Add the `VaultKey` global and the startup migration/unlock flow to `workspace.rs`**

Add near the top of `workspace.rs` (after its existing `use` block):

```rust
/// The unlocked vault's master key, available anywhere via
/// `cx.global::<VaultKey>()` / `cx.try_global::<VaultKey>()` once unlock
/// succeeds — set once at startup (see `Workspace::new`), never re-locked
/// during the session. Mirrors the existing `Theme::global_mut(cx)` idiom
/// gpui-component itself uses for app-wide state.
pub struct VaultKey(pub crate::crypto::MasterKey);
impl gpui::Global for VaultKey {}
```

In `Workspace::new` (currently around line 180), replace:

```rust
        let cfg = config::load();
        let saved = cx.new(|cx| {
            SessionsPanel::new(cfg.connections, cfg.groups, window, cx)
        });
```

with:

```rust
        let mut cfg = config::load();
        let vault_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(rust_i18n::t!("Vault.password_placeholder"))
        });
        let vault_confirm_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(rust_i18n::t!("Vault.confirm_password_placeholder"))
        });
        let vault_error = cx.new(|_cx| SharedString::default());

        if cfg.vault.is_none() {
            // First launch after upgrade (or a fresh install): no [vault]
            // section yet. Block on setting a master password before the
            // app is usable — mandatory, not opt-in (see design spec).
            let error = vault_error.clone();
            let pw_input = vault_password_input.clone();
            let confirm_input = vault_confirm_input.clone();
            window.open_alert_dialog(cx, move |alert, _window, cx| {
                let error = error.clone();
                let pw_input = pw_input.clone();
                let confirm_input = confirm_input.clone();
                let body = gpui_component::v_flex()
                    .gap_2()
                    .child(rust_i18n::t!("Vault.setup_body"))
                    .child(Input::new(&pw_input))
                    .child(Input::new(&confirm_input))
                    .when(!error.read(cx).is_empty(), |el| {
                        el.child(div().text_color(gpui::red()).child(error.read(cx).clone()))
                    });
                alert
                    .title(rust_i18n::t!("Vault.setup_title"))
                    .description(body)
                    .confirm()
                    .on_ok(move |_, window, cx| {
                        let password = pw_input.read(cx).value().to_string();
                        let confirm = confirm_input.read(cx).value().to_string();
                        if password.is_empty() {
                            error.update(cx, |e, cx| { *e = rust_i18n::t!("Vault.error_empty").into(); cx.notify(); });
                            return false;
                        }
                        if password != confirm {
                            error.update(cx, |e, cx| { *e = rust_i18n::t!("Vault.error_mismatch").into(); cx.notify(); });
                            return false;
                        }
                        window.close_dialog(cx);
                        true
                    })
            });
            // `open_alert_dialog`'s `on_ok` runs on the next event-loop
            // turn, not synchronously here, so `cfg`/the master key aren't
            // available yet at this point in `Workspace::new`. The actual
            // `vault::migrate` call + `cx.set_global(VaultKey(...))` happens
            // in a `cx.spawn_in` task below that awaits the dialog's
            // result — see the unlock branch immediately after, which both
            // paths (migrate vs. normal unlock) funnel into.
        }
```

At this point, note the two-phase nature of `open_alert_dialog`: its
`on_ok` closure runs later (on confirm), not inline. `Workspace::new` is
synchronous and returns `Self` immediately, so the dialog's outcome
(the derived `MasterKey`) cannot be threaded into the `Self { ... }`
literal being built in the same function call. Structure it like this
instead — replace the whole block above with this corrected version that
actually compiles and matches gpui's async patterns:

```rust
        let mut cfg = config::load();
        let needs_setup = cfg.vault.is_none();
        let vault_password_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(rust_i18n::t!("Vault.password_placeholder"))
        });
        let vault_confirm_input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder(rust_i18n::t!("Vault.confirm_password_placeholder"))
        });
        let vault_error: Entity<SharedString> = cx.new(|_cx| SharedString::default());
```

and, once the rest of `Self { ... }` is constructed further down in the
same function (after the existing `saved`/`saved_sub`/panel setup — those
now take `cfg.vault.clone()`/`std::mem::take(&mut cfg.ssh_keys)` per Step
4's constructor change), add this at the very end of `Workspace::new`,
right before the final `Self { ... }` is returned — spawn the actual
unlock/migration prompt loop as a `cx.spawn_in` task that keeps
re-prompting on a wrong password and, on success, calls
`cx.set_global(VaultKey(master))`:

```rust
        let workspace_weak = cx.entity().downgrade();
        cx.spawn_in(window, async move |_this, cx| {
            loop {
                let password = if needs_setup {
                    prompt_for_new_master_password(&vault_password_input, &vault_confirm_input, &vault_error, cx).await
                } else {
                    prompt_for_unlock_password(&vault_password_input, &vault_error, cx).await
                };
                let Some(password) = password else { return };

                let outcome = workspace_weak.update(cx, |_ws, app_cx| {
                    let mut cfg = config::load();
                    let result = if needs_setup {
                        vault::migrate(&mut cfg, &password)
                    } else {
                        vault::unlock(&cfg, &password)
                    };
                    match result {
                        Ok(master) => {
                            if needs_setup {
                                let _ = config::save(&cfg);
                            }
                            app_cx.set_global(VaultKey(master));
                            true
                        }
                        Err(_) => false,
                    }
                });
                if matches!(outcome, Ok(true)) {
                    return;
                }
                vault_error.update(cx, |e, cx| {
                    *e = rust_i18n::t!("Vault.error_wrong_password").into();
                    cx.notify();
                }).ok();
            }
        })
        .detach();
```

This references two small async helpers — add them as private free
functions in `workspace.rs` (below `impl Workspace`):

```rust
async fn prompt_for_new_master_password(
    pw_input: &Entity<InputState>,
    confirm_input: &Entity<InputState>,
    error: &Entity<SharedString>,
    cx: &mut gpui::AsyncWindowContext,
) -> Option<String> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let pw_input = pw_input.clone();
    let confirm_input = confirm_input.clone();
    let error = error.clone();
    let _ = cx.update(|window, cx| {
        window.open_alert_dialog(cx, move |alert, _window, cx| {
            let pw_input = pw_input.clone();
            let confirm_input = confirm_input.clone();
            let error = error.clone();
            let tx = tx.clone();
            let body = v_flex()
                .gap_2()
                .child(div().child(rust_i18n::t!("Vault.setup_body")))
                .child(Input::new(&pw_input))
                .child(Input::new(&confirm_input))
                .when(!error.read(cx).is_empty(), |el| {
                    el.child(div().text_color(gpui::red()).child(error.read(cx).clone()))
                });
            alert
                .title(rust_i18n::t!("Vault.setup_title"))
                .description(body)
                .confirm()
                .on_ok(move |_, window, cx| {
                    let password = pw_input.read(cx).value().to_string();
                    let confirm = confirm_input.read(cx).value().to_string();
                    if password.is_empty() {
                        error.update(cx, |e, cx| { *e = rust_i18n::t!("Vault.error_empty").into(); cx.notify(); });
                        return false;
                    }
                    if password != confirm {
                        error.update(cx, |e, cx| { *e = rust_i18n::t!("Vault.error_mismatch").into(); cx.notify(); });
                        return false;
                    }
                    window.close_dialog(cx);
                    if let Some(tx) = tx.borrow_mut().take() {
                        let _ = tx.send(password);
                    }
                    true
                })
        });
    });
    rx.await.ok()
}

async fn prompt_for_unlock_password(
    pw_input: &Entity<InputState>,
    error: &Entity<SharedString>,
    cx: &mut gpui::AsyncWindowContext,
) -> Option<String> {
    let (tx, rx) = futures::channel::oneshot::channel();
    let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
    let pw_input = pw_input.clone();
    let error = error.clone();
    let _ = cx.update(|window, cx| {
        window.open_alert_dialog(cx, move |alert, _window, cx| {
            let pw_input = pw_input.clone();
            let error = error.clone();
            let tx = tx.clone();
            let body = v_flex()
                .gap_2()
                .child(Input::new(&pw_input))
                .when(!error.read(cx).is_empty(), |el| {
                    el.child(div().text_color(gpui::red()).child(error.read(cx).clone()))
                });
            alert
                .title(rust_i18n::t!("Vault.unlock_title"))
                .description(body)
                .confirm()
                .on_ok(move |_, window, cx| {
                    let password = pw_input.read(cx).value().to_string();
                    window.close_dialog(cx);
                    if let Some(tx) = tx.borrow_mut().take() {
                        let _ = tx.send(password);
                    }
                    true
                })
        });
    });
    rx.await.ok()
}
```

(`futures::channel::oneshot` — `futures` is already a transitive
dependency of `gpui`/`tokio`'s ecosystem; if `cargo build` reports it's not
directly resolvable, add `futures = "0.3"` to `Cargo.toml`'s dependencies
in this same step. This bridges the dialog's callback-based `on_ok` into
the `async fn`'s `.await`-based control flow the surrounding `cx.spawn_in`
loop needs, so a wrong password can re-show the dialog instead of the
whole `Workspace::new` function needing to be async itself, which gpui
doesn't support.)

Finally, fix the `SessionsEvent::Open` handler in the `cx.subscribe_in`
closure (currently around line 186-188):

```rust
                SessionsEvent::Open(config, name) => {
                    this.open_ssh(config.clone(), name.clone(), window, cx)
                }
```
→
```rust
                SessionsEvent::Open(conn, name) => {
                    let Some(vault) = cx.try_global::<VaultKey>() else {
                        window.push_notification(
                            (NotificationType::Error, rust_i18n::t!("Vault.locked_error")),
                            cx,
                        );
                        return;
                    };
                    let ssh_keys = this.ssh_keys_snapshot();
                    match conn.to_ssh_config(&ssh_keys, &vault.0) {
                        Ok(ssh_config) => this.open_ssh(ssh_config, name.clone(), window, cx),
                        Err(e) => {
                            window.push_notification(
                                (NotificationType::Error, rust_i18n::t!("Vault.decrypt_failed_error", error = e.to_string())),
                                cx,
                            );
                        }
                    }
                }
```

This references `this.ssh_keys_snapshot()` — since `Workspace` doesn't
itself own `ssh_keys` (that's `SessionsPanel`'s state, per Step 4), add a
tiny accessor on `Workspace` that reads through to the panel:

```rust
    fn ssh_keys_snapshot(&self, cx: &App) -> Vec<crate::config::SshKeyEntry> {
        self.saved_sessions_panel_handle().read(cx).ssh_keys().to_vec()
    }
```

(Name the accessor to match whatever field `Workspace` already uses to
hold its `Entity<SessionsPanel>` handle — grep `workspace.rs` for `saved:
Entity<SessionsPanel>` if `saved_sessions_panel_handle` isn't already the
right name; use the existing field directly, e.g. `self.saved.read(cx)`,
rather than inventing a new accessor method if the field is already
directly accessible within `Workspace`'s own methods.)

- [ ] **Step 6: `new_connection_window.rs` — replace the free-text key path field with an ssh_keys picker**

`NewConnectionWindow` currently has `private_key_path: Entity<InputState>`
(a free-text field, lines 56, 132-140 of the constructor, 237-241 of
`save()`, plus its render call around line 707). Replace the *data* the
struct holds:

```rust
    private_key_path: Entity<InputState>,
```
→
```rust
    /// `Some(id)` when an existing shared key was picked; `None` with
    /// `pending_new_key` set when the user is importing a new one.
    selected_key_id: Option<String>,
    /// Set by "Import new key file..."; consumed by `save()`, which turns
    /// it into a new `SshKeyEntry` encrypted under the current vault key.
    pending_new_key: Option<(String, Vec<u8>, String)>, // (name, content, source_path)
    ssh_keys: Vec<crate::config::SshKeyEntry>,
```

Add `ssh_keys: Vec<crate::config::SshKeyEntry>` as a new parameter to
`NewConnectionWindow::new` (alongside its existing `conn: Option<(usize,
SavedConnection)>` / `group_id` / `sort_order` params — pass
`panel.read(cx).ssh_keys().to_vec()` from `sessions.rs`'s
`open_new_connection_window`, which already holds a `panel:
WeakEntity<SessionsPanel>`... since it's constructing the window from
inside `SessionsPanel` itself at that call site, use `self.ssh_keys.clone()`
directly instead of going through the weak handle). In the constructor
body, replace the `private_key_path: cx.new(...)` initializer (lines
132-140) with:

```rust
            selected_key_id: conn.as_ref().and_then(|c| c.private_key_id.clone()),
            pending_new_key: None,
            ssh_keys,
```

Replace the render call site (around line 707,
`.child(div().flex_1().child(Input::new(&self.private_key_path)))` plus
its neighboring "browse" button at ~line 717) with a simple list + import
button, following the same `Button`-list pattern already used for
`auth_method`'s password/key toggle at lines 660-690:

```rust
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .children(self.ssh_keys.iter().map(|k| {
                                        let id = k.id.clone();
                                        let selected = self.selected_key_id.as_deref() == Some(id.as_str());
                                        Button::new(SharedString::from(format!("ssh-key-{id}")))
                                            .label(k.name.clone())
                                            .when(selected, |b| b.selected(true))
                                            .on_click(cx.listener(move |this, _, _window, cx| {
                                                this.selected_key_id = Some(id.clone());
                                                this.pending_new_key = None;
                                                cx.notify();
                                            }))
                                    }))
                                    .child(
                                        Button::new("import-new-ssh-key")
                                            .label(rust_i18n::t!("NewConnectionWindow.import_key_button"))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
                                                    files: true,
                                                    directories: false,
                                                    multiple: false,
                                                    prompt: None,
                                                });
                                                cx.spawn_in(window, async move |this, cx| {
                                                    let Ok(Ok(Some(paths))) = rx.await else { return };
                                                    let Some(path) = paths.into_iter().next() else { return };
                                                    let Ok(content) = std::fs::read(&path) else { return };
                                                    let name = path
                                                        .file_name()
                                                        .map(|n| n.to_string_lossy().to_string())
                                                        .unwrap_or_else(|| "key".to_string());
                                                    let source_path = path.to_string_lossy().to_string();
                                                    let _ = this.update(cx, |this, cx| {
                                                        this.selected_key_id = None;
                                                        this.pending_new_key = Some((name, content, source_path));
                                                        cx.notify();
                                                    });
                                                })
                                                .detach();
                                            })),
                                    ),
                            )
```

Update `save()` (currently lines 237-241) — this is inside the `auth_method
== "key"` branch that used to set `private_key_path`:

```rust
                    private_key_path: if self.auth_method == "key" {
                        Some(self.private_key_path.read(cx).value().trim().to_string())
                    } else {
                        None
                    },
```
→
```rust
                    private_key_id: if self.auth_method == "key" {
                        self.resolve_key_id(cx)
                    } else {
                        None
                    },
```

and change `password:`/`private_key_passphrase:` in the same `SavedConnection { ... }`
literal (lines 218-222, 242-247) to the new field names, encrypting via the
vault global instead of storing plaintext:

```rust
                    password: if self.auth_method == "key" {
                        String::new()
                    } else {
                        self.password.read(cx).value().to_string()
                    },
```
→
```rust
                    encrypted_password: {
                        let plaintext = if self.auth_method == "key" {
                            String::new()
                        } else {
                            self.password.read(cx).value().to_string()
                        };
                        match cx.try_global::<crate::workspace::VaultKey>() {
                            Some(vault) => vault.0.encrypt_str(&plaintext),
                            None => String::new(), // unreachable in practice — vault is unlocked before this window can open
                        }
                    },
```

```rust
                    private_key_passphrase: if self.auth_method == "key" {
                        let p = self.private_key_passphrase.read(cx).value().to_string();
                        if p.is_empty() { None } else { Some(p) }
                    } else {
                        None
                    },
```
→
```rust
                    encrypted_key_passphrase: if self.auth_method == "key" {
                        let p = self.private_key_passphrase.read(cx).value().to_string();
                        if p.is_empty() {
                            None
                        } else {
                            cx.try_global::<crate::workspace::VaultKey>().map(|v| v.0.encrypt_str(&p))
                        }
                    } else {
                        None
                    },
```

Add the small helper `resolve_key_id` referenced above, which turns
`pending_new_key` into a real, encrypted `SshKeyEntry` (via the vault
global) the first time it's needed, and reports it back to `SessionsPanel`
so it lands in `AppConfig.ssh_keys` on the next `persist()`:

```rust
    /// If a new key file was picked (`pending_new_key`), encrypts and
    /// returns its new id, telling `SessionsPanel` to add the entry.
    /// Otherwise returns whatever existing key was selected.
    fn resolve_key_id(&mut self, cx: &mut Context<Self>) -> Option<String> {
        if let Some((name, content, source_path)) = self.pending_new_key.take() {
            let vault = cx.try_global::<crate::workspace::VaultKey>()?;
            let entry = crate::config::SshKeyEntry {
                id: crate::config::generate_id(),
                name,
                source_path: Some(source_path),
                encrypted_content: vault.0.encrypt_bytes(&content),
            };
            let id = entry.id.clone();
            let _ = self.panel.update(cx, |panel, _cx| panel.add_ssh_key(entry));
            self.selected_key_id = Some(id.clone());
            Some(id)
        } else {
            self.selected_key_id.clone()
        }
    }
```

(`self.panel` here is whatever field name `NewConnectionWindow` already
uses for its `WeakEntity<SessionsPanel>` back-reference — check the struct
definition near its other fields and reuse that name.) Add the
corresponding method on `SessionsPanel` (near `upsert_connection`, Task
4's Step 4 area):

```rust
    /// Adds a newly-imported SSH key to the vault's shared key store.
    /// Persistence happens on the next `upsert_connection`/`persist()` call
    /// (the connection referencing this key is always saved in the same
    /// user action), not immediately here, to avoid a double-write.
    pub(crate) fn add_ssh_key(&mut self, entry: crate::config::SshKeyEntry) {
        self.ssh_keys.push(entry);
    }
```

- [ ] **Step 7: Add the new i18n strings**

Add to `locales/app.yml` (matching its existing nested `_version: 2` style, see `src/config.rs`'s neighboring `Sessions:`/`NewConnectionWindow:` top-level keys for the pattern to follow):

```yaml
Vault:
  setup_title:
    zh-CN: "设置主密码"
    en: "Set a master password"
  setup_body:
    zh-CN: "此密码用于加密已保存的连接密码和密钥。请妥善保管——一旦忘记将无法恢复。"
    en: "This password encrypts your saved connection passwords and keys. Keep it safe — it cannot be recovered if forgotten."
  password_placeholder:
    zh-CN: "主密码"
    en: "Master password"
  confirm_password_placeholder:
    zh-CN: "确认主密码"
    en: "Confirm master password"
  unlock_title:
    zh-CN: "解锁"
    en: "Unlock"
  error_empty:
    zh-CN: "密码不能为空"
    en: "Password cannot be empty"
  error_mismatch:
    zh-CN: "两次输入的密码不一致"
    en: "Passwords do not match"
  error_wrong_password:
    zh-CN: "密码错误，请重试"
    en: "Wrong password, please try again"
  locked_error:
    zh-CN: "未解锁，无法连接"
    en: "Vault is locked, cannot connect"
  decrypt_failed_error:
    zh-CN: "解密连接信息失败：%{error}"
    en: "Failed to decrypt connection: %{error}"
NewConnectionWindow:
  import_key_button:
    zh-CN: "导入密钥文件..."
    en: "Import key file..."
```

- [ ] **Step 8: Build, run the full test suite**

Run: `cargo build`
Expected: succeeds. Fix any remaining type errors surfaced by the compiler
at call sites this plan's prose didn't enumerate verbatim (the steps above
cover every known reference to the renamed fields as of this plan's
research, but re-run `cargo build` iteratively and address any stragglers
— e.g. any other tests in `config.rs` still referencing `.password`/
`.private_key_path`/`.private_key_passphrase` by name).

Run: `cargo test`
Expected: full suite passes.

- [ ] **Step 9: Manual verification (no automated UI tests exist in this codebase — see `config.rs`'s own test module for the established convention of testing only pure logic)**

1. `rm -f ~/.caracal/connections.toml` in a scratch/test environment (or
   point `HOME` at a scratch dir) so this exercises the fresh-install path,
   not real data.
2. `cargo run`. Create one SSH connection with password auth and one with
   key-file auth (using "Import key file..." against a real key on disk).
3. Restart the app (`cargo run` again). Confirm you're prompted to **set**
   a master password (first run) — set one, confirm both fields must
   match, confirm an empty password is rejected.
4. Restart again. Confirm you're now prompted to **unlock** (not set up
   again) — enter the wrong password once, confirm it shows an error and
   re-prompts rather than crashing; enter the correct one, confirm the app
   opens.
5. Open both saved connections (password and key-auth) — confirm both
   still connect successfully.
6. Inspect `~/.caracal/connections.toml` directly — confirm `password`,
   `private_key_path`, `private_key_passphrase` do not appear anywhere,
   and `encrypted_password`/`encrypted_key_passphrase`/`ssh_keys[].encrypted_content`
   contain base64 ciphertext, not plaintext.
7. Duplicate a connection — confirm the duplicate has no password/key
   pre-filled.

- [ ] **Step 10: Commit**

```bash
git add src/config.rs src/terminal/ssh.rs src/panels/sessions.rs src/panels/new_connection_window.rs src/workspace.rs locales/app.yml Cargo.toml Cargo.lock
git commit -m "feat: encrypt saved SSH passwords/keys at rest behind a master password"
```

---

### Task 5: Settings — "Forget saved unlock on this device" and "Reset vault"

**Files:**
- Modify: `src/settings.rs` (add a "Security" section, or extend an existing one — check the file's current section layout first and follow its pattern)
- Modify: `locales/app.yml`

**Interfaces:**
- Consumes: `keyring_store::{SecretStore, OsSecretStore}` (Task 2), `vault::reset` (Task 3), `workspace::VaultKey` (Task 4).

- [ ] **Step 1: Read `src/settings.rs` to find its existing section pattern**

Before writing code, open `src/settings.rs` and identify how an existing
settings section (e.g. Appearance or General) renders a labeled row with a
button, so the new actions match the file's established layout exactly
rather than introducing a new pattern. (This plan doesn't reproduce that
file's full render tree here since it wasn't part of this feature's
research — follow whatever section-row component the file already uses for
every other action button.)

- [ ] **Step 2: Add "Forget saved unlock on this device"**

Add a button whose click handler does:

```rust
                            .on_click(cx.listener(|_this, _, _window, cx| {
                                if let Some(vault) = cx.try_global::<crate::workspace::VaultKey>() {
                                    let cfg = crate::config::load();
                                    if let Some(meta) = &cfg.vault {
                                        crate::keyring_store::OsSecretStore.clear(&meta.vault_id);
                                    }
                                    let _ = vault; // vault stays unlocked for this session; only the OS-cached copy is removed
                                }
                            }))
```

(Only enable/show this button when `cx.try_global::<VaultKey>().is_some()`
— i.e. the vault is currently unlocked; there's nothing to forget before
that.)

- [ ] **Step 3: Add "Reset vault" with a double-confirmation dialog**

Follow the exact `window.open_alert_dialog` confirm-dialog pattern already
used by `src/panels/sftp.rs`'s `delete_selected` (title + description +
`.confirm()` + `on_ok`) — reuse that shape here, with a second nested
confirmation (open a second `open_alert_dialog` from within the first's
`on_ok`, closing the first via `window.close_dialog(cx)` first) since this
action is irreversible and destroys real secrets:

```rust
    fn reset_vault(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.open_alert_dialog(cx, move |alert, _window, _cx| {
            alert
                .title(rust_i18n::t!("Vault.reset_confirm_title"))
                .description(rust_i18n::t!("Vault.reset_confirm_body"))
                .confirm()
                .on_ok(move |_, window, cx| {
                    window.close_dialog(cx);
                    window.open_alert_dialog(cx, move |alert, _window, _cx| {
                        alert
                            .title(rust_i18n::t!("Vault.reset_confirm_title_2"))
                            .description(rust_i18n::t!("Vault.reset_confirm_body_2"))
                            .confirm()
                            .on_ok(move |_, window, cx| {
                                window.close_dialog(cx);
                                let mut cfg = crate::config::load();
                                if let Some(meta) = &cfg.vault {
                                    crate::keyring_store::OsSecretStore.clear(&meta.vault_id);
                                }
                                crate::vault::reset(&mut cfg);
                                let _ = crate::config::save(&cfg);
                                cx.remove_global::<crate::workspace::VaultKey>();
                                window.push_notification(
                                    (NotificationType::Warning, rust_i18n::t!("Vault.reset_done")),
                                    cx,
                                );
                                true
                            })
                    });
                    true
                })
        });
    }
```

(`cx.remove_global::<T>()` — confirm this exact method name exists on
`App`/`Context` at implementation time via `cx.` autocomplete or a grep of
the `gpui` checkout's `app.rs`; if it's named differently in the pinned
`gpui` revision, use whatever the equivalent "unset a global" call is. The
effect needed is: the app returns to a locked state and, since
`cfg.vault` is now `None`, the next natural place the "set a new master
password" prompt already fires — per Task 4 Step 5's `needs_setup` check —
is the *next app restart*. Add a note in this step's manual verification
below explicitly checking whether re-showing that same setup prompt
immediately, in-session, is needed instead of waiting for a restart; if so,
factor Task 4 Step 5's setup-prompt block into a small reusable function
and call it here too.)

- [ ] **Step 4: Add i18n strings**

```yaml
Vault:
  forget_unlock_button:
    zh-CN: "忘记本机的免密解锁"
    en: "Forget saved unlock on this device"
  reset_vault_button:
    zh-CN: "重置密码库"
    en: "Reset vault"
  reset_confirm_title:
    zh-CN: "重置密码库？"
    en: "Reset the vault?"
  reset_confirm_body:
    zh-CN: "这将清除所有已保存的密码和密钥。连接列表本身会保留。此操作不可撤销。"
    en: "This clears every saved password and key. The connection list itself is kept. This cannot be undone."
  reset_confirm_title_2:
    zh-CN: "确定要继续吗？"
    en: "Are you sure?"
  reset_confirm_body_2:
    zh-CN: "再次确认：所有已保存的密码和密钥都将被永久删除。"
    en: "Confirming again: every saved password and key will be permanently deleted."
  reset_done:
    zh-CN: "密码库已重置，请重启应用设置新的主密码"
    en: "Vault reset. Restart the app to set a new master password."
```

- [ ] **Step 5: Build and manually verify**

Run: `cargo build && cargo test`
Expected: green.

Manual: unlock the app (real password), click "Forget saved unlock on this
device" with the OS keyring convenience-unlock previously enabled — restart
and confirm you're prompted for the password again (not silently
unlocked). Separately, click "Reset vault," confirm both confirmation
dialogs, verify the connection list's hosts/names are still visible but
opening one now fails cleanly (no secret), and restarting the app re-shows
the "set a master password" first-run prompt.

- [ ] **Step 6: Commit**

```bash
git add src/settings.rs locales/app.yml
git commit -m "feat: add 'forget saved unlock' and 'reset vault' settings actions"
```

---

### Task 6: Update export/import for the encrypted vault

**Files:**
- Modify: `src/panels/sessions.rs` (`export_connections`, `import_connections`)
- Modify: `locales/app.yml`

**Interfaces:**
- Consumes: `vault::{unlock, import_merge}` (Task 3), `workspace::VaultKey` (Task 4).

- [ ] **Step 1: Simplify `export_connections` to a plain file copy**

The design spec's "Export" behavior is "save a copy of the current,
already-encrypted `connections.toml`" — no re-serialization needed (in
fact, Task 4's Step 3 patch to include `vault`/`ssh_keys` in the exported
`AppConfig` literal was only a stopgap to keep that task compiling; this
step replaces it properly). Replace the whole function (currently lines
850-878):

```rust
    fn export_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let start_dir = config::config_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&start_dir, Some("connections.toml"));
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            let _ = weak.update(cx, |this, _cx| {
                let export = AppConfig {
                    connections: this.connections.clone(),
                    groups: this.groups.clone(),
                    vault: this.vault.clone(),
                    ssh_keys: this.ssh_keys.clone(),
                };
                let text = match toml::to_string_pretty(&export) {
                    Ok(t) => t,
                    Err(e) => {
                        log::error!("failed to serialize exported connections: {e}");
                        return;
                    }
                };
                if let Err(e) = std::fs::write(&path, text) {
                    log::error!("failed to write exported connections to {path:?}: {e}");
                }
            });
        })
        .detach();
    }
```

with:

```rust
    /// Saves a copy of the current, already-encrypted `connections.toml`.
    /// No re-encryption needed — the file is self-contained (it carries its
    /// own `[vault]` section), so the same master password unlocks the copy
    /// on another machine. This first calls `self.persist()` so the export
    /// reflects any in-memory edits not yet flushed to disk.
    fn export_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.persist();
        let start_dir = config::config_path()
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("."));
        let rx = cx.prompt_for_new_path(&start_dir, Some("connections.toml"));
        cx.spawn_in(window, async move |_weak, cx| {
            let Ok(Ok(Some(path))) = rx.await else {
                return;
            };
            if let Err(e) = std::fs::copy(config::config_path(), &path) {
                log::error!("failed to export connections to {path:?}: {e}");
            }
        })
        .detach();
    }
```

- [ ] **Step 2: Rewrite `import_connections` to prompt for the source file's password and merge via `vault::import_merge`**

Replace the whole function (currently lines 885-921):

```rust
    fn import_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |weak, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    log::error!("failed to read {path:?}: {e}");
                    return;
                }
            };
            let imported: AppConfig = match toml::from_str(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    log::error!("failed to parse {path:?} as connections TOML: {e}");
                    return;
                }
            };
            let _ = weak.update(cx, |this, cx| {
                this.connections.extend(imported.connections);
                this.groups.extend(imported.groups);
                this.persist();
                cx.notify();
            });
        })
        .detach();
    }
```

with:

```rust
    /// Imports another vault file: picks it, prompts for *its* master
    /// password (independent of the currently-unlocked vault), decrypts it
    /// standalone, and merges its groups/connections/ssh_keys into the
    /// current vault via `vault::import_merge` — which re-encrypts every
    /// secret under the current master key. Connections/groups are
    /// appended, never overwritten; `ssh_keys` are deduped by content hash.
    fn import_connections(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(gpui::PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: None,
        });
        let weak = cx.entity().downgrade();
        cx.spawn_in(window, async move |_weak, cx| {
            let Ok(Ok(Some(paths))) = rx.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    log::error!("failed to read {path:?}: {e}");
                    return;
                }
            };
            let source: AppConfig = match toml::from_str(&text) {
                Ok(cfg) => cfg,
                Err(e) => {
                    log::error!("failed to parse {path:?} as connections TOML: {e}");
                    return;
                }
            };
            if source.vault.is_none() {
                log::error!("{path:?} has no [vault] section — nothing to import from an unmigrated file");
                return;
            }

            let source_password_input = cx
                .update(|window, cx| {
                    cx.new(|cx| {
                        crate::terminal::ssh::InputState::new(window, cx)
                            .masked(true)
                            .placeholder(rust_i18n::t!("Vault.password_placeholder"))
                    })
                })
                .ok();
            let Some(source_password_input) = source_password_input else { return };

            let (tx, rx2) = futures::channel::oneshot::channel();
            let tx = std::rc::Rc::new(std::cell::RefCell::new(Some(tx)));
            let error: Entity<SharedString> = cx.update(|_window, cx| cx.new(|_| SharedString::default())).unwrap();
            let pw_input_for_dialog = source_password_input.clone();
            let error_for_dialog = error.clone();
            let _ = cx.update(|window, cx| {
                window.open_alert_dialog(cx, move |alert, _window, cx| {
                    let pw_input = pw_input_for_dialog.clone();
                    let error = error_for_dialog.clone();
                    let tx = tx.clone();
                    let body = v_flex()
                        .gap_2()
                        .child(div().child(rust_i18n::t!("Vault.import_password_body")))
                        .child(Input::new(&pw_input))
                        .when(!error.read(cx).is_empty(), |el| {
                            el.child(div().text_color(gpui::red()).child(error.read(cx).clone()))
                        });
                    alert
                        .title(rust_i18n::t!("Vault.import_password_title"))
                        .description(body)
                        .confirm()
                        .on_ok(move |_, window, cx| {
                            let password = pw_input.read(cx).value().to_string();
                            window.close_dialog(cx);
                            if let Some(tx) = tx.borrow_mut().take() {
                                let _ = tx.send(password);
                            }
                            true
                        })
                });
            });
            let Ok(source_password) = rx2.await else { return };

            let source_key = match vault::unlock(&source, &source_password) {
                Ok(key) => key,
                Err(_) => {
                    log::error!("wrong password for imported file {path:?}");
                    return;
                }
            };

            let _ = weak.update(cx, |this, cx| {
                let Some(dest_key) = cx.try_global::<crate::workspace::VaultKey>() else {
                    log::error!("cannot import: current vault is locked");
                    return;
                };
                let mut dest = AppConfig {
                    connections: this.connections.clone(),
                    groups: this.groups.clone(),
                    vault: this.vault.clone(),
                    ssh_keys: this.ssh_keys.clone(),
                };
                if let Err(e) = vault::import_merge(&mut dest, &dest_key.0, &source, &source_key) {
                    log::error!("failed to merge imported connections: {e}");
                    return;
                }
                this.connections = dest.connections;
                this.groups = dest.groups;
                this.ssh_keys = dest.ssh_keys;
                this.persist();
                cx.notify();
            });
        })
        .detach();
    }
```

(The `cx.update(...)` calls constructing `source_password_input`/`error`
outside a window-bound closure need a `Window` in scope — since this async
block runs via `cx.spawn_in(window, ...)`, `cx` here is an
`AsyncWindowContext`, whose `.update(...)` closure signature is `|window,
cx|`; adjust the two `cx.update(|_window, cx| ...)`/`cx.update(|window,
cx| ...)` calls above to match whatever exact closure arity
`AsyncWindowContext::update` expects in the pinned `gpui` revision — check
`cx.spawn_in`'s existing usage elsewhere in `sessions.rs`/`workspace.rs`
for the precise pattern and mirror it exactly, since this plan's draft
above is written from the design intent rather than a verified compile.)

- [ ] **Step 3: Add i18n strings**

```yaml
Vault:
  import_password_title:
    zh-CN: "输入导入文件的主密码"
    en: "Enter the imported file's master password"
  import_password_body:
    zh-CN: "该文件使用自己的主密码加密，与当前密码库无关。"
    en: "This file is encrypted with its own master password, independent of the current vault."
```

- [ ] **Step 4: Build, test, manually verify**

Run: `cargo build && cargo test`
Expected: green (fix any closure-arity mismatches flagged in Step 2's
parenthetical — this is the one place in the plan where the exact gpui API
shape needs a live compiler check rather than research alone, since async
dialog-plus-file-picker composition wasn't found as an existing pattern
anywhere else in the codebase to copy verbatim).

Manual: export the current vault to a file, note its master password.
Create a second, throwaway `connections.toml` (or use a temp `HOME`) with
one connection, migrate it with a *different* password, then use "Import
connections..." in the main app, pick that file, enter its password —
confirm the connection appears in the list and opens successfully.
Re-import the same file — confirm no duplicate `ssh_keys` entry is created
if that second vault also used a key-file connection with the same key
bytes.

- [ ] **Step 5: Commit**

```bash
git add src/panels/sessions.rs locales/app.yml
git commit -m "feat: make export/import work with the encrypted vault (self-contained export, password-prompted merge import)"
```

---

## Self-Review Notes

- **Spec coverage**: every section of the design spec has a task —
  data model/crypto → Tasks 1, 3; unlock flow/migration → Task 4;
  export/import → Task 6; forgotten-password reset + convenience unlock →
  Tasks 2, 5; error handling (dangling key reference, corrupted field,
  keyring-unavailable fallback) → covered inline in Tasks 3's
  `to_ssh_config` error test and Task 2's "always treat as cache miss"
  doc comment.
- **Known soft spots flagged inline, not hidden**: Task 4 Step 5's dialog/
  async-bridging code and Task 6 Step 2's closure-arity note are marked as
  needing a live compiler check against the pinned `gpui` revision — this
  plan's research didn't find an existing example of an `async fn` awaiting
  an `open_alert_dialog` result anywhere in the current codebase (existing
  dialogs are all fire-and-forget from a synchronous click handler), so
  that composition is this plan's own design, not a verified-in-place
  pattern. Treat those two spots as the first place to look if `cargo
  build` disagrees with this plan's code.
- **Type consistency**: `SavedConnection.{encrypted_password,
  encrypted_key_passphrase, private_key_id}`, `SshKeyEntry.{id, name,
  source_path, encrypted_content}`, `VaultMeta`'s field names, and
  `SshAuth::PrivateKeyContent{content, passphrase}` are used identically
  across Tasks 3, 4, 5, and 6 — cross-checked while writing.
