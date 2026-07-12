// Copyright 2026 Tree xie.
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
// http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

//! String manipulation and cryptography utilities.
//!
//! This module provides utility functions for:
//! - Fast case-insensitive substring searching with ASCII optimization
//! - AES-256-GCM encryption and decryption for sensitive data (e.g., passwords)
//! - Base64 encoding/decoding for storage and transport

use crate::error::Error;
use aes_gcm::{
    Aes256Gcm, Nonce,
    aead::{Aead, Generate, KeyInit, array::Array, consts::U16},
};
use argon2::Argon2;
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use redis::Value;

// Pure string helpers live in zedis-core; re-exported so this module's
// public surface (and the app's `crate::helpers::*` glob) is unchanged.
pub use zedis_core::string::{
    fast_contains_ignore_case, format_duration, group_thousands, starts_with_ignore_ascii_case,
};

type Result<T, E = Error> = std::result::Result<T, E>;

/// Master encryption key for AES-256-GCM cipher.
///
/// WARNING: In production, this should be stored securely (e.g., keychain, env var)
/// rather than hardcoded in the binary.
const MASTER_KEY: &[u8; 32] = b"9dFVxjgeQTPfOXCoDdjpgMOlPhy2HE9E";

/// Encrypts a plaintext string using AES-256-GCM encryption.
///
/// The encrypted data is encoded as Base64 for easy storage and transport.
/// Each encryption uses a randomly generated nonce for security.
///
/// # Algorithm Details
/// - **Cipher**: AES-256-GCM (Galois/Counter Mode)
/// - **Key size**: 256 bits (32 bytes)
/// - **Nonce**: 96 bits (12 bytes), randomly generated per encryption
/// - **Authentication**: Built-in authenticated encryption (AEAD)
///
/// # Storage Format
/// The output Base64 string contains: `[nonce (12 bytes)][ciphertext (variable)]`
///
/// # Arguments
/// * `plain_text` - The plaintext string to encrypt
///
/// # Returns
/// A Base64-encoded string containing the nonce and ciphertext
///
/// # Errors
/// Returns an error if encryption fails
///
/// # Security Notes
/// - Each call generates a unique nonce for security
/// - The nonce is prepended to the ciphertext for decryption
/// - GCM mode provides both confidentiality and authenticity
pub fn encrypt(plain_text: &str) -> Result<String> {
    // Initialize AES-256-GCM cipher with master key
    let cipher = Aes256Gcm::new(MASTER_KEY.into());

    // Generate a random 96-bit nonce (number used once) via the ambient
    // system RNG (aes-gcm 0.11 `Generate`); type inferred from the encrypt call.
    let nonce = Nonce::generate();

    // Encrypt the plaintext
    let ciphertext = cipher
        .encrypt(&nonce, plain_text.as_bytes())
        .map_err(|e| Error::Invalid { message: e.to_string() })?;

    // Combine nonce and ciphertext for storage
    let mut combined = nonce.to_vec();
    combined.extend_from_slice(&ciphertext);

    // Encode as Base64 for safe storage/transport
    Ok(BASE64.encode(combined))
}

/// Decrypts a Base64-encoded ciphertext encrypted with AES-256-GCM.
///
/// Expects the input to be in the format produced by `encrypt()`:
/// `[nonce (12 bytes)][ciphertext (variable)]` encoded as Base64.
///
/// # Arguments
/// * `cipher_text` - Base64-encoded string containing nonce and ciphertext
///
/// # Returns
/// The decrypted plaintext string
///
/// # Errors
/// Returns an error if:
/// - Base64 decoding fails
/// - Data format is invalid (too short, missing nonce)
/// - Decryption fails (wrong key, tampered data, authentication failure)
/// - Decrypted data is not valid UTF-8
///
/// # Security Notes
/// - GCM mode automatically verifies data authenticity
/// - Returns error if ciphertext has been tampered with
/// - Nonce is extracted from the first 12 bytes of decoded data
pub fn decrypt(cipher_text: &str) -> Result<String> {
    // Decode from Base64
    let data = BASE64
        .decode(cipher_text)
        .map_err(|e| Error::Invalid { message: e.to_string() })?;

    // Initialize cipher with master key
    let cipher = Aes256Gcm::new(MASTER_KEY.into());

    // Extract nonce from first 12 bytes
    let nonce_bytes = &data[0..12];
    let nonce = Nonce::try_from(nonce_bytes).map_err(|_| Error::Invalid {
        message: "invalid nonce length".to_string(),
    })?;

    // Extract ciphertext from remaining bytes
    let ciphertext = &data[12..];

    // Decrypt and verify authenticity
    let plaintext_bytes = cipher
        .decrypt(&nonce, ciphertext)
        .map_err(|e| Error::Invalid { message: e.to_string() })?;

    // Convert decrypted bytes to UTF-8 string
    String::from_utf8(plaintext_bytes).map_err(|e| Error::Invalid { message: e.to_string() })
}

pub fn redis_value_to_string(v: &Value) -> String {
    match v {
        Value::Nil => "(nil)".to_string(),
        Value::Int(i) => i.to_string(),
        Value::SimpleString(s) => s.clone(),
        Value::Okay => "OK".to_string(),
        Value::Double(f) => f.to_string(),
        Value::Boolean(b) => b.to_string(),

        Value::BulkString(bytes) => String::from_utf8_lossy(bytes).to_string(),

        Value::Array(items) => {
            let elements: Vec<String> = items.iter().map(redis_value_to_string).collect();
            format!("[{}]", elements.join(", "))
        }
        Value::Set(items) => {
            let elements: Vec<String> = items.iter().map(redis_value_to_string).collect();
            format!("Set({})", elements.join(", "))
        }
        Value::Map(items) => {
            let elements: Vec<String> = items
                .iter()
                .map(|(k, v)| format!("{}: {}", redis_value_to_string(k), redis_value_to_string(v)))
                .collect();
            format!("{{{}}}", elements.join(", "))
        }

        Value::VerbatimString { text, .. } => text.clone(),

        Value::Attribute { data, .. } => redis_value_to_string(data),

        Value::BigNumber(n) => format!("{:?}", n),

        Value::ServerError(e) => format!("(error) {}", e),

        Value::Push { kind, data } => {
            let elements: Vec<String> = data.iter().map(redis_value_to_string).collect();
            format!("Push({:?}): [{}]", kind, elements.join(", "))
        }
        _ => "Unsupported".to_string(),
    }
}

/// Version prefix marking a passphrase-protected share token produced by
/// [`encrypt_share`]. Lets the import dialog tell an encrypted blob apart
/// from plain JSON and reveal its passphrase input.
const SHARE_TOKEN_PREFIX: &str = "ZEDIS1.";
/// Argon2id salt length prepended to the share payload.
const SHARE_SALT_LEN: usize = 16;
/// AES-GCM nonce length following the salt.
const SHARE_NONCE_LEN: usize = 12;

/// Whether `s` looks like an encrypted share token (vs plain export JSON).
pub fn is_share_token(s: &str) -> bool {
    s.trim_start().starts_with(SHARE_TOKEN_PREFIX)
}

/// Argon2id (default params) passphrase → 256-bit AES key.
fn derive_share_key(passphrase: &str, salt: &[u8]) -> Result<[u8; 32]> {
    let mut key = [0u8; 32];
    Argon2::default()
        .hash_password_into(passphrase.as_bytes(), salt, &mut key)
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    Ok(key)
}

/// Encrypts an export payload into a portable share token:
/// `ZEDIS1.` + base64(salt ‖ nonce ‖ ciphertext), key = Argon2id(passphrase).
/// Unlike [`encrypt`] (machine-local master key), the result opens on any
/// machine that knows the passphrase — built for the share/export flow.
pub fn encrypt_share(plain_text: &str, passphrase: &str) -> Result<String> {
    // Argon2 salt + AES-GCM nonce, both from the ambient system RNG.
    let salt = Array::<u8, U16>::generate();
    let key = derive_share_key(passphrase, salt.as_slice())?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| Error::Invalid { message: e.to_string() })?;
    let nonce = Nonce::generate();
    let ciphertext = cipher
        .encrypt(&nonce, plain_text.as_bytes())
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    let mut combined = Vec::with_capacity(SHARE_SALT_LEN + SHARE_NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(salt.as_slice());
    combined.extend_from_slice(nonce.as_slice());
    combined.extend_from_slice(&ciphertext);
    Ok(format!("{SHARE_TOKEN_PREFIX}{}", BASE64.encode(combined)))
}

/// Decrypts a token produced by [`encrypt_share`]. Fails on a wrong
/// passphrase or tampered data (GCM authentication).
pub fn decrypt_share(token: &str, passphrase: &str) -> Result<String> {
    let body = token
        .trim()
        .strip_prefix(SHARE_TOKEN_PREFIX)
        .ok_or_else(|| Error::Invalid {
            message: "not a share token".to_string(),
        })?;
    let data = BASE64
        .decode(body)
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    if data.len() <= SHARE_SALT_LEN + SHARE_NONCE_LEN {
        return Err(Error::Invalid {
            message: "share token too short".to_string(),
        });
    }
    let (salt, rest) = data.split_at(SHARE_SALT_LEN);
    let (nonce_bytes, ciphertext) = rest.split_at(SHARE_NONCE_LEN);
    let key = derive_share_key(passphrase, salt)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| Error::Invalid { message: e.to_string() })?;
    let nonce = Nonce::try_from(nonce_bytes).map_err(|_| Error::Invalid {
        message: "wrong passphrase or corrupted share token".to_string(),
    })?;
    let plain = cipher.decrypt(&nonce, ciphertext).map_err(|_| Error::Invalid {
        message: "wrong passphrase or corrupted share token".to_string(),
    })?;
    String::from_utf8(plain).map_err(|e| Error::Invalid { message: e.to_string() })
}

#[cfg(test)]
mod tests {
    use super::{decrypt_share, encrypt_share, is_share_token};

    #[test]
    fn share_token_round_trip() {
        let json = r#"[{"name":"prod"}]"#;
        let token = encrypt_share(json, "s3cret").expect("encrypt");
        assert!(is_share_token(&token));
        assert!(!is_share_token(json));
        assert_eq!(decrypt_share(&token, "s3cret").expect("decrypt"), json);
        assert!(decrypt_share(&token, "wrong").is_err(), "wrong passphrase must fail");
    }
}
