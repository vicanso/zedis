// Copyright 2025 Tree xie.
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

use crate::error::Error;
use aes_gcm::{
    Aes256Gcm,
    aead::{Aead, AeadCore, KeyInit, Nonce, OsRng},
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

type Result<T, E = Error> = std::result::Result<T, E>;

const MASTER_KEY: &[u8; 32] = b"9dFVxjgeQTPfOXCoDdjpgMOlPhy2HE9E";
pub fn fast_contains_ignore_case(haystack: &str, needle_lower: &str) -> bool {
    // 1. 长度剪枝
    if needle_lower.len() > haystack.len() {
        return false;
    }

    if haystack.is_ascii() {
        let needle_bytes = needle_lower.as_bytes();
        return haystack
            .as_bytes()
            .windows(needle_bytes.len())
            .any(|window| window.eq_ignore_ascii_case(needle_bytes));
    }

    haystack.to_lowercase().contains(needle_lower)
}

pub fn encrypt(plain_text: &str) -> Result<String> {
    let cipher = Aes256Gcm::new(MASTER_KEY.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let ciphertext = cipher
        .encrypt(&nonce, plain_text.as_bytes())
        .map_err(|e| Error::Invalid { message: e.to_string() })?;

    let mut combined = nonce.to_vec();
    combined.extend_from_slice(&ciphertext);

    Ok(BASE64.encode(combined))
}

pub fn decrypt(cipher_text: &str) -> Result<String> {
    let data = BASE64
        .decode(cipher_text)
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    let cipher = Aes256Gcm::new(MASTER_KEY.into());
    let nonce_bytes = &data[0..12];
    let nonce = Nonce::<Aes256Gcm>::from_slice(nonce_bytes);
    let ciphertext = &data[12..];

    let ciphertext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| Error::Invalid { message: e.to_string() })?;
    String::from_utf8(ciphertext).map_err(|e| Error::Invalid { message: e.to_string() })
}
