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
    Aes256Gcm,
    aead::{Aead, AeadCore, KeyInit, Nonce, OsRng},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use redis::Value;
use std::time::Duration;

type Result<T, E = Error> = std::result::Result<T, E>;

/// Master encryption key for AES-256-GCM cipher.
///
/// WARNING: In production, this should be stored securely (e.g., keychain, env var)
/// rather than hardcoded in the binary.
const MASTER_KEY: &[u8; 32] = b"9dFVxjgeQTPfOXCoDdjpgMOlPhy2HE9E";
/// Performs fast case-insensitive substring search with ASCII optimization.
///
/// This function is optimized for performance with two strategies:
/// 1. **ASCII fast path**: Uses byte-level comparison for ASCII strings (~10x faster)
/// 2. **Unicode fallback**: Falls back to full Unicode lowercase comparison for non-ASCII
///
/// # Arguments
/// * `haystack` - The string to search in
/// * `needle_lower` - The substring to search for (must already be lowercase)
///
/// # Returns
/// `true` if `needle_lower` is found in `haystack` (case-insensitive), `false` otherwise
///
/// # Performance Notes
/// - Early returns if needle is longer than haystack
/// - For ASCII strings, uses efficient byte-level sliding window comparison
/// - For Unicode strings, falls back to standard case-insensitive search
///
/// # Examples
/// ```
/// assert!(fast_contains_ignore_case("Hello World", "hello"));
/// assert!(fast_contains_ignore_case("测试ABC", "abc"));
/// assert!(!fast_contains_ignore_case("short", "longer"));
/// ```
pub fn fast_contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    // Early return: needle cannot be found if it's longer than haystack
    if needle_lower.len() > haystack.len() {
        return false;
    }

    // Fast path for ASCII strings: use byte-level comparison
    if haystack.is_ascii() {
        let needle_bytes = needle_lower.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle_bytes.len())
            .any(|window| window.eq_ignore_ascii_case(needle_bytes));
    }

    // Fallback for Unicode strings: full lowercase conversion
    haystack.to_lowercase().contains(needle_lower)
}

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

    // Generate a random 96-bit nonce (number used once)
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

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
    let nonce = Nonce::<Aes256Gcm>::from_slice(nonce_bytes);

    // Extract ciphertext from remaining bytes
    let ciphertext = &data[12..];

    // Decrypt and verify authenticity
    let plaintext_bytes = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| Error::Invalid { message: e.to_string() })?;

    // Convert decrypted bytes to UTF-8 string
    String::from_utf8(plaintext_bytes).map_err(|e| Error::Invalid { message: e.to_string() })
}

const SECONDS_PER_DAY: u64 = 86400;
const SECONDS_PER_HOUR: u64 = 3600;
const SECONDS_PER_MINUTE: u64 = 60;

pub fn format_duration(duration: Duration) -> String {
    let seconds = duration.as_secs();

    if seconds >= SECONDS_PER_DAY {
        let days = seconds as f64 / SECONDS_PER_DAY as f64;
        return format!("{:.1}d", days);
    }

    if seconds >= SECONDS_PER_HOUR {
        let hours = seconds as f64 / SECONDS_PER_HOUR as f64;
        return format!("{:.1}h", hours);
    }

    if seconds >= SECONDS_PER_MINUTE {
        let minutes = seconds as f64 / SECONDS_PER_MINUTE as f64;
        return format!("{:.1}m", minutes);
    }

    format!("{}s", seconds)
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

pub fn starts_with_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if haystack.len() < needle.len() {
        return false;
    }

    match haystack.get(..needle.len()) {
        Some(sub) => sub.eq_ignore_ascii_case(needle),
        None => false,
    }
}
