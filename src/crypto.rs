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
