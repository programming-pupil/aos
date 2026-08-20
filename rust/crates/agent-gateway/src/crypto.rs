//! AES-256-GCM encryption utilities for storing reversible encrypted data.
//!
//! Uses an active 32-byte key from `ENCRYPTION_KEY` and optional retired keys
//! from `ENCRYPTION_KEY_RING`. New ciphertexts carry a format version and key
//! id; legacy base64 payloads remain readable during online rotation.

use aes_gcm::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng, Payload},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use thiserror::Error;

const NONCE_SIZE: usize = 12;
const ENVELOPE_PREFIX: &str = "aosenc:v1:";
const SCOPED_ENVELOPE_PREFIX: &str = "aosenc:v2:";
const DEFAULT_KEY_ID: &str = "primary";

#[derive(Debug, Error)]
pub enum CryptoError {
    #[error("encryption key must be 32 bytes, got {0}")]
    InvalidKeyLength(usize),
    #[error("encryption failed: {0}")]
    EncryptionFailed(String),
    #[error("decryption failed: {0}")]
    DecryptionFailed(String),
    #[error("invalid base64 encoding: {0}")]
    InvalidBase64(String),
    #[error("encryption key id is invalid: {0}")]
    InvalidKeyId(String),
    #[error("ciphertext references unknown encryption key id: {0}")]
    UnknownKeyId(String),
    #[error("encryption key ring is invalid: {0}")]
    InvalidKeyRing(String),
}

/// Build the canonical associated-data identity for a durable ciphertext row.
/// Callers must use the registry store id, not a physical table/column name, so
/// schema refactors cannot silently weaken or fork the cryptographic boundary.
#[must_use]
pub fn scoped_aad(store_id: &str, tenant_id: &str, row_id: &str) -> String {
    format!("aos.semantic-kernel/v1/{store_id}/{tenant_id}/{row_id}")
}

#[derive(Debug, Clone)]
struct KeyRing {
    active_id: String,
    keys: Vec<(String, [u8; 32])>,
}

impl KeyRing {
    fn load() -> Result<Self, CryptoError> {
        let active_id =
            std::env::var("ENCRYPTION_KEY_ID").unwrap_or_else(|_| DEFAULT_KEY_ID.to_string());
        validate_key_id(&active_id)?;
        let active = get_encryption_key()?;
        let mut keys = vec![(active_id.clone(), active)];
        if let Ok(raw) = std::env::var("ENCRYPTION_KEY_RING") {
            for (id, key) in parse_key_ring(&raw)? {
                validate_key_id(&id)?;
                let key = parse_key(&key)?;
                if let Some(existing) = keys.iter_mut().find(|(known, _)| known == &id) {
                    existing.1 = key;
                } else {
                    keys.push((id, key));
                }
            }
        }
        Ok(Self { active_id, keys })
    }

    fn active(&self) -> (&str, &[u8; 32]) {
        let (_, key) = self
            .keys
            .iter()
            .find(|(id, _)| id == &self.active_id)
            .expect("active encryption key is inserted before retired keys");
        (&self.active_id, key)
    }

    fn by_id(&self, id: &str) -> Option<&[u8; 32]> {
        self.keys
            .iter()
            .find_map(|(known, key)| (known == id).then_some(key))
    }
}

fn validate_key_id(id: &str) -> Result<(), CryptoError> {
    if id.is_empty()
        || id.len() > 64
        || !id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(CryptoError::InvalidKeyId(id.to_string()));
    }
    Ok(())
}

fn parse_key(value: &str) -> Result<[u8; 32], CryptoError> {
    let bytes = value.as_bytes();
    if bytes.len() != 32 {
        return Err(CryptoError::InvalidKeyLength(bytes.len()));
    }
    let mut key = [0_u8; 32];
    key.copy_from_slice(bytes);
    Ok(key)
}

fn parse_key_ring(raw: &str) -> Result<Vec<(String, String)>, CryptoError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(Vec::new());
    }
    if trimmed.starts_with('{') {
        let values = serde_json::from_str::<std::collections::BTreeMap<String, String>>(trimmed)
            .map_err(|error| CryptoError::InvalidKeyRing(error.to_string()))?;
        return Ok(values.into_iter().collect());
    }
    trimmed
        .split(',')
        .filter(|entry| !entry.trim().is_empty())
        .map(|entry| {
            let (id, key) = entry.split_once('=').ok_or_else(|| {
                CryptoError::InvalidKeyRing(
                    "expected comma-separated key_id=32-byte-key entries".into(),
                )
            })?;
            Ok((id.trim().to_string(), key.trim().to_string()))
        })
        .collect()
}

/// Get the 32-byte encryption key from environment.
pub fn get_encryption_key() -> Result<[u8; 32], CryptoError> {
    let key_str = std::env::var("ENCRYPTION_KEY")
        .or_else(|_| {
            #[cfg(debug_assertions)]
            {
                static WARN_ONCE: std::sync::Once = std::sync::Once::new();
                WARN_ONCE.call_once(|| {
                    tracing::warn!(
                        "ENCRYPTION_KEY is missing; using development fallback key. \
                     Set ENCRYPTION_KEY explicitly for production or shared deployments."
                    );
                });
                Ok::<String, std::env::VarError>("12345678901234567890123456789012".to_string())
            }
            #[cfg(not(debug_assertions))]
            {
                Err(std::env::VarError::NotPresent)
            }
        })
        .map_err(|_| CryptoError::InvalidKeyLength(0))?;

    parse_key(&key_str)
}

/// Encrypt plaintext using AES-256-GCM.
/// Returns base64(nonce || `ciphertext_with_tag`).
pub fn encrypt(plaintext: &str) -> Result<String, CryptoError> {
    let ring = KeyRing::load()?;
    let (key_id, key) = ring.active();
    let encoded = encrypt_payload(plaintext, key)?;
    Ok(format!("{ENVELOPE_PREFIX}{key_id}:{encoded}"))
}

pub fn encrypt_scoped(plaintext: &str, aad: &str) -> Result<String, CryptoError> {
    let ring = KeyRing::load()?;
    let (key_id, key) = ring.active();
    let aad_hash = aad_digest(aad);
    let encoded = encrypt_payload_with_aad(plaintext, key, aad_hash.as_bytes())?;
    Ok(format!(
        "{SCOPED_ENVELOPE_PREFIX}{key_id}:{aad_hash}:{encoded}"
    ))
}

fn encrypt_payload(plaintext: &str, key: &[u8; 32]) -> Result<String, CryptoError> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;

    let mut nonce_bytes = [0u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);

    let ciphertext = cipher
        .encrypt(nonce, plaintext.as_bytes())
        .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;

    let mut combined = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);

    Ok(BASE64.encode(&combined))
}

fn encrypt_payload_with_aad(
    plaintext: &str,
    key: &[u8; 32],
    aad: &[u8],
) -> Result<String, CryptoError> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;
    let mut nonce_bytes = [0_u8; NONCE_SIZE];
    OsRng.fill_bytes(&mut nonce_bytes);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce_bytes),
            Payload {
                msg: plaintext.as_bytes(),
                aad,
            },
        )
        .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;
    let mut combined = Vec::with_capacity(NONCE_SIZE + ciphertext.len());
    combined.extend_from_slice(&nonce_bytes);
    combined.extend_from_slice(&ciphertext);
    Ok(BASE64.encode(combined))
}

/// Decrypt ciphertext using AES-256-GCM.
/// Input: base64(nonce || `ciphertext_with_tag`).
pub fn decrypt(encrypted: &str) -> Result<String, CryptoError> {
    let ring = KeyRing::load()?;
    if encrypted.starts_with(SCOPED_ENVELOPE_PREFIX) {
        return Err(CryptoError::DecryptionFailed(
            "scoped ciphertext requires decrypt_scoped with its tenant/store/row identity".into(),
        ));
    }
    if let Some(rest) = encrypted.strip_prefix(ENVELOPE_PREFIX) {
        let (key_id, payload) = rest.split_once(':').ok_or_else(|| {
            CryptoError::DecryptionFailed("versioned ciphertext is missing its key id".into())
        })?;
        let key = ring
            .by_id(key_id)
            .ok_or_else(|| CryptoError::UnknownKeyId(key_id.to_string()))?;
        return decrypt_payload(payload, key);
    }
    let mut last_error = None;
    for (_, key) in &ring.keys {
        match decrypt_payload(encrypted, key) {
            Ok(plaintext) => return Ok(plaintext),
            Err(error) => last_error = Some(error),
        }
    }
    Err(last_error.unwrap_or_else(|| {
        CryptoError::DecryptionFailed("no configured key could decrypt legacy ciphertext".into())
    }))
}

pub fn decrypt_scoped(encrypted: &str, aad: &str) -> Result<String, CryptoError> {
    let Some(rest) = encrypted.strip_prefix(SCOPED_ENVELOPE_PREFIX) else {
        return decrypt(encrypted);
    };
    let mut fields = rest.splitn(3, ':');
    let key_id = fields.next().unwrap_or_default();
    let stored_aad_hash = fields.next().ok_or_else(|| {
        CryptoError::DecryptionFailed("scoped ciphertext is missing its AAD hash".into())
    })?;
    let expected_aad_hash = aad_digest(aad);
    if stored_aad_hash != expected_aad_hash {
        return Err(CryptoError::DecryptionFailed(
            "ciphertext AAD does not match its tenant/store/row identity".into(),
        ));
    }
    let payload = fields.next().ok_or_else(|| {
        CryptoError::DecryptionFailed("scoped ciphertext is missing its payload".into())
    })?;
    let ring = KeyRing::load()?;
    let key = ring
        .by_id(key_id)
        .ok_or_else(|| CryptoError::UnknownKeyId(key_id.to_string()))?;
    decrypt_payload_with_aad(payload, key, stored_aad_hash.as_bytes())
}

fn decrypt_payload(encrypted: &str, key: &[u8; 32]) -> Result<String, CryptoError> {
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;

    let combined = BASE64
        .decode(encrypted)
        .map_err(|e| CryptoError::InvalidBase64(e.to_string()))?;

    if combined.len() < NONCE_SIZE + 16 {
        return Err(CryptoError::DecryptionFailed("data too short".to_string()));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_SIZE);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;

    String::from_utf8(plaintext).map_err(|e| CryptoError::DecryptionFailed(e.to_string()))
}

fn decrypt_payload_with_aad(
    encrypted: &str,
    key: &[u8; 32],
    aad: &[u8],
) -> Result<String, CryptoError> {
    let combined = BASE64
        .decode(encrypted)
        .map_err(|e| CryptoError::InvalidBase64(e.to_string()))?;
    if combined.len() < NONCE_SIZE + 16 {
        return Err(CryptoError::DecryptionFailed("data too short".into()));
    }
    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_SIZE);
    let cipher =
        Aes256Gcm::new_from_slice(key).map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce_bytes),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;
    String::from_utf8(plaintext).map_err(|e| CryptoError::DecryptionFailed(e.to_string()))
}

fn aad_digest(aad: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(aad.as_bytes()))
}

/// Return the key id embedded in a versioned ciphertext. Legacy ciphertexts
/// intentionally return `None` so callers can schedule online re-encryption.
pub fn ciphertext_key_id(encrypted: &str) -> Option<&str> {
    encrypted
        .strip_prefix(SCOPED_ENVELOPE_PREFIX)
        .or_else(|| encrypted.strip_prefix(ENVELOPE_PREFIX))
        .and_then(|rest| rest.split_once(':').map(|(id, _)| id))
}

pub fn active_key_id() -> Result<String, CryptoError> {
    Ok(KeyRing::load()?.active_id)
}

pub fn configured_key_ids() -> Result<Vec<String>, CryptoError> {
    let ring = KeyRing::load()?;
    let mut ids = ring
        .keys
        .iter()
        .map(|(id, _)| id.clone())
        .collect::<Vec<_>>();
    ids.sort();
    Ok(ids)
}

pub fn needs_reencryption(encrypted: &str) -> Result<bool, CryptoError> {
    let ring = KeyRing::load()?;
    Ok(ciphertext_key_id(encrypted) != Some(ring.active_id.as_str()))
}

pub fn reencrypt(encrypted: &str) -> Result<String, CryptoError> {
    if !needs_reencryption(encrypted)? {
        return Ok(encrypted.to_string());
    }
    encrypt(&decrypt(encrypted)?)
}

pub fn reencrypt_scoped(encrypted: &str, aad: &str) -> Result<String, CryptoError> {
    let plaintext = if encrypted.starts_with(SCOPED_ENVELOPE_PREFIX) {
        decrypt_scoped(encrypted, aad)?
    } else {
        decrypt(encrypted)?
    };
    let active_key_id = active_key_id()?;
    if encrypted.starts_with(&format!("{SCOPED_ENVELOPE_PREFIX}{active_key_id}:")) {
        return Ok(encrypted.to_string());
    }
    encrypt_scoped(&plaintext, aad)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("ENCRYPTION_KEY", "12345678901234567890123456789012");
        std::env::set_var("ENCRYPTION_KEY_ID", "primary");
        std::env::remove_var("ENCRYPTION_KEY_RING");

        let original = "sk-ant-api03-test-key-12345";
        let encrypted = encrypt(original).unwrap();
        assert_eq!(ciphertext_key_id(&encrypted), Some("primary"));
        let decrypted = decrypt(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }

    #[test]
    fn retired_key_can_read_and_rotate_versioned_ciphertext() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("ENCRYPTION_KEY", "11111111111111111111111111111111");
        std::env::set_var("ENCRYPTION_KEY_ID", "old");
        std::env::remove_var("ENCRYPTION_KEY_RING");
        let old = encrypt("durable payload").unwrap();

        std::env::set_var("ENCRYPTION_KEY", "22222222222222222222222222222222");
        std::env::set_var("ENCRYPTION_KEY_ID", "new");
        std::env::set_var(
            "ENCRYPTION_KEY_RING",
            r#"{"old":"11111111111111111111111111111111"}"#,
        );
        assert_eq!(decrypt(&old).unwrap(), "durable payload");
        assert!(needs_reencryption(&old).unwrap());
        let rotated = reencrypt(&old).unwrap();
        assert_eq!(ciphertext_key_id(&rotated), Some("new"));
        assert_eq!(decrypt(&rotated).unwrap(), "durable payload");
    }

    #[test]
    fn scoped_ciphertext_rejects_cross_row_movement_and_rotates() {
        let _guard = ENV_LOCK.lock().unwrap();
        std::env::set_var("ENCRYPTION_KEY", "11111111111111111111111111111111");
        std::env::set_var("ENCRYPTION_KEY_ID", "old");
        std::env::remove_var("ENCRYPTION_KEY_RING");
        let old = encrypt_scoped("tenant secret", "tenant-a/api_keys/key-1").unwrap();
        assert_eq!(
            decrypt_scoped(&old, "tenant-a/api_keys/key-1").unwrap(),
            "tenant secret"
        );
        assert!(decrypt(&old).is_err());
        assert!(decrypt_scoped(&old, "tenant-a/api_keys/key-2").is_err());

        std::env::set_var("ENCRYPTION_KEY", "22222222222222222222222222222222");
        std::env::set_var("ENCRYPTION_KEY_ID", "new");
        std::env::set_var(
            "ENCRYPTION_KEY_RING",
            r#"{"old":"11111111111111111111111111111111"}"#,
        );
        let rotated = reencrypt_scoped(&old, "tenant-a/api_keys/key-1").unwrap();
        assert!(rotated.starts_with("aosenc:v2:new:"));
        assert_eq!(
            decrypt_scoped(&rotated, "tenant-a/api_keys/key-1").unwrap(),
            "tenant secret"
        );
    }
}
