//! AES-256-GCM encryption utilities for storing reversible encrypted data.
//!
//! Uses a 32-byte key from `ENCRYPTION_KEY` environment variable.
//! Output format: base64(nonce || ciphertext || tag)

use aes_gcm::{
    aead::{rand_core::RngCore, Aead, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use thiserror::Error;

const NONCE_SIZE: usize = 12;

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

    let key_bytes = key_str.as_bytes();
    if key_bytes.len() != 32 {
        return Err(CryptoError::InvalidKeyLength(key_bytes.len()));
    }

    let mut key = [0u8; 32];
    key.copy_from_slice(key_bytes);
    Ok(key)
}

/// Encrypt plaintext using AES-256-GCM.
/// Returns base64(nonce || `ciphertext_with_tag`).
pub fn encrypt(plaintext: &str) -> Result<String, CryptoError> {
    let key = get_encryption_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| CryptoError::EncryptionFailed(e.to_string()))?;

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

/// Decrypt ciphertext using AES-256-GCM.
/// Input: base64(nonce || `ciphertext_with_tag`).
pub fn decrypt(encrypted: &str) -> Result<String, CryptoError> {
    let key = get_encryption_key()?;
    let cipher = Aes256Gcm::new_from_slice(&key)
        .map_err(|e| CryptoError::DecryptionFailed(e.to_string()))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        std::env::set_var("ENCRYPTION_KEY", "12345678901234567890123456789012");

        let original = "sk-ant-api03-test-key-12345";
        let encrypted = encrypt(original).unwrap();
        let decrypted = decrypt(&encrypted).unwrap();

        assert_eq!(original, decrypted);
    }
}
